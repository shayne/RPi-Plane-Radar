use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Receiver;
use std::sync::{Arc, Barrier, Mutex, Once};
use std::time::Duration;

use log::{Log, Metadata, Record};
use planeradar::adsb::AdsbClient;
use planeradar::flight_data::{
    EnrichmentNeeds, FlightDataClient, FlightDataError, FlightDataService, FlightLookup,
    LookupValue,
};
use planeradar::http::{HttpClient, HttpError, HttpRequest, HttpResponse};
use planeradar::model::{Aircraft, Location, RadarSettings};
use planeradar::runtime::{
    AdsbWorker, ChannelWaiter, FlightDataWorker, RuntimeModel, WaitResult, Waiter, WorkerCommand,
};
use planeradar::time::{Clock, Sleeper};

type RecordedRequests = Arc<Mutex<Vec<(Duration, HttpRequest)>>>;
type RecordedSleeps = Arc<Mutex<Vec<Duration>>>;
type TestClient = FlightDataClient<FakeHttp, FakeClock, FakeSleeper>;

struct RecordingLogger {
    messages: Mutex<Vec<String>>,
}

static RECORDING_LOGGER: RecordingLogger = RecordingLogger {
    messages: Mutex::new(Vec::new()),
};
static INSTALL_RECORDING_LOGGER: Once = Once::new();

impl Log for RecordingLogger {
    fn enabled(&self, _metadata: &Metadata<'_>) -> bool {
        true
    }

    fn log(&self, record: &Record<'_>) {
        if self.enabled(record.metadata()) {
            self.messages
                .lock()
                .expect("log messages")
                .push(record.args().to_string());
        }
    }

    fn flush(&self) {}
}

fn reset_recorded_logs() {
    INSTALL_RECORDING_LOGGER.call_once(|| {
        log::set_logger(&RECORDING_LOGGER).expect("recording logger");
        log::set_max_level(log::LevelFilter::Warn);
    });
    RECORDING_LOGGER
        .messages
        .lock()
        .expect("log messages")
        .clear();
}

#[derive(Clone, Debug, Default)]
struct FakeClock(Arc<Mutex<Duration>>);

impl FakeClock {
    fn advance(&self, duration: Duration) {
        *self.0.lock().expect("clock") += duration;
    }
}

impl Clock for FakeClock {
    fn monotonic(&self) -> Duration {
        *self.0.lock().expect("clock")
    }

    fn unix_seconds(&self) -> u64 {
        0
    }
}

enum WaitOutcome {
    TimedOut,
    Action(Box<dyn FnOnce() + Send>),
    SettingsChanged(RadarSettings),
    SettingsChangedAfter(Duration, RadarSettings),
    ActionSettingsChangedAfter(Duration, Box<dyn FnOnce() + Send>),
    Stop,
}

struct ScriptedWaiter {
    clock: FakeClock,
    waits: Arc<Mutex<Vec<Duration>>>,
    outcomes: Mutex<VecDeque<WaitOutcome>>,
}

impl ScriptedWaiter {
    fn new(
        clock: FakeClock,
        outcomes: impl IntoIterator<Item = WaitOutcome>,
    ) -> (Self, Arc<Mutex<Vec<Duration>>>) {
        let waits = Arc::new(Mutex::new(Vec::new()));
        (
            Self {
                clock,
                waits: waits.clone(),
                outcomes: Mutex::new(outcomes.into_iter().collect()),
            },
            waits,
        )
    }
}

impl Waiter for ScriptedWaiter {
    fn wait(
        &self,
        _commands: &Receiver<WorkerCommand>,
        stop: &AtomicBool,
        duration: Duration,
    ) -> WaitResult {
        self.waits.lock().expect("waits").push(duration);
        match self
            .outcomes
            .lock()
            .expect("wait outcomes")
            .pop_front()
            .expect("scripted wait outcome")
        {
            WaitOutcome::TimedOut => {
                self.clock.advance(duration);
                WaitResult::TimedOut
            }
            WaitOutcome::Action(action) => {
                action();
                self.clock.advance(duration);
                WaitResult::TimedOut
            }
            WaitOutcome::SettingsChanged(settings) => {
                WaitResult::Command(WorkerCommand::SettingsChanged(settings))
            }
            WaitOutcome::SettingsChangedAfter(elapsed, settings) => {
                self.clock.advance(elapsed);
                WaitResult::Command(WorkerCommand::SettingsChanged(settings))
            }
            WaitOutcome::ActionSettingsChangedAfter(elapsed, action) => {
                self.clock.advance(elapsed);
                action();
                WaitResult::Command(WorkerCommand::SettingsChanged(RadarSettings::default()))
            }
            WaitOutcome::Stop => {
                stop.store(true, Ordering::Release);
                WaitResult::Command(WorkerCommand::Stop)
            }
        }
    }
}

#[derive(Clone)]
struct FakeSleeper {
    clock: FakeClock,
    sleeps: Arc<Mutex<Vec<Duration>>>,
}

impl FakeSleeper {
    fn new(clock: FakeClock) -> (Self, Arc<Mutex<Vec<Duration>>>) {
        let sleeps = Arc::new(Mutex::new(Vec::new()));
        (
            Self {
                clock,
                sleeps: sleeps.clone(),
            },
            sleeps,
        )
    }
}

impl Sleeper for FakeSleeper {
    fn sleep(&self, duration: Duration) {
        self.sleeps.lock().expect("sleeps").push(duration);
        self.clock.advance(duration);
    }
}

#[derive(Clone)]
struct FakeHttp {
    clock: FakeClock,
    requests: Arc<Mutex<Vec<(Duration, HttpRequest)>>>,
    responses: Arc<Mutex<VecDeque<Result<HttpResponse, HttpError>>>>,
}

impl FakeHttp {
    fn new(
        clock: FakeClock,
        responses: impl IntoIterator<Item = Result<HttpResponse, HttpError>>,
    ) -> (Self, RecordedRequests) {
        let requests = Arc::new(Mutex::new(Vec::new()));
        (
            Self {
                clock,
                requests: requests.clone(),
                responses: Arc::new(Mutex::new(responses.into_iter().collect())),
            },
            requests,
        )
    }
}

impl HttpClient for FakeHttp {
    fn execute(&self, request: HttpRequest) -> Result<HttpResponse, HttpError> {
        self.requests
            .lock()
            .expect("requests")
            .push((self.clock.monotonic(), request));
        self.responses
            .lock()
            .expect("responses")
            .pop_front()
            .expect("fake response")
    }
}

type ServiceCall = (Duration, Aircraft, EnrichmentNeeds);

struct FakeService {
    clock: FakeClock,
    calls: Arc<Mutex<Vec<ServiceCall>>>,
    results: VecDeque<Result<FlightLookup, FlightDataError>>,
    actions: VecDeque<Box<dyn FnOnce() + Send>>,
}

impl FakeService {
    fn new(
        clock: FakeClock,
        results: impl IntoIterator<Item = Result<FlightLookup, FlightDataError>>,
    ) -> (Self, Arc<Mutex<Vec<ServiceCall>>>) {
        let calls = Arc::new(Mutex::new(Vec::new()));
        (
            Self {
                clock,
                calls: calls.clone(),
                results: results.into_iter().collect(),
                actions: VecDeque::new(),
            },
            calls,
        )
    }

    fn with_action(mut self, action: impl FnOnce() + Send + 'static) -> Self {
        self.actions.push_back(Box::new(action));
        self
    }
}

impl FlightDataService for FakeService {
    fn lookup(
        &mut self,
        aircraft: &Aircraft,
        needs: EnrichmentNeeds,
    ) -> Result<FlightLookup, FlightDataError> {
        self.calls.lock().expect("service calls").push((
            self.clock.monotonic(),
            aircraft.clone(),
            needs,
        ));
        if let Some(action) = self.actions.pop_front() {
            action();
        }
        self.results.pop_front().expect("fake lookup result")
    }
}

fn configured(route: bool, model: bool) -> RadarSettings {
    RadarSettings {
        location: Some(Location {
            latitude: 40.0,
            longitude: -74.0,
            label: "test location".to_owned(),
        }),
        show_route: route,
        show_expanded_model: model,
        ..RadarSettings::default()
    }
}

fn aircraft(hex: &str, callsign: &str, latitude: f64, longitude: f64) -> Aircraft {
    Aircraft {
        hex: hex.to_owned(),
        flight_callsign: callsign.to_owned(),
        latitude,
        longitude,
        nose_degrees: 0.0,
        track_degrees: 0.0,
        ground_speed_knots: 0.0,
        callsign: callsign.to_owned(),
        aircraft_type: "B738".to_owned(),
        altitude_feet: Some(10_000),
        altitude: "10000 ft".to_owned(),
    }
}

fn found_model(model: &str) -> Result<FlightLookup, FlightDataError> {
    Ok(FlightLookup {
        route: LookupValue::NotRequested,
        model: LookupValue::Found(model.to_owned()),
    })
}

fn missing_model() -> Result<FlightLookup, FlightDataError> {
    Ok(FlightLookup {
        route: LookupValue::NotRequested,
        model: LookupValue::Missing,
    })
}

fn found_both() -> Result<FlightLookup, FlightDataError> {
    Ok(FlightLookup {
        route: LookupValue::Found("JFK→LAX".to_owned()),
        model: LookupValue::Found("737-800".to_owned()),
    })
}

fn response(body: &[u8]) -> Result<HttpResponse, HttpError> {
    Ok(HttpResponse {
        status: 200,
        body: body.to_vec(),
    })
}

fn client(
    clock: FakeClock,
    responses: impl IntoIterator<Item = Result<HttpResponse, HttpError>>,
) -> (TestClient, RecordedRequests, RecordedSleeps) {
    let (http, requests) = FakeHttp::new(clock.clone(), responses);
    let (sleeper, sleeps) = FakeSleeper::new(clock.clone());
    (
        FlightDataClient::with_provider_base(
            http,
            clock,
            sleeper,
            "https://api.adsbdb.test/v0".to_owned(),
        ),
        requests,
        sleeps,
    )
}

fn run<D: FlightDataService, W: Waiter>(
    service: D,
    model: RuntimeModel,
    clock: FakeClock,
    waiter: W,
) {
    let (_sender, receiver) = std::sync::mpsc::channel();
    FlightDataWorker::new(service, model, clock, waiter)
        .run(receiver, Arc::new(AtomicBool::new(false)));
}

#[test]
fn disabled_and_unconfigured_settings_make_no_adsbdb_request() {
    for settings in [
        configured(false, false),
        RadarSettings {
            show_route: true,
            show_expanded_model: true,
            ..RadarSettings::default()
        },
    ] {
        let clock = FakeClock::default();
        let model = RuntimeModel::new(settings, "http://planeradar.local".to_owned());
        model.record_aircraft(
            vec![aircraft("abc123", "aal1", 40.01, -74.0)],
            Duration::ZERO,
        );
        let (service, calls) = FakeService::new(clock.clone(), []);
        let (waiter, waits) = ScriptedWaiter::new(clock.clone(), [WaitOutcome::Stop]);

        run(service, model, clock, waiter);

        assert!(calls.lock().expect("calls").is_empty());
        assert_eq!(*waits.lock().expect("waits"), [Duration::from_secs(30)]);
    }
}

#[test]
fn model_only_sends_the_hex_without_the_raw_callsign() {
    let clock = FakeClock::default();
    let model = RuntimeModel::new(configured(false, true), "http://local".to_owned());
    model.record_aircraft(
        vec![aircraft("ab-c 123", "private 12", 40.01, -74.0)],
        Duration::ZERO,
    );
    let model_response = br#"{"response":{"aircraft":{"type":"Boeing 737-800"}}}"#;
    let (service, requests, _) = client(clock.clone(), [response(model_response)]);
    let (waiter, _) = ScriptedWaiter::new(clock.clone(), [WaitOutcome::Stop]);

    run(service, model, clock, waiter);

    let requests = requests.lock().expect("requests");
    assert_eq!(requests.len(), 1);
    assert_eq!(
        requests[0].1.url,
        "https://api.adsbdb.test/v0/aircraft/ABC123"
    );
    assert!(requests[0].1.query.is_empty());
    assert!(!requests[0].1.url.contains("PRIVATE12"));
}

#[test]
fn route_only_sends_the_raw_callsign_without_the_hex() {
    let clock = FakeClock::default();
    let model = RuntimeModel::new(configured(true, false), "http://local".to_owned());
    model.record_aircraft(
        vec![aircraft("private hex", "aa-l 12!", 40.01, -74.0)],
        Duration::ZERO,
    );
    let route_response = br#"{"response":{"flightroute":{"origin":{"iata_code":"JFK"},"destination":{"iata_code":"LAX"}}}}"#;
    let (service, requests, _) = client(clock.clone(), [response(route_response)]);
    let (waiter, _) = ScriptedWaiter::new(clock.clone(), [WaitOutcome::Stop]);

    run(service, model, clock, waiter);

    let requests = requests.lock().expect("requests");
    assert_eq!(requests.len(), 1);
    assert_eq!(
        requests[0].1.url,
        "https://api.adsbdb.test/v0/callsign/AAL12"
    );
    assert!(requests[0].1.query.is_empty());
    assert!(!requests[0].1.url.contains("PRIVATEHEX"));
}

#[test]
fn nearest_finite_distance_pending_aircraft_is_looked_up_first() {
    let clock = FakeClock::default();
    let model = RuntimeModel::new(configured(false, true), "http://local".to_owned());
    model.record_aircraft(
        vec![
            aircraft("far", "far1", 41.0, -74.0),
            aircraft("invalid", "bad1", f64::NAN, -74.0),
            aircraft("near", "near1", 40.01, -74.0),
        ],
        Duration::ZERO,
    );
    let (service, calls) = FakeService::new(clock.clone(), [missing_model()]);
    let (waiter, _) = ScriptedWaiter::new(clock.clone(), [WaitOutcome::Stop]);

    run(service, model, clock, waiter);

    let calls = calls.lock().expect("calls");
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].1.hex, "near");
    assert_eq!(
        calls[0].2,
        EnrichmentNeeds {
            route: false,
            model: true,
        }
    );
}

#[test]
fn successful_and_missing_cache_entries_suppress_duplicates_and_space_starts() {
    let clock = FakeClock::default();
    let model = RuntimeModel::new(configured(false, true), "http://local".to_owned());
    model.record_aircraft(
        vec![
            aircraft("abc001", "near1", 40.01, -74.0),
            aircraft("abc002", "far1", 40.02, -74.0),
        ],
        Duration::ZERO,
    );
    let found = br#"{"response":{"aircraft":{"type":"Boeing 737-800"}}}"#;
    let missing = br#"{"response":"unknown aircraft"}"#;
    let (service, requests, _) = client(clock.clone(), [response(found), response(missing)]);
    let (waiter, waits) = ScriptedWaiter::new(
        clock.clone(),
        [
            WaitOutcome::TimedOut,
            WaitOutcome::TimedOut,
            WaitOutcome::Stop,
        ],
    );

    run(service, model.clone(), clock, waiter);

    let requests = requests.lock().expect("requests");
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[0].0, Duration::ZERO);
    assert_eq!(requests[1].0, Duration::from_millis(750));
    assert!(requests[0].1.url.ends_with("/aircraft/ABC001"));
    assert!(requests[1].1.url.ends_with("/aircraft/ABC002"));
    assert_eq!(
        *waits.lock().expect("waits"),
        [
            Duration::from_millis(750),
            Duration::from_millis(750),
            Duration::from_secs(3),
        ]
    );
    assert_eq!(
        model
            .snapshot()
            .enrichment
            .get(&aircraft("abc001", "near1", 0.0, 0.0).key())
            .and_then(|enrichment| enrichment.model.as_deref()),
        Some("737-800")
    );
}

#[test]
fn network_failure_requests_a_thirty_second_wait() {
    let clock = FakeClock::default();
    let model = RuntimeModel::new(configured(false, true), "http://local".to_owned());
    model.record_aircraft(
        vec![aircraft("abc123", "aal1", 40.01, -74.0)],
        Duration::ZERO,
    );
    let (service, _) = FakeService::new(
        clock.clone(),
        [Err(FlightDataError::Http(HttpError::Timeout))],
    );
    let (waiter, waits) = ScriptedWaiter::new(clock.clone(), [WaitOutcome::Stop]);

    run(service, model, clock, waiter);

    assert_eq!(*waits.lock().expect("waits"), [Duration::from_secs(30)]);
}

#[test]
fn successful_lookup_does_not_reset_the_thirty_second_failure_log_throttle() {
    reset_recorded_logs();
    let clock = FakeClock::default();
    let settings = configured(false, true);
    let model = RuntimeModel::new(settings.clone(), "http://local".to_owned());
    model.record_aircraft(
        vec![aircraft("aaa001", "aa1", 40.01, -74.0)],
        Duration::ZERO,
    );
    let (service, calls) = FakeService::new(
        clock.clone(),
        [
            Err(FlightDataError::Http(HttpError::Timeout)),
            found_model("737-800"),
            Err(FlightDataError::Http(HttpError::Transport)),
        ],
    );
    let replacement_model = model.clone();
    let second_replacement_model = model.clone();
    let (waiter, waits) = ScriptedWaiter::new(
        clock.clone(),
        [
            WaitOutcome::ActionSettingsChangedAfter(
                Duration::from_secs(10),
                Box::new(move || {
                    replacement_model.record_aircraft(
                        vec![aircraft("aaa002", "bb2", 40.02, -74.0)],
                        Duration::from_secs(10),
                    );
                }),
            ),
            WaitOutcome::Action(Box::new(move || {
                second_replacement_model.record_aircraft(
                    vec![aircraft("aaa003", "cc3", 40.03, -74.0)],
                    Duration::from_millis(10_750),
                );
            })),
            WaitOutcome::Stop,
        ],
    );

    run(service, model, clock, waiter);

    assert_eq!(
        calls
            .lock()
            .expect("calls")
            .iter()
            .map(|(at, aircraft, _)| (*at, aircraft.hex.as_str()))
            .collect::<Vec<_>>(),
        [
            (Duration::ZERO, "aaa001"),
            (Duration::from_secs(10), "aaa002"),
            (Duration::from_millis(10_750), "aaa003"),
        ]
    );
    assert_eq!(
        *waits.lock().expect("waits"),
        [
            Duration::from_secs(30),
            Duration::from_millis(750),
            Duration::from_secs(30),
        ]
    );
    let throttle_logs = RECORDING_LOGGER
        .messages
        .lock()
        .expect("log messages")
        .iter()
        .filter(|message| message.contains("AAA001"))
        .cloned()
        .collect::<Vec<_>>();
    assert_eq!(
        throttle_logs,
        ["provider=ADSBDB category=timeout aircraft=AAA001/AA1"]
    );
}

#[test]
fn unchanged_settings_notification_preserves_the_failure_deadline() {
    let clock = FakeClock::default();
    let settings = configured(false, true);
    let model = RuntimeModel::new(settings.clone(), "http://local".to_owned());
    model.record_aircraft(
        vec![aircraft("abc123", "aal1", 40.01, -74.0)],
        Duration::ZERO,
    );
    let (service, calls) = FakeService::new(
        clock.clone(),
        [
            Err(FlightDataError::Http(HttpError::Timeout)),
            Err(FlightDataError::Http(HttpError::Timeout)),
            Err(FlightDataError::Http(HttpError::Timeout)),
        ],
    );
    let (waiter, waits) = ScriptedWaiter::new(
        clock.clone(),
        [
            WaitOutcome::SettingsChangedAfter(Duration::from_secs(10), settings),
            WaitOutcome::TimedOut,
            WaitOutcome::Stop,
        ],
    );

    run(service, model, clock, waiter);

    assert_eq!(
        calls
            .lock()
            .expect("calls")
            .iter()
            .map(|(at, _, _)| *at)
            .collect::<Vec<_>>(),
        [Duration::ZERO, Duration::from_secs(30)]
    );
    assert_eq!(
        *waits.lock().expect("waits"),
        [
            Duration::from_secs(30),
            Duration::from_secs(20),
            Duration::from_secs(30),
        ]
    );
}

#[test]
fn unrelated_settings_notification_preserves_the_failure_deadline() {
    let clock = FakeClock::default();
    let settings = configured(false, true);
    let model = RuntimeModel::new(settings, "http://local".to_owned());
    model.record_aircraft(
        vec![aircraft("abc123", "aal1", 40.01, -74.0)],
        Duration::ZERO,
    );
    let changed_model = model.clone();
    let (service, calls) = FakeService::new(
        clock.clone(),
        [
            Err(FlightDataError::Http(HttpError::Timeout)),
            Err(FlightDataError::Http(HttpError::Timeout)),
            Err(FlightDataError::Http(HttpError::Timeout)),
        ],
    );
    let (waiter, _) = ScriptedWaiter::new(
        clock.clone(),
        [
            WaitOutcome::ActionSettingsChangedAfter(
                Duration::from_secs(10),
                Box::new(move || {
                    let mut changed = configured(false, true);
                    changed.radar_text_scale_percent = 110;
                    changed_model.replace_settings(changed);
                }),
            ),
            WaitOutcome::TimedOut,
            WaitOutcome::Stop,
        ],
    );

    run(service, model, clock, waiter);

    assert_eq!(
        calls
            .lock()
            .expect("calls")
            .iter()
            .map(|(at, _, _)| *at)
            .collect::<Vec<_>>(),
        [Duration::ZERO, Duration::from_secs(30)]
    );
}

#[test]
fn material_needs_or_active_identity_change_can_retry_immediately() {
    let clock = FakeClock::default();
    let model = RuntimeModel::new(configured(false, true), "http://local".to_owned());
    model.record_aircraft(
        vec![aircraft("abc123", "aal1", 40.01, -74.0)],
        Duration::ZERO,
    );
    let needs_model = model.clone();
    let identity_model = model.clone();
    let (service, calls) = FakeService::new(
        clock.clone(),
        [
            Err(FlightDataError::Http(HttpError::Timeout)),
            Err(FlightDataError::Http(HttpError::Timeout)),
            Err(FlightDataError::Http(HttpError::Timeout)),
        ],
    );
    let (waiter, _) = ScriptedWaiter::new(
        clock.clone(),
        [
            WaitOutcome::ActionSettingsChangedAfter(
                Duration::from_secs(5),
                Box::new(move || {
                    needs_model.replace_settings(configured(true, false));
                }),
            ),
            WaitOutcome::ActionSettingsChangedAfter(
                Duration::from_secs(5),
                Box::new(move || {
                    identity_model.record_aircraft(
                        vec![aircraft("def456", "dal2", 40.02, -74.0)],
                        Duration::from_secs(10),
                    );
                }),
            ),
            WaitOutcome::Stop,
        ],
    );

    run(service, model, clock, waiter);

    assert_eq!(
        calls
            .lock()
            .expect("calls")
            .iter()
            .map(|(at, aircraft, needs)| (*at, aircraft.hex.as_str(), *needs))
            .collect::<Vec<_>>(),
        [
            (
                Duration::ZERO,
                "abc123",
                EnrichmentNeeds {
                    route: false,
                    model: true,
                },
            ),
            (
                Duration::from_secs(5),
                "abc123",
                EnrichmentNeeds {
                    route: true,
                    model: false,
                },
            ),
            (
                Duration::from_secs(10),
                "def456",
                EnrichmentNeeds {
                    route: true,
                    model: false,
                },
            ),
        ]
    );
}

#[test]
fn disabling_during_failure_backoff_stops_lookup_work_immediately() {
    let clock = FakeClock::default();
    let model = RuntimeModel::new(configured(false, true), "http://local".to_owned());
    model.record_aircraft(
        vec![aircraft("abc123", "aal1", 40.01, -74.0)],
        Duration::ZERO,
    );
    let disabled_model = model.clone();
    let (service, calls) = FakeService::new(
        clock.clone(),
        [Err(FlightDataError::Http(HttpError::Timeout))],
    );
    let (waiter, waits) = ScriptedWaiter::new(
        clock.clone(),
        [
            WaitOutcome::ActionSettingsChangedAfter(
                Duration::from_secs(10),
                Box::new(move || {
                    disabled_model.replace_settings(configured(false, false));
                }),
            ),
            WaitOutcome::Stop,
        ],
    );

    run(service, model, clock, waiter);

    assert_eq!(calls.lock().expect("calls").len(), 1);
    assert_eq!(
        *waits.lock().expect("waits"),
        [Duration::from_secs(30), Duration::from_secs(30)]
    );
}

#[test]
fn settings_change_wakes_disabled_wait() {
    let disabled_clock = FakeClock::default();
    let disabled_settings = configured(false, false);
    let disabled_model = RuntimeModel::new(disabled_settings.clone(), "http://local".to_owned());
    disabled_model.record_aircraft(
        vec![aircraft("abc123", "aal1", 40.01, -74.0)],
        Duration::ZERO,
    );
    let (disabled_service, disabled_calls) = FakeService::new(disabled_clock.clone(), []);
    let (disabled_waiter, disabled_waits) = ScriptedWaiter::new(
        disabled_clock.clone(),
        [
            WaitOutcome::SettingsChanged(disabled_settings),
            WaitOutcome::Stop,
        ],
    );
    run(
        disabled_service,
        disabled_model,
        disabled_clock.clone(),
        disabled_waiter,
    );
    assert!(disabled_calls.lock().expect("calls").is_empty());
    assert_eq!(disabled_clock.monotonic(), Duration::ZERO);
    assert_eq!(
        *disabled_waits.lock().expect("waits"),
        [Duration::from_secs(30), Duration::from_secs(30)]
    );
}

#[test]
fn disabling_during_in_flight_work_keeps_the_result_hidden() {
    let clock = FakeClock::default();
    let model = RuntimeModel::new(configured(false, true), "http://local".to_owned());
    let tracked = aircraft("abc123", "aal1", 40.01, -74.0);
    model.record_aircraft(vec![tracked.clone()], Duration::ZERO);
    let changed_model = model.clone();
    let (service, _) = FakeService::new(clock.clone(), [found_model("737-800")]);
    let service = service.with_action(move || {
        changed_model.replace_settings(configured(false, false));
    });
    let (waiter, _) = ScriptedWaiter::new(clock.clone(), [WaitOutcome::Stop]);

    run(service, model.clone(), clock, waiter);

    assert!(!model.snapshot().enrichment.contains_key(&tracked.key()));
}

#[test]
fn changed_needs_reject_an_in_flight_combined_result() {
    let clock = FakeClock::default();
    let model = RuntimeModel::new(configured(true, true), "http://local".to_owned());
    let tracked = aircraft("abc123", "aal1", 40.01, -74.0);
    model.record_aircraft(vec![tracked.clone()], Duration::ZERO);
    let changed_model = model.clone();
    let (service, calls) = FakeService::new(clock.clone(), [found_both()]);
    let service = service.with_action(move || {
        changed_model.replace_settings(configured(false, true));
    });
    let (waiter, _) = ScriptedWaiter::new(clock.clone(), [WaitOutcome::Stop]);

    run(service, model.clone(), clock, waiter);

    assert_eq!(
        calls.lock().expect("calls")[0].2,
        EnrichmentNeeds {
            route: true,
            model: true,
        }
    );
    assert!(!model.snapshot().enrichment.contains_key(&tracked.key()));
}

#[test]
fn conditional_model_publication_rechecks_settings_under_its_write_lock() {
    let model = RuntimeModel::new(configured(true, true), "http://local".to_owned());
    let tracked = aircraft("abc123", "aal1", 40.01, -74.0);
    let key = tracked.key();
    model.record_aircraft(vec![tracked], Duration::ZERO);
    let expected_location = configured(true, true).location.expect("location");
    let expected_needs = EnrichmentNeeds {
        route: true,
        model: true,
    };
    let barrier = Arc::new(Barrier::new(2));

    let publisher_model = model.clone();
    let publisher_barrier = barrier.clone();
    let publisher = std::thread::spawn(move || {
        publisher_barrier.wait();
        publisher_barrier.wait();
        publisher_model.record_enrichment_if_current(
            &expected_location,
            expected_needs,
            &key,
            planeradar::flight_data::AircraftEnrichment {
                route: Some("JFK→LAX".to_owned()),
                model: Some("737-800".to_owned()),
            },
        )
    });

    barrier.wait();
    model.replace_settings(configured(false, false));
    let disabled_generation = model.snapshot().generation;
    barrier.wait();

    assert_eq!(publisher.join().expect("publisher"), None);
    let snapshot = model.snapshot();
    assert!(snapshot.enrichment.is_empty());
    assert_eq!(snapshot.generation, disabled_generation);
}

#[test]
fn departed_aircraft_rejects_late_publication() {
    let clock = FakeClock::default();
    let model = RuntimeModel::new(configured(false, true), "http://local".to_owned());
    let departed = aircraft("abc123", "aal1", 40.01, -74.0);
    model.record_aircraft(vec![departed.clone()], Duration::ZERO);
    let changed_model = model.clone();
    let (service, _) = FakeService::new(clock.clone(), [found_model("737-800")]);
    let service = service.with_action(move || {
        changed_model.record_aircraft(Vec::new(), Duration::from_secs(1));
    });
    let (waiter, _) = ScriptedWaiter::new(clock.clone(), [WaitOutcome::Stop]);

    run(service, model.clone(), clock, waiter);

    assert!(!model.snapshot().enrichment.contains_key(&departed.key()));
}

#[test]
fn cached_result_republishes_for_a_returning_aircraft_without_a_network_request() {
    let clock = FakeClock::default();
    let model = RuntimeModel::new(configured(false, true), "http://local".to_owned());
    let returning = aircraft("abc123", "aal1", 40.01, -74.0);
    model.record_aircraft(vec![returning.clone()], Duration::ZERO);
    let (service, calls) = FakeService::new(clock.clone(), [found_model("737-800")]);

    let clear_model = model.clone();
    let restore_model = model.clone();
    let restored_aircraft = returning.clone();
    let (waiter, waits) = ScriptedWaiter::new(
        clock.clone(),
        [
            WaitOutcome::Action(Box::new(move || {
                clear_model.record_aircraft(Vec::new(), Duration::from_secs(1));
            })),
            WaitOutcome::Action(Box::new(move || {
                restore_model.record_aircraft(vec![restored_aircraft], Duration::from_secs(2));
            })),
            WaitOutcome::Stop,
        ],
    );

    run(service, model.clone(), clock, waiter);

    assert_eq!(calls.lock().expect("calls").len(), 1);
    assert_eq!(
        model
            .snapshot()
            .enrichment
            .get(&returning.key())
            .and_then(|enrichment| enrichment.model.as_deref()),
        Some("737-800")
    );
    assert_eq!(
        *waits.lock().expect("waits"),
        [
            Duration::from_millis(750),
            Duration::from_secs(3),
            Duration::from_secs(3),
        ]
    );
}

struct BlockingFlightService {
    entered: std::sync::mpsc::SyncSender<()>,
    release: Receiver<()>,
}

impl FlightDataService for BlockingFlightService {
    fn lookup(
        &mut self,
        _aircraft: &Aircraft,
        _needs: EnrichmentNeeds,
    ) -> Result<FlightLookup, FlightDataError> {
        self.entered.send(()).expect("flight entered");
        self.release.recv().expect("release flight");
        found_model("737-800")
    }
}

#[derive(Clone, Copy)]
struct ImmediateAdsbHttp;

impl HttpClient for ImmediateAdsbHttp {
    fn execute(&self, _request: HttpRequest) -> Result<HttpResponse, HttpError> {
        Ok(HttpResponse {
            status: 200,
            body: br#"{"ac":[]}"#.to_vec(),
        })
    }
}

#[test]
fn blocked_flight_call_does_not_hold_the_model_lock_or_delay_adsb_publication() {
    let model = RuntimeModel::new(configured(false, true), "http://local".to_owned());
    model.record_aircraft(
        vec![aircraft("abc123", "aal1", 40.01, -74.0)],
        Duration::ZERO,
    );
    let stop = Arc::new(AtomicBool::new(false));
    let (flight_entered_sender, flight_entered_receiver) = std::sync::mpsc::sync_channel(0);
    let (flight_release_sender, flight_release_receiver) = std::sync::mpsc::channel();
    let (flight_commands, flight_receiver) = std::sync::mpsc::channel();
    let flight_model = model.clone();
    let flight_stop = stop.clone();
    let flight = std::thread::spawn(move || {
        FlightDataWorker::new(
            BlockingFlightService {
                entered: flight_entered_sender,
                release: flight_release_receiver,
            },
            flight_model,
            FakeClock::default(),
            ChannelWaiter,
        )
        .run(flight_receiver, flight_stop);
    });
    flight_entered_receiver
        .recv_timeout(Duration::from_secs(1))
        .expect("flight call blocks");

    let (adsb_commands, adsb_receiver) = std::sync::mpsc::channel();
    let adsb_model = model.clone();
    let adsb_stop = stop.clone();
    let adsb = std::thread::spawn(move || {
        AdsbWorker::new(
            AdsbClient::new(ImmediateAdsbHttp),
            adsb_model,
            FakeClock::default(),
            ChannelWaiter,
        )
        .run(adsb_receiver, adsb_stop);
    });
    let deadline = std::time::Instant::now() + Duration::from_secs(1);
    while !model.snapshot().aircraft.is_empty() {
        assert!(
            std::time::Instant::now() < deadline,
            "ADS-B publication was blocked"
        );
        std::thread::yield_now();
    }

    assert!(model.snapshot().enrichment.is_empty());
    adsb_commands.send(WorkerCommand::Stop).expect("stop ADS-B");
    flight_release_sender.send(()).expect("release flight");
    let _ = flight_commands.send(WorkerCommand::Stop);
    adsb.join().expect("ADS-B joins");
    flight.join().expect("flight joins");
}

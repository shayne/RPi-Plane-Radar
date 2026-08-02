use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use planeradar::adsb::AdsbClient;
use planeradar::http::{HttpClient, HttpError, HttpRequest, HttpResponse};
use planeradar::model::{EnvironmentReading, FooterSettings, Location, RadarSettings, TimeZone};
use planeradar::runtime::{
    ChannelWaiter, RuntimeModel, WaitResult, Waiter, WeatherWorker, WorkerCommand,
};
use planeradar::time::Clock;
use planeradar::weather::WeatherClient;

const SUCCESS_INTERVAL: Duration = Duration::from_secs(15 * 60);
type RecordedRequests = Arc<Mutex<Vec<(Duration, HttpRequest)>>>;

#[derive(Clone, Default)]
struct FakeClock {
    monotonic_ms: Arc<AtomicU64>,
    unix_seconds: Arc<AtomicU64>,
}

impl FakeClock {
    fn advance(&self, duration: Duration) {
        self.monotonic_ms.fetch_add(
            u64::try_from(duration.as_millis()).expect("test duration fits u64"),
            Ordering::AcqRel,
        );
    }
}

impl Clock for FakeClock {
    fn monotonic(&self) -> Duration {
        Duration::from_millis(self.monotonic_ms.load(Ordering::Acquire))
    }

    fn unix_seconds(&self) -> u64 {
        self.unix_seconds.load(Ordering::Acquire)
    }
}

enum WaitOutcome {
    TimedOut,
    Action(Box<dyn FnOnce() + Send>),
    ActionAfter(Duration, Box<dyn FnOnce() + Send>),
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
                WaitResult::Command(WorkerCommand::SettingsChanged(RadarSettings::default()))
            }
            WaitOutcome::ActionAfter(elapsed, action) => {
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

type HttpAction = Box<dyn FnOnce() + Send>;

#[derive(Clone)]
struct FakeHttp {
    clock: FakeClock,
    requests: Arc<Mutex<Vec<(Duration, HttpRequest)>>>,
    responses: Arc<Mutex<VecDeque<Result<HttpResponse, HttpError>>>>,
    actions: Arc<Mutex<VecDeque<HttpAction>>>,
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
                actions: Arc::new(Mutex::new(VecDeque::new())),
            },
            requests,
        )
    }

    fn with_action(self, action: impl FnOnce() + Send + 'static) -> Self {
        self.actions
            .lock()
            .expect("HTTP actions")
            .push_back(Box::new(action));
        self
    }
}

impl HttpClient for FakeHttp {
    fn execute(&self, request: HttpRequest) -> Result<HttpResponse, HttpError> {
        self.requests
            .lock()
            .expect("requests")
            .push((self.clock.monotonic(), request));
        if let Some(action) = self.actions.lock().expect("HTTP actions").pop_front() {
            action();
        }
        self.responses
            .lock()
            .expect("responses")
            .pop_front()
            .expect("fake response")
    }
}

fn location(latitude: f64) -> Location {
    Location {
        latitude,
        longitude: -74.0,
        label: "test location".to_owned(),
    }
}

fn configured(footer: FooterSettings) -> RadarSettings {
    RadarSettings {
        location: Some(location(40.0)),
        footer,
        ..RadarSettings::default()
    }
}

fn weather_footer() -> FooterSettings {
    FooterSettings {
        show_condition: true,
        ..FooterSettings::default()
    }
}

fn radar_local_time_footer() -> FooterSettings {
    FooterSettings {
        show_time: true,
        time_zone: TimeZone::RadarLocal,
        ..FooterSettings::default()
    }
}

fn zulu_time_footer() -> FooterSettings {
    FooterSettings {
        show_time: true,
        time_zone: TimeZone::Zulu,
        ..FooterSettings::default()
    }
}

fn ok() -> Result<HttpResponse, HttpError> {
    Ok(HttpResponse {
        status: 200,
        body: br#"{"utc_offset_seconds":-14400,"current":{"temperature_2m":21.5,"relative_humidity_2m":48,"weather_code":2}}"#.to_vec(),
    })
}

fn run<W: Waiter>(http: FakeHttp, model: RuntimeModel, clock: FakeClock, waiter: W) {
    let (_sender, receiver) = mpsc::channel();
    WeatherWorker::new(
        WeatherClient::with_provider_base(
            http,
            "https://api.open-meteo.test/v1/forecast".to_owned(),
        ),
        model,
        clock,
        waiter,
    )
    .run(receiver, Arc::new(AtomicBool::new(false)));
}

#[test]
fn environment_dependency_table_controls_requests() {
    let cases = [
        (FooterSettings::default(), false),
        (zulu_time_footer(), false),
        (
            FooterSettings {
                show_date: true,
                time_zone: TimeZone::Zulu,
                ..FooterSettings::default()
            },
            false,
        ),
        (weather_footer(), true),
        (
            FooterSettings {
                show_temperature: true,
                ..FooterSettings::default()
            },
            true,
        ),
        (
            FooterSettings {
                show_humidity: true,
                ..FooterSettings::default()
            },
            true,
        ),
        (radar_local_time_footer(), true),
        (
            FooterSettings {
                show_date: true,
                time_zone: TimeZone::RadarLocal,
                ..FooterSettings::default()
            },
            true,
        ),
    ];

    for (footer, should_request) in cases {
        let clock = FakeClock::default();
        let model = RuntimeModel::new(configured(footer), "http://local".to_owned());
        let responses = should_request.then(ok).into_iter();
        let (http, requests) = FakeHttp::new(clock.clone(), responses);
        let (waiter, _) = ScriptedWaiter::new(clock.clone(), [WaitOutcome::Stop]);

        run(http, model, clock, waiter);

        assert_eq!(
            requests.lock().expect("requests").len(),
            usize::from(should_request)
        );
    }
}

#[test]
fn first_fetch_is_immediate_and_successes_are_fifteen_minutes_apart() {
    let clock = FakeClock::default();
    let model = RuntimeModel::new(configured(weather_footer()), "http://local".to_owned());
    let (http, requests) = FakeHttp::new(clock.clone(), [ok(), ok()]);
    let (waiter, waits) =
        ScriptedWaiter::new(clock.clone(), [WaitOutcome::TimedOut, WaitOutcome::Stop]);

    run(http, model.clone(), clock, waiter);

    assert_eq!(
        requests
            .lock()
            .expect("requests")
            .iter()
            .map(|(at, _)| *at)
            .collect::<Vec<_>>(),
        [Duration::ZERO, SUCCESS_INTERVAL]
    );
    assert_eq!(*waits.lock().expect("waits"), [SUCCESS_INTERVAL; 2]);
    assert_eq!(
        model
            .snapshot()
            .environment
            .expect("environment")
            .fetched_at,
        SUCCESS_INTERVAL
    );
}

#[test]
fn successful_request_completion_anchors_freshness_and_refresh_deadline() {
    let clock = FakeClock::default();
    let model = RuntimeModel::new(configured(weather_footer()), "http://local".to_owned());
    let request_clock = clock.clone();
    let (http, _) = FakeHttp::new(clock.clone(), [ok()]);
    let http = http.with_action(move || request_clock.advance(Duration::from_secs(7)));
    let (waiter, waits) = ScriptedWaiter::new(clock.clone(), [WaitOutcome::Stop]);

    run(http, model.clone(), clock, waiter);

    assert_eq!(
        model
            .snapshot()
            .environment
            .expect("environment")
            .fetched_at,
        Duration::from_secs(7)
    );
    assert_eq!(*waits.lock().expect("waits"), [SUCCESS_INTERVAL]);
}

#[test]
fn failed_request_completion_anchors_retry_deadline() {
    let clock = FakeClock::default();
    let model = RuntimeModel::new(configured(weather_footer()), "http://local".to_owned());
    let request_clock = clock.clone();
    let (http, requests) = FakeHttp::new(
        clock.clone(),
        [Err(HttpError::Timeout), Err(HttpError::Timeout)],
    );
    let http = http.with_action(move || request_clock.advance(Duration::from_secs(7)));
    let (waiter, waits) =
        ScriptedWaiter::new(clock.clone(), [WaitOutcome::TimedOut, WaitOutcome::Stop]);

    run(http, model, clock, waiter);

    assert_eq!(
        requests
            .lock()
            .expect("requests")
            .iter()
            .map(|(at, _)| *at)
            .collect::<Vec<_>>(),
        [Duration::ZERO, Duration::from_secs(37)]
    );
    assert_eq!(
        *waits.lock().expect("waits"),
        [Duration::from_secs(30), Duration::from_secs(60)]
    );
}

#[test]
fn irrelevant_settings_change_preserves_the_remaining_success_deadline() {
    let clock = FakeClock::default();
    let model = RuntimeModel::new(configured(weather_footer()), "http://local".to_owned());
    let changed_model = model.clone();
    let (http, requests) = FakeHttp::new(clock.clone(), [ok(), ok(), ok()]);
    let (waiter, waits) = ScriptedWaiter::new(
        clock.clone(),
        [
            WaitOutcome::ActionAfter(
                Duration::from_secs(120),
                Box::new(move || {
                    let mut settings = configured(weather_footer());
                    settings.range_index = 2;
                    changed_model.replace_settings(settings);
                }),
            ),
            WaitOutcome::TimedOut,
            WaitOutcome::Stop,
        ],
    );

    run(http, model, clock, waiter);

    assert_eq!(
        requests
            .lock()
            .expect("requests")
            .iter()
            .map(|(at, _)| *at)
            .collect::<Vec<_>>(),
        [Duration::ZERO, SUCCESS_INTERVAL]
    );
    assert_eq!(
        *waits.lock().expect("waits"),
        [
            SUCCESS_INTERVAL,
            Duration::from_secs(13 * 60),
            SUCCESS_INTERVAL,
        ]
    );
}

#[test]
fn irrelevant_settings_change_preserves_the_remaining_failure_deadline() {
    let clock = FakeClock::default();
    let model = RuntimeModel::new(configured(weather_footer()), "http://local".to_owned());
    let changed_model = model.clone();
    let (http, requests) = FakeHttp::new(
        clock.clone(),
        [
            Err(HttpError::Timeout),
            Err(HttpError::Timeout),
            Err(HttpError::Timeout),
        ],
    );
    let (waiter, waits) = ScriptedWaiter::new(
        clock.clone(),
        [
            WaitOutcome::ActionAfter(
                Duration::from_secs(10),
                Box::new(move || {
                    let mut settings = configured(weather_footer());
                    settings.range_index = 2;
                    changed_model.replace_settings(settings);
                }),
            ),
            WaitOutcome::TimedOut,
            WaitOutcome::Stop,
        ],
    );

    run(http, model, clock, waiter);

    assert_eq!(
        requests
            .lock()
            .expect("requests")
            .iter()
            .map(|(at, _)| *at)
            .collect::<Vec<_>>(),
        [Duration::ZERO, Duration::from_secs(30)]
    );
    assert_eq!(
        *waits.lock().expect("waits"),
        [
            Duration::from_secs(30),
            Duration::from_secs(20),
            Duration::from_secs(60),
        ]
    );
}

#[test]
fn disable_then_reenable_preserves_failure_progression() {
    let clock = FakeClock::default();
    let model = RuntimeModel::new(configured(weather_footer()), "http://local".to_owned());
    let disabled_model = model.clone();
    let enabled_model = model.clone();
    let (http, requests) = FakeHttp::new(
        clock.clone(),
        [Err(HttpError::Timeout), Err(HttpError::Timeout)],
    );
    let (waiter, waits) = ScriptedWaiter::new(
        clock.clone(),
        [
            WaitOutcome::ActionAfter(
                Duration::from_secs(10),
                Box::new(move || {
                    disabled_model.replace_settings(configured(FooterSettings::default()));
                }),
            ),
            WaitOutcome::Action(Box::new(move || {
                enabled_model.replace_settings(configured(weather_footer()));
            })),
            WaitOutcome::Stop,
        ],
    );

    run(http, model, clock, waiter);

    assert_eq!(
        requests
            .lock()
            .expect("requests")
            .iter()
            .map(|(at, _)| *at)
            .collect::<Vec<_>>(),
        [Duration::ZERO, Duration::from_secs(10)]
    );
    assert_eq!(
        *waits.lock().expect("waits"),
        [
            Duration::from_secs(30),
            Duration::from_secs(30),
            Duration::from_secs(60),
        ]
    );
}

#[test]
fn location_change_during_fetch_rejects_the_old_result_and_fetches_new_location_immediately() {
    let clock = FakeClock::default();
    let model = RuntimeModel::new(configured(weather_footer()), "http://local".to_owned());
    let changed_model = model.clone();
    let (http, requests) = FakeHttp::new(clock.clone(), [ok(), ok()]);
    let http = http.with_action(move || {
        let mut settings = configured(weather_footer());
        settings.location = Some(location(41.0));
        changed_model.replace_settings(settings);
    });
    let (waiter, _) = ScriptedWaiter::new(clock.clone(), [WaitOutcome::Stop]);

    run(http, model.clone(), clock, waiter);

    let requests = requests.lock().expect("requests");
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[0].0, Duration::ZERO);
    assert_eq!(requests[1].0, Duration::ZERO);
    assert!(
        requests[0]
            .1
            .query
            .contains(&("latitude".to_owned(), "40".to_owned()))
    );
    assert!(
        requests[1]
            .1
            .query
            .contains(&("latitude".to_owned(), "41".to_owned()))
    );
    assert!(model.snapshot().environment.is_some());
}

#[test]
fn failure_backoff_grows_then_resets_after_success() {
    let clock = FakeClock::default();
    let model = RuntimeModel::new(configured(weather_footer()), "http://local".to_owned());
    let (http, requests) = FakeHttp::new(
        clock.clone(),
        [
            Err(HttpError::Timeout),
            Err(HttpError::Transport),
            Err(HttpError::Body),
            Err(HttpError::Timeout),
            Err(HttpError::Timeout),
            ok(),
            Err(HttpError::Timeout),
        ],
    );
    let (waiter, waits) = ScriptedWaiter::new(
        clock.clone(),
        [
            WaitOutcome::TimedOut,
            WaitOutcome::TimedOut,
            WaitOutcome::TimedOut,
            WaitOutcome::TimedOut,
            WaitOutcome::TimedOut,
            WaitOutcome::TimedOut,
            WaitOutcome::Stop,
        ],
    );

    run(http, model, clock, waiter);

    assert_eq!(
        *waits.lock().expect("waits"),
        [30, 60, 300, 900, 900, 900, 30].map(Duration::from_secs)
    );
    assert_eq!(requests.lock().expect("requests").len(), 7);
}

#[test]
fn settings_change_wakes_disabled_worker_without_advancing_time() {
    let clock = FakeClock::default();
    let model = RuntimeModel::new(
        configured(FooterSettings::default()),
        "http://local".to_owned(),
    );
    let changed_model = model.clone();
    let (http, requests) = FakeHttp::new(clock.clone(), [ok()]);
    let (waiter, _) = ScriptedWaiter::new(
        clock.clone(),
        [
            WaitOutcome::Action(Box::new(move || {
                changed_model.replace_settings(configured(weather_footer()));
            })),
            WaitOutcome::Stop,
        ],
    );

    run(http, model, clock, waiter);

    assert_eq!(
        requests
            .lock()
            .expect("requests")
            .iter()
            .map(|(at, _)| *at)
            .collect::<Vec<_>>(),
        [Duration::ZERO]
    );
}

#[test]
fn disabling_environment_during_fetch_rejects_the_result() {
    let clock = FakeClock::default();
    let model = RuntimeModel::new(configured(weather_footer()), "http://local".to_owned());
    let changed_model = model.clone();
    let (http, _) = FakeHttp::new(clock.clone(), [ok()]);
    let http = http.with_action(move || {
        changed_model.replace_settings(configured(FooterSettings::default()));
    });
    let (waiter, _) = ScriptedWaiter::new(clock.clone(), [WaitOutcome::Stop]);

    run(http, model.clone(), clock, waiter);

    assert_eq!(model.snapshot().environment, None);
}

#[test]
fn weather_failure_retains_last_good_reading_and_does_not_touch_adsb_error() {
    let clock = FakeClock::default();
    let model = RuntimeModel::new(configured(weather_footer()), "http://local".to_owned());
    let previous = EnvironmentReading {
        temperature_celsius: 10.0,
        humidity_percent: 20,
        weather_code: 1,
        utc_offset_seconds: -18_000,
        fetched_at: Duration::from_secs(12),
    };
    model
        .record_environment_if_location(&location(40.0), previous.clone())
        .expect("initial reading");
    let (http, _) = FakeHttp::new(clock.clone(), [Err(HttpError::Timeout)]);
    let (waiter, _) = ScriptedWaiter::new(clock.clone(), [WaitOutcome::Stop]);

    run(http, model.clone(), clock, waiter);

    let snapshot = model.snapshot();
    assert_eq!(snapshot.environment, Some(previous));
    assert_eq!(snapshot.environment_last_error_at, Some(Duration::ZERO));
    assert_eq!(snapshot.last_error_at, None);
}

#[test]
fn stop_command_interrupts_the_success_wait() {
    let clock = FakeClock::default();
    let model = RuntimeModel::new(configured(weather_footer()), "http://local".to_owned());
    let (http, requests) = FakeHttp::new(clock.clone(), [ok()]);
    let (sender, receiver) = mpsc::channel();
    let stop = Arc::new(AtomicBool::new(false));
    let worker_stop = stop.clone();
    let worker = std::thread::spawn(move || {
        WeatherWorker::new(
            WeatherClient::with_provider_base(
                http,
                "https://api.open-meteo.test/v1/forecast".to_owned(),
            ),
            model,
            clock,
            ChannelWaiter,
        )
        .run(receiver, worker_stop);
    });
    while requests.lock().expect("requests").is_empty() {
        std::thread::yield_now();
    }

    let started = Instant::now();
    sender.send(WorkerCommand::Stop).expect("stop");
    worker.join().expect("worker joins");

    assert!(started.elapsed() < Duration::from_secs(1));
    assert!(stop.load(Ordering::Acquire));
}

struct BlockingWeatherHttp {
    entered: mpsc::SyncSender<()>,
    release: Mutex<Receiver<()>>,
}

impl HttpClient for BlockingWeatherHttp {
    fn execute(&self, _request: HttpRequest) -> Result<HttpResponse, HttpError> {
        self.entered.send(()).expect("weather entered");
        self.release
            .lock()
            .expect("weather release")
            .recv()
            .expect("release weather");
        ok()
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
fn blocked_weather_call_does_not_hold_the_model_lock_or_delay_adsb_publication() {
    let model = RuntimeModel::new(configured(weather_footer()), "http://local".to_owned());
    let stop = Arc::new(AtomicBool::new(false));
    let (weather_entered_sender, weather_entered_receiver) = mpsc::sync_channel(0);
    let (weather_release_sender, weather_release_receiver) = mpsc::channel();
    let (weather_commands, weather_receiver) = mpsc::channel();
    let weather_model = model.clone();
    let weather_stop = stop.clone();
    let weather = std::thread::spawn(move || {
        WeatherWorker::new(
            WeatherClient::with_provider_base(
                BlockingWeatherHttp {
                    entered: weather_entered_sender,
                    release: Mutex::new(weather_release_receiver),
                },
                "https://api.open-meteo.test/v1/forecast".to_owned(),
            ),
            weather_model,
            FakeClock::default(),
            ChannelWaiter,
        )
        .run(weather_receiver, weather_stop);
    });
    weather_entered_receiver
        .recv_timeout(Duration::from_secs(1))
        .expect("weather call blocks");

    let (adsb_commands, adsb_receiver) = mpsc::channel();
    let adsb_model = model.clone();
    let adsb_stop = stop.clone();
    let adsb = std::thread::spawn(move || {
        planeradar::runtime::AdsbWorker::new(
            AdsbClient::new(ImmediateAdsbHttp),
            adsb_model,
            FakeClock::default(),
            ChannelWaiter,
        )
        .run(adsb_receiver, adsb_stop);
    });
    let deadline = Instant::now() + Duration::from_secs(1);
    while !model.snapshot().has_successful_fetch_for_current_location {
        assert!(Instant::now() < deadline, "ADS-B publication was blocked");
        std::thread::yield_now();
    }

    assert_eq!(model.snapshot().environment, None);
    adsb_commands.send(WorkerCommand::Stop).expect("stop ADS-B");
    weather_release_sender.send(()).expect("release weather");
    let _ = weather_commands.send(WorkerCommand::Stop);
    adsb.join().expect("ADS-B joins");
    weather.join().expect("weather joins");
}

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use planeradar::adsb::{AdsbClient, AltitudeFilter};
use planeradar::http::{HttpClient, HttpError, HttpRequest, HttpResponse};
use planeradar::model::{Aircraft, AppState, Location, RadarSettings, Units};
use planeradar::runtime::{
    AdsbWorker, ChannelWaiter, RuntimeConfig, RuntimeHealthSource, RuntimeModel,
    RuntimeSettingsService, WaitResult, Waiter, WorkerCommand,
};
use planeradar::settings::SettingsStore;
use planeradar::time::Clock;
use planeradar::web::{HealthSource, SettingsService};

fn configured() -> RadarSettings {
    RadarSettings {
        location: Some(Location {
            latitude: 40.7,
            longitude: -74.0,
            label: "private place".to_owned(),
        }),
        ..RadarSettings::default()
    }
}

fn aircraft() -> Aircraft {
    Aircraft {
        hex: String::new(),
        flight_callsign: String::new(),
        latitude: 40.8,
        longitude: -74.1,
        nose_degrees: 0.0,
        track_degrees: 0.0,
        ground_speed_knots: 0.0,
        callsign: String::new(),
        aircraft_type: String::new(),
        altitude_feet: None,
        altitude: String::new(),
    }
}

fn aircraft_at(hex: &str, altitude_feet: Option<i32>) -> Aircraft {
    Aircraft {
        hex: hex.to_owned(),
        flight_callsign: hex.to_owned(),
        callsign: hex.to_owned(),
        altitude: altitude_feet
            .map(|altitude| format!("{altitude} ft"))
            .unwrap_or_default(),
        altitude_feet,
        ..aircraft()
    }
}

#[test]
fn snapshot_updates_are_immutable_and_generation_is_monotonic() {
    let model = RuntimeModel::new(
        RadarSettings::default(),
        "http://planeradar.local".to_owned(),
    );
    let before = model.snapshot();
    assert_eq!(before.generation, 0);
    model.replace_settings(configured());
    model.record_aircraft(vec![aircraft()], Duration::from_secs(10));
    let after = model.snapshot();

    assert!(before.settings.location.is_none());
    assert!(before.aircraft.is_empty());
    assert_eq!(after.aircraft.len(), 1);
    assert_eq!(after.generation, 2);
}

#[test]
fn health_has_only_runtime_status_and_marks_data_stale_at_thirty_seconds() {
    let model = RuntimeModel::new(configured(), "http://planeradar.local".to_owned());
    model.record_aircraft(vec![aircraft()], Duration::from_secs(10));
    let health = RuntimeHealthSource::new(model.clone(), Arc::new(|| Duration::from_secs(40)));
    let snapshot = health.health();

    assert!(snapshot.configured);
    assert_eq!(snapshot.state, AppState::Radar);
    assert!(snapshot.data_stale);
    let encoded = serde_json::to_string(&snapshot).expect("health JSON");
    assert!(!encoded.contains("latitude"));
    assert!(!encoded.contains("private place"));
}

#[test]
fn stale_boundary_is_false_before_thirty_seconds_and_true_at_thirty_seconds() {
    let model = RuntimeModel::new(configured(), "http://planeradar.local".to_owned());
    model.record_aircraft(vec![aircraft()], Duration::from_secs(10));
    let before =
        RuntimeHealthSource::new(model.clone(), Arc::new(|| Duration::from_millis(39_999)));
    let at = RuntimeHealthSource::new(model, Arc::new(|| Duration::from_secs(40)));
    assert!(!before.health().data_stale);
    assert!(at.health().data_stale);
}

#[test]
fn runtime_defaults_are_the_documented_production_values() {
    let config = RuntimeConfig::default();
    assert_eq!(
        config.settings_path,
        std::path::PathBuf::from("/var/lib/planeradar/settings.json")
    );
    assert_eq!(
        config.geocode_cache_path,
        std::path::PathBuf::from("/var/lib/planeradar/geocode-cache.json")
    );
    assert_eq!(config.http_address.to_string(), "0.0.0.0:80");
    assert_eq!(config.local_url, "http://planeradar.local");
    assert_eq!(
        config.nominatim_url,
        "https://nominatim.openstreetmap.org/search"
    );
}

#[test]
fn configured_without_a_successful_fetch_is_waiting_for_network() {
    let model = RuntimeModel::new(configured(), "http://planeradar.local".to_owned());
    let health = RuntimeHealthSource::new(model, Arc::new(|| Duration::ZERO));
    assert_eq!(health.health().state, AppState::WaitingForNetwork);
}

#[test]
fn settings_replace_is_persisted_then_published_and_failed_save_is_not_published() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let store = Arc::new(SettingsStore::new(directory.path().join("settings.json")));
    let model = RuntimeModel::new(
        RadarSettings::default(),
        "http://planeradar.local".to_owned(),
    );
    let (sender, _receiver) = mpsc::channel();
    let service = RuntimeSettingsService::new(model.clone(), store.clone(), sender);
    let generation = model.snapshot().generation;
    service.replace(configured()).expect("replace");
    assert_eq!(model.snapshot().settings, configured());
    assert_eq!(model.snapshot().generation, generation + 1);
    assert_eq!(store.load().expect("load"), configured());

    let blocked_parent = directory.path().join("not-a-directory");
    std::fs::write(&blocked_parent, b"blocked").expect("create blocker");
    let failed_store = Arc::new(SettingsStore::new(blocked_parent.join("settings.json")));
    let (sender, _receiver) = mpsc::channel();
    let failed = RuntimeSettingsService::new(model.clone(), failed_store, sender);
    let before = model.snapshot();
    assert!(failed.replace(RadarSettings::default()).is_err());
    assert_eq!(model.snapshot().generation, before.generation);
    assert_eq!(model.snapshot().settings, before.settings);
}

#[test]
fn range_change_drops_aircraft_fetched_for_the_old_query_coverage() {
    let model = RuntimeModel::new(configured(), "http://planeradar.local".to_owned());
    model.record_aircraft(vec![aircraft()], Duration::from_secs(10));
    let mut wider = configured();
    wider.range_index = 2;
    model.replace_settings(wider);
    let snapshot = model.snapshot();
    assert!(snapshot.aircraft.is_empty());
    assert_eq!(snapshot.fetched_at, None);
}

#[test]
fn non_location_settings_change_preserves_successful_fetch_state() {
    let model = RuntimeModel::new(configured(), "http://planeradar.local".to_owned());
    model.record_aircraft(vec![aircraft()], Duration::from_secs(10));
    let mut changed = configured();
    changed.units = Units::Miles;
    changed.show_runways = false;
    model.replace_settings(changed);

    let health = RuntimeHealthSource::new(model, Arc::new(|| Duration::from_secs(11)));
    assert_eq!(health.health().state, AppState::Radar);
}

#[test]
fn enabling_or_tightening_altitude_bounds_immediately_removes_disallowed_aircraft() {
    let model = RuntimeModel::new(configured(), "http://planeradar.local".to_owned());
    model.record_aircraft(
        vec![
            aircraft_at("low", Some(5_000)),
            aircraft_at("mid", Some(15_000)),
            aircraft_at("high", Some(35_000)),
            aircraft_at("unknown", None),
        ],
        Duration::from_secs(10),
    );

    let mut bounded = configured();
    bounded.minimum_altitude_feet = Some(10_000);
    model.replace_settings(bounded.clone());
    let enabled = model.snapshot();
    assert_eq!(
        enabled
            .aircraft
            .iter()
            .map(|aircraft| aircraft.hex.as_str())
            .collect::<Vec<_>>(),
        ["mid", "high"]
    );
    assert_eq!(enabled.fetched_at, Some(Duration::from_secs(10)));

    bounded.maximum_altitude_feet = Some(20_000);
    model.replace_settings(bounded);
    let tightened = model.snapshot();
    assert_eq!(
        tightened
            .aircraft
            .iter()
            .map(|aircraft| aircraft.hex.as_str())
            .collect::<Vec<_>>(),
        ["mid"]
    );
    assert_eq!(tightened.fetched_at, Some(Duration::from_secs(10)));
}

#[test]
fn accepted_conditional_publication_records_success_for_the_current_location() {
    let settings = configured();
    let location = settings.location.clone().expect("location");
    let model = RuntimeModel::new(settings.clone(), "http://planeradar.local".to_owned());
    assert!(
        model
            .record_aircraft_if_query(
                &location,
                settings.range_index,
                AltitudeFilter::from(&settings),
                vec![aircraft()],
                Duration::from_secs(10),
            )
            .is_some()
    );

    let health = RuntimeHealthSource::new(model, Arc::new(|| Duration::from_secs(11)));
    assert_eq!(health.health().state, AppState::Radar);
}

#[test]
fn location_change_resets_success_and_stale_publication_cannot_restore_it() {
    let original = configured();
    let original_location = original.location.clone().expect("original location");
    let model = RuntimeModel::new(original, "http://planeradar.local".to_owned());
    model.record_aircraft(vec![aircraft()], Duration::from_secs(10));

    let mut moved = configured();
    moved.location = Some(Location {
        latitude: 41.0,
        longitude: -73.0,
        label: "new place".to_owned(),
    });
    model.replace_settings(moved);
    let health = RuntimeHealthSource::new(model.clone(), Arc::new(|| Duration::from_secs(11)));
    assert_eq!(health.health().state, AppState::WaitingForNetwork);

    assert_eq!(
        model.record_aircraft_if_query(
            &original_location,
            1,
            AltitudeFilter::from(&configured()),
            vec![aircraft()],
            Duration::from_secs(12),
        ),
        None
    );
    let snapshot = model.snapshot();
    assert!(snapshot.aircraft.is_empty());
    assert_eq!(snapshot.fetched_at, None);
    assert_eq!(health.health().state, AppState::WaitingForNetwork);
}

#[test]
fn conditional_publication_rejects_a_superseded_query_under_the_model_lock() {
    let original = configured();
    let original_location = original.location.clone().expect("location");
    let model = RuntimeModel::new(original.clone(), "http://planeradar.local".to_owned());
    let mut wider = original;
    wider.range_index = 2;
    model.replace_settings(wider);

    assert_eq!(
        model.record_aircraft_if_query(
            &original_location,
            1,
            AltitudeFilter::from(&configured()),
            vec![aircraft()],
            Duration::from_secs(10),
        ),
        None
    );
    assert_eq!(
        model.record_adsb_error_if_query(
            &original_location,
            1,
            AltitudeFilter::from(&configured()),
            Duration::from_secs(11),
        ),
        None
    );
    let snapshot = model.snapshot();
    assert!(snapshot.aircraft.is_empty());
    assert_eq!(snapshot.fetched_at, None);
    assert_eq!(snapshot.last_error_at, None);
    let health = RuntimeHealthSource::new(model, Arc::new(|| Duration::from_secs(12)));
    assert_eq!(health.health().state, AppState::WaitingForNetwork);
}

#[test]
fn conditional_publication_rejects_results_and_errors_after_filter_changes() {
    let original = configured();
    let location = original.location.clone().expect("location");
    let expected_filter = AltitudeFilter::from(&original);
    let model = RuntimeModel::new(original.clone(), "http://planeradar.local".to_owned());
    let mut filtered = original;
    filtered.minimum_altitude_feet = Some(10_000);
    model.replace_settings(filtered);

    assert_eq!(
        model.record_aircraft_if_query(
            &location,
            1,
            expected_filter,
            vec![aircraft_at("stale", Some(5_000))],
            Duration::from_secs(10),
        ),
        None
    );
    assert_eq!(
        model.record_adsb_error_if_query(&location, 1, expected_filter, Duration::from_secs(11)),
        None
    );
    let snapshot = model.snapshot();
    assert!(snapshot.aircraft.is_empty());
    assert_eq!(snapshot.fetched_at, None);
    assert_eq!(snapshot.last_error_at, None);
}

#[derive(Clone, Default)]
struct TestClock(Arc<AtomicU64>);

impl Clock for TestClock {
    fn monotonic(&self) -> Duration {
        Duration::from_secs(self.0.load(Ordering::Acquire))
    }

    fn unix_seconds(&self) -> u64 {
        0
    }
}

#[derive(Clone)]
struct RecordingWaiter {
    clock: TestClock,
    waits: Arc<Mutex<Vec<Duration>>>,
    stop_after: usize,
    calls: Arc<AtomicU64>,
}

impl Waiter for RecordingWaiter {
    fn wait(
        &self,
        _commands: &Receiver<WorkerCommand>,
        stop: &AtomicBool,
        duration: Duration,
    ) -> WaitResult {
        self.waits.lock().expect("waits").push(duration);
        self.clock.0.fetch_add(duration.as_secs(), Ordering::AcqRel);
        if self.calls.fetch_add(1, Ordering::AcqRel) as usize + 1 >= self.stop_after {
            stop.store(true, Ordering::Release);
        }
        WaitResult::TimedOut
    }
}

#[derive(Clone)]
struct FakeHttp {
    results: Arc<Mutex<VecDeque<Result<HttpResponse, HttpError>>>>,
    requests: Arc<Mutex<Vec<Duration>>>,
    clock: TestClock,
}

impl HttpClient for FakeHttp {
    fn execute(&self, _request: HttpRequest) -> Result<HttpResponse, HttpError> {
        self.requests
            .lock()
            .expect("requests")
            .push(self.clock.monotonic());
        self.results
            .lock()
            .expect("results")
            .pop_front()
            .expect("result")
    }
}

struct WorkerFixture {
    worker: AdsbWorker<FakeHttp, TestClock, RecordingWaiter>,
    commands: Receiver<WorkerCommand>,
    stop: Arc<AtomicBool>,
    waits: Arc<Mutex<Vec<Duration>>>,
    requests: Arc<Mutex<Vec<Duration>>>,
}

fn worker_fixture(
    results: impl IntoIterator<Item = Result<HttpResponse, HttpError>>,
    stop_after: usize,
) -> WorkerFixture {
    let clock = TestClock::default();
    let waits = Arc::new(Mutex::new(Vec::new()));
    let requests = Arc::new(Mutex::new(Vec::new()));
    let http = FakeHttp {
        results: Arc::new(Mutex::new(results.into_iter().collect())),
        requests: requests.clone(),
        clock: clock.clone(),
    };
    let waiter = RecordingWaiter {
        clock: clock.clone(),
        waits: waits.clone(),
        stop_after,
        calls: Arc::new(AtomicU64::new(0)),
    };
    let model = RuntimeModel::new(configured(), "http://planeradar.local".to_owned());
    let (_sender, receiver) = mpsc::channel();
    WorkerFixture {
        worker: AdsbWorker::new(AdsbClient::new(http), model, clock, waiter),
        commands: receiver,
        stop: Arc::new(AtomicBool::new(false)),
        waits,
        requests,
    }
}

fn ok() -> Result<HttpResponse, HttpError> {
    Ok(HttpResponse {
        status: 200,
        body: br#"{"ac":[]}"#.to_vec(),
    })
}

#[test]
fn worker_spaces_successful_request_starts_by_three_seconds() {
    let fixture = worker_fixture([ok(), ok()], 2);
    fixture.worker.run(fixture.commands, fixture.stop);
    assert_eq!(
        *fixture.requests.lock().expect("requests"),
        [Duration::ZERO, Duration::from_secs(3)]
    );
    assert_eq!(
        *fixture.waits.lock().expect("waits"),
        [Duration::from_secs(3), Duration::from_secs(3)]
    );
}

#[test]
fn worker_backs_off_failures_and_resets_after_success() {
    let fixture = worker_fixture(
        [
            Err(HttpError::Timeout),
            Err(HttpError::Timeout),
            Err(HttpError::Timeout),
            Err(HttpError::Timeout),
            Err(HttpError::Timeout),
            Err(HttpError::Timeout),
            ok(),
        ],
        7,
    );
    fixture.worker.run(fixture.commands, fixture.stop);
    assert_eq!(
        *fixture.waits.lock().expect("waits"),
        [3, 6, 12, 24, 30, 30, 3].map(Duration::from_secs)
    );
}

#[test]
fn worker_does_not_fetch_without_a_location_and_stop_command_is_immediate() {
    let clock = TestClock::default();
    let waits = Arc::new(Mutex::new(Vec::new()));
    let http = FakeHttp {
        results: Arc::new(Mutex::new(VecDeque::new())),
        requests: Arc::new(Mutex::new(Vec::new())),
        clock: clock.clone(),
    };
    let waiter = RecordingWaiter {
        clock: clock.clone(),
        waits: waits.clone(),
        stop_after: 1,
        calls: Arc::new(AtomicU64::new(0)),
    };
    let model = RuntimeModel::new(
        RadarSettings::default(),
        "http://planeradar.local".to_owned(),
    );
    let (sender, receiver) = mpsc::channel();
    sender.send(WorkerCommand::Stop).expect("stop");
    AdsbWorker::new(AdsbClient::new(http.clone()), model, clock, waiter)
        .run(receiver, Arc::new(AtomicBool::new(false)));
    assert!(http.requests.lock().expect("requests").is_empty());
    assert!(waits.lock().expect("waits").is_empty());
}

#[test]
fn settings_changed_wakes_an_idle_channel_wait_without_waiting_for_backoff() {
    let (sender, receiver) = mpsc::channel();
    sender
        .send(WorkerCommand::SettingsChanged(configured()))
        .expect("settings change");
    assert!(matches!(
        ChannelWaiter.wait(&receiver, &AtomicBool::new(false), Duration::from_secs(30)),
        WaitResult::Command(WorkerCommand::SettingsChanged(_))
    ));
}

struct ChangingHttp {
    model: RuntimeModel,
    commands: mpsc::Sender<WorkerCommand>,
    calls: AtomicU64,
}

impl HttpClient for ChangingHttp {
    fn execute(&self, _request: HttpRequest) -> Result<HttpResponse, HttpError> {
        if self.calls.fetch_add(1, Ordering::AcqRel) == 0 {
            let mut changed = configured();
            changed.location = Some(Location {
                latitude: 41.0,
                longitude: -73.0,
                label: String::new(),
            });
            self.model.replace_settings(changed.clone());
            self.commands
                .send(WorkerCommand::SettingsChanged(changed))
                .expect("change settings");
        }
        Ok(HttpResponse {
            status: 200,
            body: br#"{"ac":[]}"#.to_vec(),
        })
    }
}

#[test]
fn worker_discards_an_in_flight_result_when_location_changes() {
    let model = RuntimeModel::new(configured(), "http://planeradar.local".to_owned());
    let (sender, receiver) = mpsc::channel();
    let clock = TestClock::default();
    let waiter = RecordingWaiter {
        clock,
        waits: Arc::new(Mutex::new(Vec::new())),
        stop_after: 1,
        calls: Arc::new(AtomicU64::new(0)),
    };
    let stop = Arc::new(AtomicBool::new(false));
    AdsbWorker::new(
        AdsbClient::new(ChangingHttp {
            model: model.clone(),
            commands: sender,
            calls: AtomicU64::new(0),
        }),
        model.clone(),
        TestClock::default(),
        waiter,
    )
    .run(receiver, stop);
    let snapshot = model.snapshot();
    assert!(snapshot.aircraft.is_empty());
    assert!(snapshot.fetched_at.is_some());
}

struct FailingChangingHttp {
    model: RuntimeModel,
    commands: mpsc::Sender<WorkerCommand>,
}

impl HttpClient for FailingChangingHttp {
    fn execute(&self, _request: HttpRequest) -> Result<HttpResponse, HttpError> {
        let mut changed = configured();
        changed.range_index = 2;
        self.model.replace_settings(changed);
        self.commands.send(WorkerCommand::Stop).expect("stop");
        Err(HttpError::Timeout)
    }
}

#[test]
fn worker_discards_an_in_flight_error_when_range_coverage_changes() {
    let model = RuntimeModel::new(configured(), "http://planeradar.local".to_owned());
    let (sender, receiver) = mpsc::channel();
    let clock = TestClock::default();
    let waiter = RecordingWaiter {
        clock,
        waits: Arc::new(Mutex::new(Vec::new())),
        stop_after: 1,
        calls: Arc::new(AtomicU64::new(0)),
    };
    AdsbWorker::new(
        AdsbClient::new(FailingChangingHttp {
            model: model.clone(),
            commands: sender,
        }),
        model.clone(),
        TestClock::default(),
        waiter,
    )
    .run(receiver, Arc::new(AtomicBool::new(false)));
    assert_eq!(model.snapshot().last_error_at, None);
}

#[test]
fn transient_worker_failure_retains_the_last_good_aircraft_snapshot() {
    let model = RuntimeModel::new(configured(), "http://planeradar.local".to_owned());
    model.record_aircraft(vec![aircraft()], Duration::ZERO);
    let clock = TestClock::default();
    let http = FakeHttp {
        results: Arc::new(Mutex::new([Err(HttpError::Timeout)].into())),
        requests: Arc::new(Mutex::new(Vec::new())),
        clock: clock.clone(),
    };
    let waiter = RecordingWaiter {
        clock: clock.clone(),
        waits: Arc::new(Mutex::new(Vec::new())),
        stop_after: 1,
        calls: Arc::new(AtomicU64::new(0)),
    };
    let (_sender, receiver) = mpsc::channel();
    AdsbWorker::new(AdsbClient::new(http), model.clone(), clock, waiter)
        .run(receiver, Arc::new(AtomicBool::new(false)));
    assert_eq!(model.snapshot().aircraft.as_ref(), [aircraft()]);
}

struct ChangingOnceHttp {
    model: RuntimeModel,
    commands: mpsc::Sender<WorkerCommand>,
    requests: Arc<Mutex<Vec<Duration>>>,
    clock: TestClock,
    calls: AtomicU64,
}

impl HttpClient for ChangingOnceHttp {
    fn execute(&self, _request: HttpRequest) -> Result<HttpResponse, HttpError> {
        self.requests
            .lock()
            .expect("requests")
            .push(self.clock.monotonic());
        if self.calls.fetch_add(1, Ordering::AcqRel) == 0 {
            let mut changed = configured();
            changed.location = Some(Location {
                latitude: 41.0,
                longitude: -73.0,
                label: String::new(),
            });
            self.model.replace_settings(changed.clone());
            self.commands
                .send(WorkerCommand::SettingsChanged(changed))
                .expect("settings change");
        }
        Ok(HttpResponse {
            status: 200,
            body: br#"{"ac":[]}"#.to_vec(),
        })
    }
}

#[test]
fn in_flight_settings_change_starts_the_new_center_request_without_backoff_delay() {
    let model = RuntimeModel::new(configured(), "http://planeradar.local".to_owned());
    let (sender, receiver) = mpsc::channel();
    let clock = TestClock::default();
    let requests = Arc::new(Mutex::new(Vec::new()));
    let waiter = RecordingWaiter {
        clock: clock.clone(),
        waits: Arc::new(Mutex::new(Vec::new())),
        stop_after: 1,
        calls: Arc::new(AtomicU64::new(0)),
    };
    let http = ChangingOnceHttp {
        model: model.clone(),
        commands: sender,
        requests: requests.clone(),
        clock: clock.clone(),
        calls: AtomicU64::new(0),
    };
    AdsbWorker::new(AdsbClient::new(http), model, clock, waiter)
        .run(receiver, Arc::new(AtomicBool::new(false)));
    assert_eq!(
        *requests.lock().expect("requests"),
        [Duration::ZERO, Duration::ZERO]
    );
}

struct FailingChangingOnceHttp {
    model: RuntimeModel,
    commands: mpsc::Sender<WorkerCommand>,
    calls: AtomicU64,
}

impl HttpClient for FailingChangingOnceHttp {
    fn execute(&self, _request: HttpRequest) -> Result<HttpResponse, HttpError> {
        if self.calls.fetch_add(1, Ordering::AcqRel) == 0 {
            let mut changed = configured();
            changed.range_index = 2;
            self.model.replace_settings(changed.clone());
            self.commands
                .send(WorkerCommand::SettingsChanged(changed))
                .expect("settings change");
        }
        Err(HttpError::Timeout)
    }
}

#[test]
fn superseded_error_does_not_increase_the_new_query_backoff() {
    let model = RuntimeModel::new(configured(), "http://planeradar.local".to_owned());
    let (sender, receiver) = mpsc::channel();
    let clock = TestClock::default();
    let waits = Arc::new(Mutex::new(Vec::new()));
    let waiter = RecordingWaiter {
        clock: clock.clone(),
        waits: waits.clone(),
        stop_after: 1,
        calls: Arc::new(AtomicU64::new(0)),
    };
    let http = FailingChangingOnceHttp {
        model: model.clone(),
        commands: sender,
        calls: AtomicU64::new(0),
    };
    AdsbWorker::new(AdsbClient::new(http), model, clock, waiter)
        .run(receiver, Arc::new(AtomicBool::new(false)));
    assert_eq!(*waits.lock().expect("waits"), [Duration::from_secs(3)]);
}

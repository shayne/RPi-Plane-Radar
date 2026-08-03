use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver};
use std::sync::{Arc, Condvar, Mutex};
use std::thread;
use std::time::Duration;

use planeradar::adsb::AdsbClient;
use planeradar::http::{HttpClient, HttpError, HttpRequest, HttpResponse};
use planeradar::model::{EnvironmentReading, Location, RadarSettings};
use planeradar::runtime::{
    AdsbWorker, RuntimeModel, SolarWorker, WaitResult, Waiter, WorkerCommand,
};
use planeradar::solar::{SolarClient, SolarErrorCategory, SolarSchedule, load_cache, save_cache};
use planeradar::time::Clock;

const START_UNIX: u64 = 1_785_700_000;
const SOLAR_URL: &str = "https://api.open-meteo.test/v1/forecast";

#[derive(Clone)]
struct FakeClock {
    monotonic_seconds: Arc<AtomicU64>,
    unix_seconds: Arc<AtomicU64>,
}

impl Default for FakeClock {
    fn default() -> Self {
        Self {
            monotonic_seconds: Arc::new(AtomicU64::new(0)),
            unix_seconds: Arc::new(AtomicU64::new(START_UNIX)),
        }
    }
}

impl FakeClock {
    fn advance(&self, duration: Duration) {
        self.monotonic_seconds
            .fetch_add(duration.as_secs(), Ordering::AcqRel);
        self.unix_seconds
            .fetch_add(duration.as_secs(), Ordering::AcqRel);
    }

    fn set_unix(&self, unix_seconds: u64) {
        self.unix_seconds.store(unix_seconds, Ordering::Release);
    }
}

impl Clock for FakeClock {
    fn monotonic(&self) -> Duration {
        Duration::from_secs(self.monotonic_seconds.load(Ordering::Acquire))
    }

    fn unix_seconds(&self) -> u64 {
        self.unix_seconds.load(Ordering::Acquire)
    }
}

enum WaitOutcome {
    TimedOut,
    Action(Box<dyn FnOnce() + Send>),
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
    requests: Arc<Mutex<Vec<HttpRequest>>>,
    responses: Arc<Mutex<VecDeque<Result<HttpResponse, HttpError>>>>,
    actions: Arc<Mutex<VecDeque<HttpAction>>>,
}

impl FakeHttp {
    fn new(
        responses: impl IntoIterator<Item = Result<HttpResponse, HttpError>>,
    ) -> (Self, Arc<Mutex<Vec<HttpRequest>>>) {
        let requests = Arc::new(Mutex::new(Vec::new()));
        (
            Self {
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
        self.requests.lock().expect("requests").push(request);
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

fn location(latitude: f64, longitude: f64, label: &str) -> Location {
    Location {
        latitude,
        longitude,
        label: label.to_owned(),
    }
}

fn fixture_location() -> Location {
    location(40.7769, -73.8740, "LaGuardia")
}

fn enabled(location: Option<Location>) -> RadarSettings {
    let mut settings = RadarSettings {
        location,
        ..RadarSettings::default()
    };
    settings.brightness.night.enabled = true;
    settings
}

fn solar_response() -> Result<HttpResponse, HttpError> {
    Ok(HttpResponse {
        status: 200,
        body: include_bytes!("fixtures/open_meteo/solar.json").to_vec(),
    })
}

fn adsb_response() -> Result<HttpResponse, HttpError> {
    Ok(HttpResponse {
        status: 200,
        body: br#"{"ac":[]}"#.to_vec(),
    })
}

fn schedule_for(location: &Location, fetched_at_unix: u64) -> SolarSchedule {
    let (http, _) = FakeHttp::new([solar_response()]);
    SolarClient::with_provider_base(http, SOLAR_URL.to_owned())
        .fetch(location, fetched_at_unix)
        .expect("valid solar fixture")
}

fn run_worker<W: Waiter>(
    http: FakeHttp,
    cache_path: PathBuf,
    model: RuntimeModel,
    clock: FakeClock,
    waiter: W,
) {
    let (_sender, receiver) = mpsc::channel();
    SolarWorker::new(
        SolarClient::with_provider_base(http, SOLAR_URL.to_owned()),
        cache_path,
        model,
        clock,
        waiter,
    )
    .run(receiver, Arc::new(AtomicBool::new(false)));
}

#[test]
fn disabled_or_unlocated_night_mode_idles_without_a_solar_request() {
    for settings in [RadarSettings::default(), enabled(None)] {
        let temporary = tempfile::tempdir().expect("state directory");
        let clock = FakeClock::default();
        let (http, requests) = FakeHttp::new([]);
        let (waiter, waits) = ScriptedWaiter::new(clock.clone(), [WaitOutcome::Stop]);

        run_worker(
            http,
            temporary.path().join("solar.json"),
            RuntimeModel::new(settings, "http://planeradar.local".to_owned()),
            clock,
            waiter,
        );

        assert!(requests.lock().expect("requests").is_empty());
        assert_eq!(*waits.lock().expect("waits"), [Duration::from_secs(30)]);
    }
}

#[test]
fn matching_cache_is_published_before_the_first_refresh_request() {
    let temporary = tempfile::tempdir().expect("state directory");
    let cache_path = temporary.path().join("solar.json");
    let location = fixture_location();
    let cached = schedule_for(&location, START_UNIX - 3_600);
    save_cache(&cache_path, &cached).expect("cache");
    let model = RuntimeModel::new(
        enabled(Some(location)),
        "http://planeradar.local".to_owned(),
    );
    let model_at_request = model.clone();
    let (http, requests) = FakeHttp::new([solar_response()]);
    let http = http.with_action(move || {
        assert_eq!(
            model_at_request.snapshot().solar_schedule.as_deref(),
            Some(&cached)
        );
    });
    let clock = FakeClock::default();
    let (waiter, _) = ScriptedWaiter::new(clock.clone(), [WaitOutcome::Stop]);

    run_worker(http, cache_path, model.clone(), clock, waiter);

    assert_eq!(requests.lock().expect("requests").len(), 1);
    assert!(model.snapshot().solar_schedule.is_some());
}

#[test]
fn successful_refresh_runs_at_most_once_per_radar_local_day() {
    let temporary = tempfile::tempdir().expect("state directory");
    let model = RuntimeModel::new(
        enabled(Some(fixture_location())),
        "http://planeradar.local".to_owned(),
    );
    let clock = FakeClock::default();
    let same_day_clock = clock.clone();
    let next_day_clock = clock.clone();
    let (waiter, waits) = ScriptedWaiter::new(
        clock.clone(),
        [
            WaitOutcome::Action(Box::new(move || {
                same_day_clock.set_unix(START_UNIX + 60 * 60);
            })),
            WaitOutcome::Action(Box::new(move || {
                next_day_clock.set_unix(START_UNIX + 24 * 60 * 60);
            })),
            WaitOutcome::Stop,
        ],
    );
    let (http, requests) = FakeHttp::new([solar_response(), solar_response()]);

    run_worker(
        http,
        temporary.path().join("solar.json"),
        model,
        clock,
        waiter,
    );

    assert_eq!(requests.lock().expect("requests").len(), 2);
    assert_eq!(
        *waits.lock().expect("waits"),
        [Duration::from_secs(15 * 60); 3]
    );
}

#[test]
fn clock_rollback_does_not_refetch_a_previously_successful_local_day() {
    let temporary = tempfile::tempdir().expect("state directory");
    let model = RuntimeModel::new(
        enabled(Some(fixture_location())),
        "http://planeradar.local".to_owned(),
    );
    let clock = FakeClock::default();
    let next_day_clock = clock.clone();
    let previous_day_clock = clock.clone();
    let (waiter, waits) = ScriptedWaiter::new(
        clock.clone(),
        [
            WaitOutcome::Action(Box::new(move || {
                next_day_clock.set_unix(START_UNIX + 24 * 60 * 60);
            })),
            WaitOutcome::Action(Box::new(move || {
                previous_day_clock.set_unix(START_UNIX);
            })),
            WaitOutcome::Stop,
        ],
    );
    let (http, requests) = FakeHttp::new([solar_response(), solar_response()]);

    run_worker(
        http,
        temporary.path().join("solar.json"),
        model,
        clock,
        waiter,
    );

    assert_eq!(requests.lock().expect("requests").len(), 2);
    assert_eq!(
        *waits.lock().expect("waits"),
        [Duration::from_secs(15 * 60); 3]
    );
}

#[test]
fn successful_fetch_at_an_unrepresentable_wall_time_uses_the_bounded_success_wait() {
    let temporary = tempfile::tempdir().expect("state directory");
    let model = RuntimeModel::new(
        enabled(Some(fixture_location())),
        "http://planeradar.local".to_owned(),
    );
    let clock = FakeClock::default();
    clock.set_unix(u64::MAX);
    let (waiter, waits) = ScriptedWaiter::new(clock.clone(), [WaitOutcome::Stop]);
    let (http, requests) = FakeHttp::new([solar_response()]);

    run_worker(
        http,
        temporary.path().join("solar.json"),
        model,
        clock,
        waiter,
    );

    assert_eq!(requests.lock().expect("requests").len(), 1);
    assert_eq!(
        *waits.lock().expect("waits"),
        [Duration::from_secs(15 * 60)]
    );
}

#[test]
fn failures_publish_sanitized_status_and_follow_the_capped_retry_schedule() {
    let temporary = tempfile::tempdir().expect("state directory");
    let model = RuntimeModel::new(
        enabled(Some(fixture_location())),
        "http://planeradar.local".to_owned(),
    );
    let clock = FakeClock::default();
    let (waiter, waits) = ScriptedWaiter::new(
        clock.clone(),
        [
            WaitOutcome::TimedOut,
            WaitOutcome::TimedOut,
            WaitOutcome::TimedOut,
            WaitOutcome::Stop,
        ],
    );
    let (http, _) = FakeHttp::new([
        Err(HttpError::Timeout),
        Err(HttpError::Timeout),
        Err(HttpError::Timeout),
        Err(HttpError::Timeout),
    ]);

    run_worker(
        http,
        temporary.path().join("solar.json"),
        model.clone(),
        clock,
        waiter,
    );

    assert_eq!(
        *waits.lock().expect("waits"),
        [30, 60, 5 * 60, 15 * 60].map(Duration::from_secs)
    );
    let failure = model.snapshot().solar_last_error.expect("solar failure");
    assert_eq!(failure.category, SolarErrorCategory::Timeout);
    assert_eq!(failure.at, Duration::from_secs(30 + 60 + 5 * 60));
}

#[test]
fn settings_changes_interrupt_failure_waits_without_skipping_the_next_backoff() {
    let temporary = tempfile::tempdir().expect("state directory");
    let original = fixture_location();
    let model = RuntimeModel::new(
        enabled(Some(original.clone())),
        "http://planeradar.local".to_owned(),
    );
    let rename_model = model.clone();
    let renamed = location(original.latitude, original.longitude, "Renamed");
    let clock = FakeClock::default();
    let (waiter, waits) = ScriptedWaiter::new(
        clock.clone(),
        [
            WaitOutcome::Action(Box::new(move || {
                rename_model.replace_settings(enabled(Some(renamed)));
            })),
            WaitOutcome::Stop,
        ],
    );
    let (http, requests) = FakeHttp::new([Err(HttpError::Timeout), Err(HttpError::Timeout)]);

    run_worker(
        http,
        temporary.path().join("solar.json"),
        model,
        clock,
        waiter,
    );

    assert_eq!(requests.lock().expect("requests").len(), 2);
    assert_eq!(
        *waits.lock().expect("waits"),
        [Duration::from_secs(30), Duration::from_secs(60)]
    );
}

#[test]
fn label_changes_keep_results_while_coordinate_changes_clear_and_refetch() {
    let temporary = tempfile::tempdir().expect("state directory");
    let cache_path = temporary.path().join("solar.json");
    let original = fixture_location();
    let moved = location(41.0, -73.0, "Moved");
    let model = RuntimeModel::new(
        enabled(Some(original.clone())),
        "http://planeradar.local".to_owned(),
    );

    let renamed_model = model.clone();
    let renamed = location(original.latitude, original.longitude, "Renamed only");
    let moved_model = model.clone();
    let moved_for_action = moved.clone();
    let (http, requests) = FakeHttp::new([solar_response(), solar_response()]);
    let http = http
        .with_action(move || {
            renamed_model.replace_settings(enabled(Some(renamed)));
        })
        .with_action(move || {
            moved_model.replace_settings(enabled(Some(moved_for_action)));
        });
    let clock = FakeClock::default();
    let move_model = model.clone();
    let move_location = moved.clone();
    let (waiter, _) = ScriptedWaiter::new(
        clock.clone(),
        [
            WaitOutcome::Action(Box::new(move || {
                move_model.replace_settings(enabled(Some(move_location)));
            })),
            WaitOutcome::Stop,
        ],
    );

    run_worker(http, cache_path.clone(), model.clone(), clock, waiter);

    let requests = requests.lock().expect("requests");
    assert_eq!(requests.len(), 2);
    assert!(
        requests[0]
            .query
            .contains(&("latitude".to_owned(), "40.7769".to_owned()))
    );
    assert!(
        requests[1]
            .query
            .contains(&("latitude".to_owned(), "41.0000".to_owned()))
    );
    drop(requests);
    let snapshot = model.snapshot();
    let schedule = snapshot.solar_schedule.expect("moved schedule");
    assert_eq!((schedule.latitude, schedule.longitude), (41.0, -73.0));
    assert_eq!(
        load_cache(&cache_path, &moved).map(|schedule| (schedule.latitude, schedule.longitude)),
        Some((41.0, -73.0))
    );
}

#[test]
fn disabling_solar_clears_only_solar_state_and_interrupts_the_wait() {
    let temporary = tempfile::tempdir().expect("state directory");
    let location = fixture_location();
    let model = RuntimeModel::new(
        enabled(Some(location.clone())),
        "http://planeradar.local".to_owned(),
    );
    let reading = EnvironmentReading {
        temperature_celsius: 22.0,
        humidity_percent: 50,
        weather_code: 1,
        utc_offset_seconds: -14_400,
        fetched_at: Duration::from_secs(5),
    };
    model.record_environment_if_location(&location, reading.clone());
    let disable_model = model.clone();
    let mut disabled = enabled(Some(location));
    disabled.brightness.night.enabled = false;
    let clock = FakeClock::default();
    let (waiter, waits) = ScriptedWaiter::new(
        clock.clone(),
        [
            WaitOutcome::Action(Box::new(move || {
                disable_model.replace_settings(disabled);
            })),
            WaitOutcome::Stop,
        ],
    );
    let (http, _) = FakeHttp::new([solar_response()]);

    run_worker(
        http,
        temporary.path().join("solar.json"),
        model.clone(),
        clock,
        waiter,
    );

    let snapshot = model.snapshot();
    assert_eq!(snapshot.solar_schedule, None);
    assert_eq!(snapshot.solar_last_error, None);
    assert_eq!(snapshot.environment, Some(reading));
    assert_eq!(
        *waits.lock().expect("waits"),
        [Duration::from_secs(15 * 60), Duration::from_secs(30)]
    );
}

#[test]
fn disable_and_reenable_at_the_same_coordinates_does_not_refetch_that_local_day() {
    let temporary = tempfile::tempdir().expect("state directory");
    let location = fixture_location();
    let model = RuntimeModel::new(
        enabled(Some(location.clone())),
        "http://planeradar.local".to_owned(),
    );
    let disable_model = model.clone();
    let mut disabled = enabled(Some(location.clone()));
    disabled.brightness.night.enabled = false;
    let enable_model = model.clone();
    let reenabled = enabled(Some(location));
    let clock = FakeClock::default();
    let (waiter, waits) = ScriptedWaiter::new(
        clock.clone(),
        [
            WaitOutcome::Action(Box::new(move || {
                disable_model.replace_settings(disabled);
            })),
            WaitOutcome::Action(Box::new(move || {
                enable_model.replace_settings(reenabled);
            })),
            WaitOutcome::Stop,
        ],
    );
    let (http, requests) = FakeHttp::new([solar_response()]);

    run_worker(
        http,
        temporary.path().join("solar.json"),
        model,
        clock,
        waiter,
    );

    assert_eq!(requests.lock().expect("requests").len(), 1);
    assert_eq!(
        *waits.lock().expect("waits"),
        [
            Duration::from_secs(15 * 60),
            Duration::from_secs(30),
            Duration::from_secs(15 * 60),
        ]
    );
}

#[derive(Clone)]
struct BlockingHttp {
    state: Arc<(Mutex<(bool, bool)>, Condvar)>,
}

impl BlockingHttp {
    fn new() -> Self {
        Self {
            state: Arc::new((Mutex::new((false, false)), Condvar::new())),
        }
    }

    fn wait_until_entered(&self) {
        let (lock, wake) = &*self.state;
        let guard = lock.lock().expect("block state");
        drop(
            wake.wait_while(guard, |(entered, _)| !*entered)
                .expect("block wait"),
        );
    }

    fn release(&self) {
        let (lock, wake) = &*self.state;
        lock.lock().expect("block state").1 = true;
        wake.notify_all();
    }
}

impl HttpClient for BlockingHttp {
    fn execute(&self, _request: HttpRequest) -> Result<HttpResponse, HttpError> {
        let (lock, wake) = &*self.state;
        let mut guard = lock.lock().expect("block state");
        guard.0 = true;
        wake.notify_all();
        drop(
            wake.wait_while(guard, |(_, released)| !*released)
                .expect("block release"),
        );
        solar_response()
    }
}

#[derive(Clone, Copy)]
struct StopWaiter;

impl Waiter for StopWaiter {
    fn wait(
        &self,
        _commands: &Receiver<WorkerCommand>,
        stop: &AtomicBool,
        _duration: Duration,
    ) -> WaitResult {
        stop.store(true, Ordering::Release);
        WaitResult::Command(WorkerCommand::Stop)
    }
}

#[test]
fn a_blocked_solar_client_cannot_delay_the_adsb_worker() {
    let temporary = tempfile::tempdir().expect("state directory");
    let model = RuntimeModel::new(
        enabled(Some(fixture_location())),
        "http://planeradar.local".to_owned(),
    );
    let shared_stop = Arc::new(AtomicBool::new(false));
    let blocking = BlockingHttp::new();
    let solar_thread = {
        let blocking = blocking.clone();
        let model = model.clone();
        let stop = shared_stop.clone();
        let cache_path = temporary.path().join("solar.json");
        thread::spawn(move || {
            let (_sender, receiver) = mpsc::channel();
            SolarWorker::new(
                SolarClient::with_provider_base(blocking, SOLAR_URL.to_owned()),
                cache_path,
                model,
                FakeClock::default(),
                StopWaiter,
            )
            .run(receiver, stop);
        })
    };
    blocking.wait_until_entered();

    let (adsb_http, adsb_requests) = FakeHttp::new([adsb_response()]);
    let adsb_thread = {
        let model = model.clone();
        let stop = shared_stop.clone();
        thread::spawn(move || {
            let (_sender, receiver) = mpsc::channel();
            AdsbWorker::new(
                AdsbClient::new(adsb_http),
                model,
                FakeClock::default(),
                StopWaiter,
            )
            .run(receiver, stop);
        })
    };
    adsb_thread.join().expect("ADSB worker");
    assert_eq!(adsb_requests.lock().expect("ADSB requests").len(), 1);

    blocking.release();
    solar_thread.join().expect("solar worker");
}

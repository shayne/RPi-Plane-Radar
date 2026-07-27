use std::fs;
use std::io::BufReader;
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use nix::sys::signal::{Signal, kill};
use nix::unistd::Pid;
use planeradar::app::{AppRuntime, PlaneRadarApp};
use planeradar::display::{DisplayHandler, InputEvent};
use planeradar::model::{Aircraft, AppState, Location, RadarSettings};
use planeradar::range::next_range_index;
use planeradar::render::FontAsset;
use planeradar::render::radar::RadarRenderer;
use planeradar::render::setup::{CANONICAL_LOCAL_URL, SetupRenderer};
use planeradar::runtime::{RuntimeError, RuntimeHealthSource, RuntimeModel, RuntimeSnapshot};
use planeradar::settings::SettingsStore;
use planeradar::touch::Gesture;
use planeradar::web::HealthSource;

#[derive(Clone)]
struct FakeControl {
    model: RuntimeModel,
    store: Arc<SettingsStore>,
    now_ms: Arc<AtomicU64>,
    stop: Arc<AtomicBool>,
    shutdown: Arc<AtomicBool>,
}

struct FakeRuntime {
    control: FakeControl,
}

impl AppRuntime for FakeRuntime {
    fn snapshot(&self) -> RuntimeSnapshot {
        self.control.model.snapshot()
    }

    fn cycle_range(&self) -> Result<RadarSettings, RuntimeError> {
        let mut settings = self.control.model.snapshot().settings;
        settings.range_index = next_range_index(settings.range_index);
        self.control.store.save(&settings)?;
        self.control.model.replace_settings(settings.clone());
        Ok(settings)
    }

    fn monotonic(&self) -> Duration {
        Duration::from_millis(self.control.now_ms.load(Ordering::Acquire))
    }

    fn stop_requested(&self) -> bool {
        self.control.stop.load(Ordering::Acquire)
    }

    fn shutdown(self: Box<Self>) -> Result<(), RuntimeError> {
        self.control.shutdown.store(true, Ordering::Release);
        Ok(())
    }
}

fn configured() -> RadarSettings {
    RadarSettings {
        location: Some(Location {
            latitude: 40.7,
            longitude: -74.0,
            label: "test".to_owned(),
        }),
        ..RadarSettings::default()
    }
}

fn aircraft() -> Aircraft {
    Aircraft {
        latitude: 40.8,
        longitude: -74.1,
        nose_degrees: 90.0,
        track_degrees: 90.0,
        ground_speed_knots: 300.0,
        callsign: "TEST".to_owned(),
        aircraft_type: String::new(),
        altitude: "12000".to_owned(),
    }
}

fn app_fixture(
    settings: RadarSettings,
    fetched_at: Option<Duration>,
    debug_path: PathBuf,
) -> (PlaneRadarApp, FakeControl, Arc<AtomicBool>) {
    let settings_parent = debug_path
        .ancestors()
        .skip(1)
        .find(|path| path.exists())
        .expect("existing settings parent");
    let settings_name = format!(
        ".{}-settings.json",
        debug_path
            .file_stem()
            .and_then(|name| name.to_str())
            .unwrap_or("app")
    );
    let store = Arc::new(SettingsStore::new(settings_parent.join(settings_name)));
    store.save(&settings).expect("initial settings");
    let model = RuntimeModel::new(settings.clone(), "http://planeradar.local".to_owned());
    model.set_urls(
        "http://planeradar.local".to_owned(),
        Some("http://10.0.4.74".to_owned()),
    );
    if let Some(fetched_at) = fetched_at {
        model.record_aircraft(vec![aircraft()], fetched_at);
    }
    let control = FakeControl {
        model,
        store,
        now_ms: Arc::new(AtomicU64::new(0)),
        stop: Arc::new(AtomicBool::new(false)),
        shutdown: Arc::new(AtomicBool::new(false)),
    };
    let debug_requested = Arc::new(AtomicBool::new(false));
    let app = PlaneRadarApp::with_runtime(
        Box::new(FakeRuntime {
            control: control.clone(),
        }),
        RadarRenderer::new(FontAsset::embedded().expect("font")),
        SetupRenderer::new(FontAsset::embedded().expect("font")),
        Vec::new(),
        debug_path,
        debug_requested.clone(),
    );
    (app, control, debug_requested)
}

#[test]
fn missing_location_is_setup_required() {
    let directory = tempfile::tempdir().expect("tempdir");
    let (app, _, _) = app_fixture(
        RadarSettings::default(),
        None,
        directory.path().join("debug.png"),
    );
    assert_eq!(app.state(), AppState::SetupRequired);
}

#[test]
fn configured_without_data_is_waiting_for_network() {
    let directory = tempfile::tempdir().expect("tempdir");
    let (mut app, control, _) = app_fixture(configured(), None, directory.path().join("debug.png"));
    control.model.set_urls(CANONICAL_LOCAL_URL.to_owned(), None);
    app.step(&[], Instant::now());
    assert_eq!(app.state(), AppState::WaitingForNetwork);
}

#[test]
fn waiting_for_network_uses_the_exact_setup_copy_and_current_ip() {
    let directory = tempfile::tempdir().expect("tempdir");
    let (mut app, _, _) = app_fixture(configured(), None, directory.path().join("debug.png"));
    let actual = app.step(&[], Instant::now()).frame.expect("waiting frame");
    let expected = SetupRenderer::new(FontAsset::embedded().expect("font"))
        .render(
            CANONICAL_LOCAL_URL,
            Some("http://10.0.4.74"),
            true,
            "WAITING FOR NETWORK",
        )
        .expect("expected waiting frame");
    assert_eq!(actual, expected.pixels());
}

#[test]
fn first_valid_adsb_response_selects_radar() {
    let directory = tempfile::tempdir().expect("tempdir");
    let (mut app, control, _) = app_fixture(configured(), None, directory.path().join("debug.png"));
    control
        .model
        .record_aircraft(vec![aircraft()], Duration::ZERO);
    app.step(&[], Instant::now());
    assert_eq!(app.state(), AppState::Radar);
}

#[test]
fn tap_cycles_range_and_long_press_opens_settings() {
    let directory = tempfile::tempdir().expect("tempdir");
    let (mut app, control, _) = app_fixture(
        configured(),
        Some(Duration::ZERO),
        directory.path().join("debug.png"),
    );
    assert_eq!(app.state(), AppState::Radar);
    app.handle_gesture(Gesture::Tap).expect("tap");
    assert_eq!(app.settings().range_index, 2);
    assert_eq!(control.store.load().expect("reload").range_index, 2);
    app.handle_gesture(Gesture::LongPress).expect("hold");
    assert_eq!(app.state(), AppState::Settings);
}

#[test]
fn tap_in_configured_settings_returns_to_radar_without_another_range_change() {
    let directory = tempfile::tempdir().expect("tempdir");
    let (mut app, control, _) = app_fixture(
        configured(),
        Some(Duration::ZERO),
        directory.path().join("debug.png"),
    );
    app.handle_gesture(Gesture::LongPress).expect("hold");
    app.handle_gesture(Gesture::Tap).expect("tap");
    assert_eq!(app.state(), AppState::Radar);
    assert_eq!(control.store.load().expect("reload").range_index, 1);
}

#[test]
fn tap_in_setup_required_is_a_noop() {
    let directory = tempfile::tempdir().expect("tempdir");
    let (mut app, control, _) = app_fixture(
        RadarSettings::default(),
        None,
        directory.path().join("debug.png"),
    );
    app.handle_gesture(Gesture::Tap).expect("tap");
    assert_eq!(app.state(), AppState::SetupRequired);
    assert_eq!(
        control.store.load().expect("reload"),
        RadarSettings::default()
    );
}

#[test]
fn long_press_release_cannot_cycle_the_range() {
    let directory = tempfile::tempdir().expect("tempdir");
    let (mut app, control, _) = app_fixture(
        configured(),
        Some(Duration::ZERO),
        directory.path().join("debug.png"),
    );
    app.step(
        &[InputEvent::Pressed {
            pointer_id: 1,
            x: 240.0,
            y: 240.0,
        }],
        Instant::now(),
    );
    control.now_ms.store(3_000, Ordering::Release);
    app.step(&[], Instant::now());
    control.now_ms.store(3_100, Ordering::Release);
    app.step(
        &[InputEvent::Released {
            pointer_id: 1,
            x: 240.0,
            y: 240.0,
        }],
        Instant::now(),
    );
    assert_eq!(app.state(), AppState::Settings);
    assert_eq!(app.settings().range_index, 1);
}

#[test]
fn tap_range_change_keeps_radar_while_invalidating_the_old_poll_snapshot() {
    let directory = tempfile::tempdir().expect("tempdir");
    let (mut app, control, _) = app_fixture(
        configured(),
        Some(Duration::ZERO),
        directory.path().join("debug.png"),
    );
    app.handle_gesture(Gesture::Tap).expect("tap");
    let snapshot = control.model.snapshot();
    assert_eq!(snapshot.settings.range_index, 2);
    assert!(snapshot.aircraft.is_empty());
    assert_eq!(snapshot.fetched_at, None);

    let health = RuntimeHealthSource::new(control.model.clone(), Arc::new(|| Duration::ZERO));
    let rendered = app
        .step(&[], Instant::now())
        .frame
        .expect("range-refetch frame");
    let waiting = SetupRenderer::new(FontAsset::embedded().expect("font"))
        .render(
            CANONICAL_LOCAL_URL,
            Some("http://10.0.4.74"),
            true,
            "WAITING FOR NETWORK",
        )
        .expect("waiting frame");
    assert_eq!(
        (
            app.state(),
            health.health().state,
            rendered.as_slice() == waiting.pixels(),
        ),
        (AppState::Radar, AppState::Radar, false),
        "range-only tap rendered the waiting QR while refetching"
    );
}

#[test]
fn transient_adsb_error_and_loss_of_ip_retain_radar() {
    let directory = tempfile::tempdir().expect("tempdir");
    let (mut app, control, _) = app_fixture(
        configured(),
        Some(Duration::ZERO),
        directory.path().join("debug.png"),
    );
    control.model.record_adsb_error(Duration::from_secs(5));
    control
        .model
        .set_urls("http://planeradar.local".to_owned(), None);
    app.step(&[], Instant::now());
    assert_eq!(app.state(), AppState::Radar);
    assert_eq!(control.model.snapshot().aircraft.as_ref(), [aircraft()]);
}

#[test]
fn stale_boundary_draws_at_thirty_seconds_and_fresh_data_clears_it() {
    let directory = tempfile::tempdir().expect("tempdir");
    let (mut app, control, _) = app_fixture(
        configured(),
        Some(Duration::ZERO),
        directory.path().join("debug.png"),
    );
    control.now_ms.store(29_999, Ordering::Release);
    let fresh = app
        .step(&[], Instant::now())
        .frame
        .expect("initial fresh frame");
    control.now_ms.store(30_000, Ordering::Release);
    let stale = app
        .step(&[], Instant::now())
        .frame
        .expect("stale boundary frame");
    assert_ne!(fresh, stale);

    control
        .model
        .record_aircraft(vec![aircraft()], Duration::from_secs(30));
    let recovered = app
        .step(&[], Instant::now())
        .frame
        .expect("recovered frame");
    assert_eq!(recovered, fresh);
}

#[test]
fn one_debug_request_writes_one_valid_frame_and_failure_is_nonfatal() {
    let directory = tempfile::tempdir().expect("tempdir");
    let debug_path = directory.path().join("debug.png");
    let (mut app, _, debug_requested) =
        app_fixture(configured(), Some(Duration::ZERO), debug_path.clone());
    debug_requested.store(true, Ordering::Release);
    let first = app.step(&[], Instant::now());
    assert!(!first.exit);
    let decoder = png::Decoder::new(BufReader::new(
        fs::File::open(&debug_path).expect("debug png"),
    ));
    let reader = decoder.read_info().expect("PNG header");
    assert_eq!(reader.info().width, 480);
    assert_eq!(reader.info().height, 480);

    fs::write(&debug_path, b"sentinel").expect("replace with sentinel");
    app.step(&[], Instant::now());
    assert_eq!(fs::read(&debug_path).expect("sentinel"), b"sentinel");

    let missing_path = directory.path().join("missing").join("debug.png");
    let (mut failing_app, _, failing_request) =
        app_fixture(configured(), Some(Duration::ZERO), missing_path.clone());
    failing_request.store(true, Ordering::Release);
    assert!(!failing_app.step(&[], Instant::now()).exit);
    fs::create_dir(directory.path().join("missing")).expect("create parent");
    assert!(!failing_app.step(&[], Instant::now()).exit);
    assert!(!missing_path.exists());
    failing_request.store(true, Ordering::Release);
    assert!(!failing_app.step(&[], Instant::now()).exit);
    assert!(missing_path.exists());
}

#[test]
fn unchanged_runtime_content_does_not_render_another_frame() {
    let directory = tempfile::tempdir().expect("tempdir");
    let (mut app, control, _) = app_fixture(
        configured(),
        Some(Duration::ZERO),
        directory.path().join("debug.png"),
    );
    assert!(app.step(&[], Instant::now()).frame.is_some());
    assert!(app.step(&[], Instant::now()).frame.is_none());

    control.model.record_adsb_error(Duration::from_millis(1));
    assert!(app.step(&[], Instant::now()).frame.is_some());
    assert!(app.step(&[], Instant::now()).frame.is_none());
}

#[test]
fn stop_signal_and_quit_coordinate_shutdown_before_step_returns() {
    let directory = tempfile::tempdir().expect("tempdir");
    let (mut stopped_app, stopped_control, _) = app_fixture(
        configured(),
        Some(Duration::ZERO),
        directory.path().join("stop.png"),
    );
    stopped_control.stop.store(true, Ordering::Release);
    let update = stopped_app.step(&[], Instant::now());
    assert!(update.exit);
    assert!(stopped_control.shutdown.load(Ordering::Acquire));

    let (mut quit_app, quit_control, _) = app_fixture(
        configured(),
        Some(Duration::ZERO),
        directory.path().join("quit.png"),
    );
    let update = quit_app.step(&[InputEvent::Quit], Instant::now());
    assert!(update.exit);
    assert!(quit_control.shutdown.load(Ordering::Acquire));
}

#[test]
fn display_initialization_error_returns_nonzero() {
    let directory = tempfile::tempdir().expect("tempdir");
    let output = Command::new(env!("CARGO_BIN_EXE_planeradar"))
        .args([
            "run",
            "--settings",
            directory
                .path()
                .join("settings.json")
                .to_str()
                .expect("settings path"),
            "--geocode-cache",
            directory
                .path()
                .join("geocode.json")
                .to_str()
                .expect("cache path"),
            "--debug-frame",
            directory
                .path()
                .join("debug.png")
                .to_str()
                .expect("debug path"),
            "--http",
            "127.0.0.1:0",
        ])
        .env("SDL_VIDEODRIVER", "definitely-not-a-real-driver")
        .output()
        .expect("run planeradar");
    assert!(!output.status.success());
}

#[test]
fn sigterm_requests_coordinated_shutdown() {
    let directory = tempfile::tempdir().expect("tempdir");
    let listener = TcpListener::bind("127.0.0.1:0").expect("reserve address");
    let address = listener.local_addr().expect("listener address");
    drop(listener);
    let mut child = Command::new(env!("CARGO_BIN_EXE_planeradar"))
        .args([
            "run",
            "--headless",
            "--settings",
            directory
                .path()
                .join("settings.json")
                .to_str()
                .expect("settings path"),
            "--geocode-cache",
            directory
                .path()
                .join("geocode.json")
                .to_str()
                .expect("cache path"),
            "--http",
            &address.to_string(),
        ])
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .expect("start planeradar");
    let deadline = Instant::now() + Duration::from_secs(2);
    while TcpStream::connect(address).is_err() {
        assert!(child.try_wait().expect("poll child").is_none());
        assert!(Instant::now() < deadline, "HTTP listener did not start");
        thread::sleep(Duration::from_millis(10));
    }
    let pid = i32::try_from(child.id()).expect("process id");
    kill(Pid::from_raw(pid), Signal::SIGTERM).expect("send SIGTERM");
    let output = child.wait_with_output().expect("wait for planeradar");
    assert!(
        output.status.success(),
        "status: {:?}, stderr: {}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );
}

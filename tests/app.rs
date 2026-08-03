use std::fs;
use std::io::BufReader;
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use nix::sys::signal::{Signal, kill};
use nix::unistd::Pid;
use planeradar::app::{AppRuntime, PlaneRadarApp};
use planeradar::backlight::{Backlight, BacklightError};
use planeradar::display::{DisplayHandler, InputEvent};
use planeradar::model::{
    Aircraft, AppState, BacklightAvailability, EnvironmentReading, FrameColorMode, Location,
    RadarSettings, TimeZone,
};
use planeradar::range::next_range_index;
use planeradar::render::FontAsset;
use planeradar::render::radar::RadarRenderer;
use planeradar::render::setup::{CANONICAL_LOCAL_URL, SetupRenderer};
use planeradar::runtime::{RuntimeError, RuntimeHealthSource, RuntimeModel, RuntimeSnapshot};
use planeradar::settings::SettingsStore;
use planeradar::solar::{SolarDay, SolarSchedule};
use planeradar::touch::Gesture;
use planeradar::web::HealthSource;
use qrcode::QrCode;
use qrcode::types::{Color, EcLevel};

#[derive(Clone)]
struct FakeControl {
    model: RuntimeModel,
    store: Arc<SettingsStore>,
    now_ms: Arc<AtomicU64>,
    unix_seconds: Arc<AtomicU64>,
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

    fn unix_seconds(&self) -> u64 {
        self.control.unix_seconds.load(Ordering::Acquire)
    }

    fn stop_requested(&self) -> bool {
        self.control.stop.load(Ordering::Acquire)
    }

    fn record_backlight_availability(&self, availability: BacklightAvailability) {
        self.control
            .model
            .record_backlight_availability(availability);
    }

    fn shutdown(self: Box<Self>) -> Result<(), RuntimeError> {
        self.control.shutdown.store(true, Ordering::Release);
        Ok(())
    }
}

#[derive(Clone)]
struct RecordingBacklight {
    availability: BacklightAvailability,
    current: Arc<AtomicU64>,
    max_level: u32,
    writes: Arc<Mutex<Vec<u32>>>,
}

impl RecordingBacklight {
    fn available(current: u32, max_level: u32) -> Self {
        Self {
            availability: BacklightAvailability::Available,
            current: Arc::new(AtomicU64::new(u64::from(current))),
            max_level,
            writes: Arc::new(Mutex::new(Vec::new())),
        }
    }

    fn writes(&self) -> Vec<u32> {
        self.writes.lock().expect("recording backlight").clone()
    }
}

impl Backlight for RecordingBacklight {
    fn availability(&self) -> BacklightAvailability {
        self.availability
    }

    fn current_level(&mut self) -> Result<u32, BacklightError> {
        u32::try_from(self.current.load(Ordering::Acquire))
            .map_err(|_| BacklightError::InvalidValue)
    }

    fn max_level(&self) -> u32 {
        self.max_level
    }

    fn write_level(&mut self, level: u32) -> Result<(), BacklightError> {
        self.current.store(u64::from(level), Ordering::Release);
        self.writes.lock().expect("recording backlight").push(level);
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
        hex: "a00001".to_owned(),
        flight_callsign: "TEST".to_owned(),
        latitude: 40.8,
        longitude: -74.1,
        nose_degrees: 90.0,
        track_degrees: 90.0,
        ground_speed_knots: 300.0,
        callsign: "TEST".to_owned(),
        aircraft_type: String::new(),
        altitude_feet: Some(12_000),
        altitude: "12000".to_owned(),
    }
}

fn app_fixture(
    settings: RadarSettings,
    fetched_at: Option<Duration>,
    debug_path: PathBuf,
) -> (PlaneRadarApp, FakeControl, Arc<AtomicBool>) {
    app_fixture_with_backlight(
        settings,
        fetched_at,
        debug_path,
        Box::new(RecordingBacklight::available(100, 100)),
    )
}

fn app_fixture_with_backlight(
    settings: RadarSettings,
    fetched_at: Option<Duration>,
    debug_path: PathBuf,
    backlight: Box<dyn Backlight>,
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
        unix_seconds: Arc::new(AtomicU64::new(1_775_000_000)),
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
        backlight,
    );
    (app, control, debug_requested)
}

fn unix(value: &str) -> u64 {
    value
        .parse::<jiff::Timestamp>()
        .expect("UTC timestamp")
        .as_second()
        .try_into()
        .expect("positive timestamp")
}

fn solar_schedule(location: &Location) -> SolarSchedule {
    let zone = jiff::tz::TimeZone::get("America/New_York").expect("fixture zone");
    let mut date = "2026-07-28"
        .parse::<jiff::civil::Date>()
        .expect("fixture date");
    let mut days = Vec::with_capacity(17);
    for _ in 0..17 {
        days.push(SolarDay {
            date: date.to_string(),
            sunrise_unix: Some(
                zone.to_timestamp(date.at(6, 0, 0, 0))
                    .expect("fixture sunrise")
                    .as_second(),
            ),
        });
        date = date.tomorrow().expect("next fixture day");
    }
    SolarSchedule {
        schema_version: 1,
        latitude: location.latitude,
        longitude: location.longitude,
        time_zone: "America/New_York".to_owned(),
        fetched_at_unix: 0,
        days,
    }
}

fn red_night_settings() -> RadarSettings {
    let mut settings = configured();
    settings.brightness.night.enabled = true;
    settings.brightness.night.brightness_percent = 30;
    settings.brightness.night.start_hour = 20;
    settings.brightness.night.start_minute = 0;
    settings.brightness.night.red_mode = true;
    settings
}

fn decode_png(path: &std::path::Path) -> Vec<u8> {
    let decoder = png::Decoder::new(BufReader::new(fs::File::open(path).expect("debug PNG")));
    let mut reader = decoder.read_info().expect("debug PNG header");
    let mut bytes = vec![0; reader.output_buffer_size().expect("PNG buffer size")];
    let info = reader.next_frame(&mut bytes).expect("debug PNG frame");
    bytes.truncate(info.buffer_size());
    bytes
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
fn waiting_for_network_uses_the_runtime_local_url_and_current_ip() {
    let directory = tempfile::tempdir().expect("tempdir");
    let (mut app, control, _) = app_fixture(configured(), None, directory.path().join("debug.png"));
    let local_url = "http://hangar-2.local";
    control
        .model
        .set_urls(local_url.to_owned(), Some("http://10.0.4.74".to_owned()));
    let actual = app.step(&[], Instant::now()).frame.expect("waiting frame");
    assert_setup_qr(&actual, local_url);
}

fn assert_setup_qr(frame: &[u8], local_url: &str) {
    const SIZE: usize = 480;
    const QR_LEFT: usize = 108;
    const QR_TOP: usize = 50;
    const QR_MODULE_PIXELS: usize = 8;
    const QR_QUIET_MODULES: usize = 4;

    let code = QrCode::with_error_correction_level(local_url.as_bytes(), EcLevel::M)
        .expect("expected local URL QR payload");
    let code_width = code.width();
    for (index, color) in code.into_colors().into_iter().enumerate() {
        let row = index / code_width + QR_QUIET_MODULES;
        let column = index % code_width + QR_QUIET_MODULES;
        let x = QR_LEFT + column * QR_MODULE_PIXELS;
        let y = QR_TOP + row * QR_MODULE_PIXELS;
        let offset = (y * SIZE + x) * 4;
        let expected = match color {
            Color::Dark => [0, 0, 0, 255],
            Color::Light => [255, 255, 255, 255],
        };
        assert_eq!(
            &frame[offset..offset + 4],
            expected,
            "QR module ({column}, {row})"
        );
    }
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
fn visible_time_and_date_redraw_exactly_when_the_sampled_unix_minute_changes() {
    let directory = tempfile::tempdir().expect("tempdir");
    let mut settings = configured();
    settings.footer.show_time = true;
    settings.footer.show_date = true;
    settings.footer.time_zone = TimeZone::Zulu;
    let (mut app, control, _) = app_fixture(
        settings,
        Some(Duration::ZERO),
        directory.path().join("debug.png"),
    );

    control.unix_seconds.store(6_059, Ordering::Release);
    assert!(app.step(&[], Instant::now()).frame.is_some());
    assert!(app.step(&[], Instant::now()).frame.is_none());
    control.unix_seconds.store(6_060, Ordering::Release);
    assert!(app.step(&[], Instant::now()).frame.is_some());
    control.unix_seconds.store(6_119, Ordering::Release);
    assert!(app.step(&[], Instant::now()).frame.is_none());
    control.unix_seconds.store(6_120, Ordering::Release);
    assert!(app.step(&[], Instant::now()).frame.is_some());
}

#[test]
fn disabled_footer_ignores_wall_minutes_and_keeps_generation_and_adsb_stale_invalidation() {
    let directory = tempfile::tempdir().expect("tempdir");
    let (mut app, control, _) = app_fixture(
        configured(),
        Some(Duration::ZERO),
        directory.path().join("debug.png"),
    );

    control.unix_seconds.store(6_059, Ordering::Release);
    assert!(app.step(&[], Instant::now()).frame.is_some());
    control.unix_seconds.store(6_120, Ordering::Release);
    assert!(app.step(&[], Instant::now()).frame.is_none());
    control.now_ms.store(29_999, Ordering::Release);
    assert!(app.step(&[], Instant::now()).frame.is_none());
    control.now_ms.store(30_000, Ordering::Release);
    assert!(app.step(&[], Instant::now()).frame.is_some());
    control.model.record_adsb_error(Duration::from_secs(31));
    assert!(app.step(&[], Instant::now()).frame.is_some());
}

#[test]
fn selected_weather_redraws_at_the_monotonic_forty_five_minute_boundary() {
    let directory = tempfile::tempdir().expect("tempdir");
    let mut settings = configured();
    settings.footer.show_temperature = true;
    let (mut app, control, _) = app_fixture(
        settings,
        Some(Duration::ZERO),
        directory.path().join("debug.png"),
    );
    let location = control
        .model
        .snapshot()
        .settings
        .location
        .expect("configured location");
    control
        .model
        .record_environment_if_location(
            &location,
            EnvironmentReading {
                temperature_celsius: 22.0,
                humidity_percent: 54,
                weather_code: 2,
                utc_offset_seconds: -4 * 60 * 60,
                fetched_at: Duration::ZERO,
            },
        )
        .expect("current environment update");

    control.now_ms.store(2_699_000, Ordering::Release);
    assert!(app.step(&[], Instant::now()).frame.is_some());
    control.now_ms.store(2_699_999, Ordering::Release);
    assert!(app.step(&[], Instant::now()).frame.is_none());
    control.now_ms.store(2_700_000, Ordering::Release);
    assert!(app.step(&[], Instant::now()).frame.is_some());
    assert!(app.step(&[], Instant::now()).frame.is_none());
}

#[test]
fn active_night_transforms_the_uploaded_and_debug_frame_exactly_once() {
    let directory = tempfile::tempdir().expect("tempdir");
    let debug_path = directory.path().join("debug.png");
    let night_settings = red_night_settings();
    let (mut night_app, night_control, debug_requested) = app_fixture_with_backlight(
        night_settings,
        Some(Duration::ZERO),
        debug_path.clone(),
        Box::new(RecordingBacklight::available(5, 100)),
    );
    let location = night_control
        .model
        .snapshot()
        .settings
        .location
        .expect("location");
    night_control
        .model
        .record_solar_schedule_if_current(&location, Arc::new(solar_schedule(&location)))
        .expect("solar generation");
    night_control
        .unix_seconds
        .store(unix("2026-08-03T01:00:00Z"), Ordering::Release);
    debug_requested.store(true, Ordering::Release);
    let red = night_app
        .step(&[], Instant::now())
        .frame
        .expect("active-night frame");

    assert!(
        red.chunks_exact(4)
            .all(|pixel| pixel[1] == 0 && pixel[2] == 0),
        "physical red frame must contain no green or blue output"
    );
    assert_eq!(decode_png(&debug_path), red);

    let (mut full_app, full_control, _) = app_fixture(
        configured(),
        Some(Duration::ZERO),
        directory.path().join("full.png"),
    );
    full_control
        .unix_seconds
        .store(unix("2026-08-03T01:00:00Z"), Ordering::Release);
    let full = full_app
        .step(&[], Instant::now())
        .frame
        .expect("full-color frame");
    let mut expected = planeradar::render::Frame::new(480, 480, full).expect("expected frame");
    expected.apply_color_mode(FrameColorMode::RedOnly);
    assert_eq!(red, expected.pixels());

    let mut twice = expected.clone();
    twice.apply_color_mode(FrameColorMode::RedOnly);
    assert_ne!(
        red,
        twice.pixels(),
        "application pipeline applied the red transform more than once"
    );
}

#[test]
fn wall_minute_transition_ticks_brightness_without_rerender_until_color_changes() {
    let directory = tempfile::tempdir().expect("tempdir");
    let backlight = RecordingBacklight::available(100, 100);
    let probe = backlight.clone();
    let (mut app, control, _) = app_fixture_with_backlight(
        red_night_settings(),
        Some(Duration::ZERO),
        directory.path().join("debug.png"),
        Box::new(backlight),
    );
    let location = control
        .model
        .snapshot()
        .settings
        .location
        .expect("location");
    control
        .model
        .record_solar_schedule_if_current(&location, Arc::new(solar_schedule(&location)))
        .expect("solar generation");

    control
        .unix_seconds
        .store(unix("2026-08-03T23:59:59Z"), Ordering::Release);
    assert!(app.step(&[], Instant::now()).frame.is_some());
    assert_eq!(
        control.model.snapshot().backlight_availability,
        BacklightAvailability::Available
    );

    control.now_ms.store(1_000, Ordering::Release);
    control
        .unix_seconds
        .store(unix("2026-08-04T00:00:00Z"), Ordering::Release);
    assert!(
        app.step(&[], Instant::now()).frame.is_none(),
        "entering night must keep the existing full-color frame while dimming"
    );
    control.now_ms.store(2_000, Ordering::Release);
    assert!(app.step(&[], Instant::now()).frame.is_none());
    control.now_ms.store(3_000, Ordering::Release);
    let red = app
        .step(&[], Instant::now())
        .frame
        .expect("red transition frame");
    assert!(red.chunks_exact(4).all(|pixel| pixel[1..3] == [0, 0]));
    assert!(app.step(&[], Instant::now()).frame.is_none());
    assert_eq!(probe.writes(), [65, 30]);
}

#[test]
fn settings_location_and_solar_generations_invalidate_the_display_policy_key() {
    let directory = tempfile::tempdir().expect("tempdir");
    let settings = red_night_settings();
    let (mut app, control, _) = app_fixture(
        settings.clone(),
        Some(Duration::ZERO),
        directory.path().join("debug.png"),
    );
    control
        .unix_seconds
        .store(unix("2026-08-03T12:00:00Z"), Ordering::Release);
    assert!(app.step(&[], Instant::now()).frame.is_some());
    assert!(app.step(&[], Instant::now()).frame.is_none());

    let mut changed_settings = settings;
    changed_settings.brightness.day_percent = 95;
    control.model.replace_settings(changed_settings.clone());
    assert!(app.step(&[], Instant::now()).frame.is_some());

    let mut moved = changed_settings;
    moved.location.as_mut().expect("location").label = "moved".to_owned();
    control.model.replace_settings(moved.clone());
    assert!(app.step(&[], Instant::now()).frame.is_some());

    let location = moved.location.expect("location");
    control
        .model
        .record_solar_schedule_if_current(&location, Arc::new(solar_schedule(&location)))
        .expect("solar generation");
    assert!(app.step(&[], Instant::now()).frame.is_some());
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
            "--local-url",
            "http://planeradar.local",
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

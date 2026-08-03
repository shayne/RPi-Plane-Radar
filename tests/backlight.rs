use std::fs;
use std::os::unix::fs::{PermissionsExt, symlink};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use planeradar::backlight::{
    Backlight, BacklightController, BacklightError, EffectiveBrightness, NoopBacklight,
    SysfsBacklight, percent_to_level,
};
use planeradar::model::{
    BacklightAvailability, DisplayPeriod, DisplayPolicy, FrameColorMode, SolarStatus,
};
use tempfile::TempDir;

#[test]
fn opens_only_the_named_regular_readable_and_writable_class_device() {
    let fixture = SysfsFixture::new("255\n", "13\n");

    let backlight = SysfsBacklight::open(fixture.class_device.clone()).expect("named device");
    assert_eq!(backlight.availability(), BacklightAvailability::Available);
    assert_eq!(backlight.max_level(), 255);

    let wrong_name = fixture.class_root.join("other-backlight");
    symlink(&fixture.device_root, &wrong_name).expect("wrong-name class link");
    assert!(matches!(
        SysfsBacklight::open(wrong_name),
        Err(BacklightError::InvalidDevice)
    ));

    for attribute in ["max_brightness", "brightness"] {
        let fixture = SysfsFixture::new("255\n", "13\n");
        let path = fixture.device_root.join(attribute);
        fs::remove_file(&path).expect("remove regular attribute");
        fs::create_dir(&path).expect("replace attribute with directory");
        assert!(matches!(
            SysfsBacklight::open(fixture.class_device),
            Err(BacklightError::InvalidDevice)
        ));
    }

    for (attribute, mode) in [("max_brightness", 0o200), ("brightness", 0o400)] {
        let fixture = SysfsFixture::new("255\n", "13\n");
        let path = fixture.device_root.join(attribute);
        fs::set_permissions(&path, fs::Permissions::from_mode(mode)).expect("set permissions");
        assert!(matches!(
            SysfsBacklight::open(fixture.class_device),
            Err(BacklightError::InvalidDevice)
        ));
    }
}

#[test]
fn follows_the_named_class_link_but_rejects_symlinked_attributes() {
    let fixture = SysfsFixture::new("255\n", "13\n");
    let external = fixture.root.path().join("external-max");
    fs::write(&external, "255\n").expect("external attribute");
    let max_path = fixture.device_root.join("max_brightness");
    fs::remove_file(&max_path).expect("remove max");
    symlink(external, max_path).expect("symlink max");

    assert!(matches!(
        SysfsBacklight::open(fixture.class_device),
        Err(BacklightError::InvalidDevice)
    ));
}

#[test]
fn rejects_zero_malformed_overflowing_and_out_of_range_levels() {
    for max in ["0\n", "-1\n", "not-a-number\n", "4294967296\n"] {
        let fixture = SysfsFixture::new(max, "0\n");
        assert!(matches!(
            SysfsBacklight::open(fixture.class_device),
            Err(BacklightError::InvalidValue)
        ));
    }

    for current in ["-1\n", "not-a-number\n", "4294967296\n", "256\n"] {
        let fixture = SysfsFixture::new("255\n", current);
        assert!(matches!(
            SysfsBacklight::open(fixture.class_device),
            Err(BacklightError::InvalidValue)
        ));
    }
}

#[test]
fn maps_percentages_with_widened_nearest_integer_rounding() {
    assert_eq!(percent_to_level(5, 255), 13);
    assert_eq!(percent_to_level(30, 255), 77);
    assert_eq!(percent_to_level(100, 255), 255);
    assert_eq!(percent_to_level(50, 1000), 500);
    assert_eq!(percent_to_level(100, u32::MAX), u32::MAX);
}

#[test]
fn sysfs_skips_a_repeated_successful_level_without_reopening_brightness() {
    let fixture = SysfsFixture::new("255\n", "13\n");
    let mut backlight = SysfsBacklight::open(fixture.class_device.clone()).expect("named device");
    let brightness = fixture.device_root.join("brightness");
    fs::set_permissions(&brightness, fs::Permissions::from_mode(0o440))
        .expect("remove write permission after discovery");

    backlight
        .write_level(13)
        .expect("cached level requires no filesystem write");
    assert!(matches!(
        backlight.write_level(14),
        Err(BacklightError::Io(_))
    ));
    assert_eq!(fs::read_to_string(brightness).expect("brightness"), "13\n");
}

#[test]
fn ramps_linearly_for_two_seconds_without_redundant_effective_writes() {
    let fake = RecordingBacklight::available(100, 0);
    let state = fake.state.clone();
    let mut controller = BacklightController::new(Box::new(fake));
    let policy = policy(DisplayPeriod::Day, 100, FrameColorMode::FullColor);

    let samples = [0, 500, 1000, 1500, 2000]
        .map(|milliseconds| controller.update(&policy, Duration::from_millis(milliseconds)));
    assert_eq!(
        samples.map(|update| update.brightness.expect("effective brightness").level),
        [0, 25, 50, 75, 100]
    );
    assert!(
        samples
            .iter()
            .all(|update| update.availability == BacklightAvailability::Available)
    );
    assert_eq!(
        state.lock().expect("fake state").writes,
        vec![25, 50, 75, 100]
    );

    let unchanged = controller.update(&policy, Duration::from_millis(2001));
    assert_eq!(unchanged.brightness, Some(brightness(100, 100)));
    assert_eq!(
        state.lock().expect("fake state").writes,
        vec![25, 50, 75, 100]
    );
}

#[test]
fn retargets_from_the_current_interpolated_level() {
    let fake = RecordingBacklight::available(100, 100);
    let mut controller = BacklightController::new(Box::new(fake));
    let dim = policy(DisplayPeriod::Night, 20, FrameColorMode::FullColor);
    let brighten = policy(DisplayPeriod::Day, 80, FrameColorMode::FullColor);

    assert_eq!(level(&controller.update(&dim, Duration::ZERO)), 100);
    assert_eq!(
        level(&controller.update(&dim, Duration::from_millis(1000))),
        60
    );
    assert_eq!(
        level(&controller.update(&brighten, Duration::from_millis(1000))),
        60,
        "new ramp starts at the prior ramp's value at the retargeting instant"
    );
    assert_eq!(
        level(&controller.update(&brighten, Duration::from_millis(1500))),
        65
    );
    assert_eq!(
        level(&controller.update(&brighten, Duration::from_millis(3000))),
        80
    );
}

#[test]
fn entering_night_dims_completely_before_red_is_safe_to_render() {
    let fake = RecordingBacklight::available(100, 100);
    let mut controller = BacklightController::new(Box::new(fake));
    let day = policy(DisplayPeriod::Day, 100, FrameColorMode::FullColor);
    let night = policy(DisplayPeriod::Night, 20, FrameColorMode::RedOnly);

    assert_eq!(
        controller.update(&day, Duration::ZERO).color_mode,
        FrameColorMode::FullColor
    );
    for (milliseconds, expected_level) in [(100, 100), (600, 80), (1100, 60), (1600, 40)] {
        let update = controller.update(&night, Duration::from_millis(milliseconds));
        assert_eq!(level(&update), expected_level);
        assert_eq!(update.color_mode, FrameColorMode::FullColor);
    }
    let complete = controller.update(&night, Duration::from_millis(2100));
    assert_eq!(level(&complete), 20);
    assert_eq!(complete.color_mode, FrameColorMode::RedOnly);
}

#[test]
fn leaving_night_restores_full_color_at_the_dim_level_before_brightening() {
    let fake = RecordingBacklight::available(100, 20);
    let mut controller = BacklightController::new(Box::new(fake));
    let night = policy(DisplayPeriod::Night, 20, FrameColorMode::RedOnly);
    let day = policy(DisplayPeriod::Day, 100, FrameColorMode::FullColor);

    assert_eq!(
        controller.update(&night, Duration::ZERO).color_mode,
        FrameColorMode::RedOnly
    );
    let transition = controller.update(&day, Duration::from_secs(1));
    assert_eq!(transition.brightness, Some(brightness(20, 20)));
    assert_eq!(transition.color_mode, FrameColorMode::FullColor);

    let ramping = controller.update(&day, Duration::from_millis(1500));
    assert_eq!(ramping.brightness, Some(brightness(40, 40)));
    assert_eq!(ramping.color_mode, FrameColorMode::FullColor);
}

#[test]
fn active_night_startup_is_red_on_the_first_result_and_ramps_from_boot_level() {
    let fake = RecordingBacklight::available(100, 5);
    let mut controller = BacklightController::new(Box::new(fake));
    let night = policy(DisplayPeriod::Night, 30, FrameColorMode::RedOnly);

    let first = controller.update(&night, Duration::ZERO);
    assert_eq!(first.brightness, Some(brightness(5, 5)));
    assert_eq!(first.color_mode, FrameColorMode::RedOnly);
    let middle = controller.update(&night, Duration::from_secs(1));
    assert_eq!(middle.brightness, Some(brightness(18, 18)));
    assert_eq!(middle.color_mode, FrameColorMode::RedOnly);
    let complete = controller.update(&night, Duration::from_secs(2));
    assert_eq!(complete.brightness, Some(brightness(30, 30)));
    assert_eq!(complete.color_mode, FrameColorMode::RedOnly);
}

#[test]
fn read_failure_is_nonfatal_rate_limited_and_keeps_requested_red() {
    let fake = RecordingBacklight::available(100, 5);
    fake.state.lock().expect("fake state").fail_reads = true;
    let mut controller = BacklightController::new(Box::new(fake));
    let night = policy(DisplayPeriod::Night, 30, FrameColorMode::RedOnly);

    let first = controller.update(&night, Duration::ZERO);
    assert_eq!(first.availability, BacklightAvailability::Unavailable);
    assert_eq!(first.brightness, None);
    assert_eq!(first.color_mode, FrameColorMode::RedOnly);
    assert!(first.should_report_failure);

    let suppressed = controller.update(&night, Duration::from_secs(29));
    assert_eq!(suppressed.color_mode, FrameColorMode::RedOnly);
    assert!(!suppressed.should_report_failure);

    let next_window = controller.update(&night, Duration::from_secs(30));
    assert_eq!(next_window.color_mode, FrameColorMode::RedOnly);
    assert!(next_window.should_report_failure);
}

#[test]
fn write_failure_is_nonfatal_and_never_strands_requested_red_mode() {
    let fake = RecordingBacklight::available(100, 100);
    let state = fake.state.clone();
    let mut controller = BacklightController::new(Box::new(fake));
    let day = policy(DisplayPeriod::Day, 100, FrameColorMode::FullColor);
    let night = policy(DisplayPeriod::Night, 20, FrameColorMode::RedOnly);

    controller.update(&day, Duration::ZERO);
    controller.update(&night, Duration::from_millis(100));
    state.lock().expect("fake state").fail_writes = true;

    let failed = controller.update(&night, Duration::from_millis(600));
    assert_eq!(failed.availability, BacklightAvailability::Unavailable);
    assert_eq!(failed.brightness, Some(brightness(100, 100)));
    assert_eq!(failed.color_mode, FrameColorMode::RedOnly);
    assert!(failed.should_report_failure);
}

#[test]
fn noop_backlight_is_deterministically_unavailable_without_disabling_red() {
    let mut controller = BacklightController::new(Box::new(NoopBacklight));
    let night = policy(DisplayPeriod::Night, 30, FrameColorMode::RedOnly);

    let first = controller.update(&night, Duration::ZERO);
    assert_eq!(first.availability, BacklightAvailability::Unavailable);
    assert_eq!(first.brightness, None);
    assert_eq!(first.color_mode, FrameColorMode::RedOnly);
    assert!(first.should_report_failure);
    let repeated = controller.update(&night, Duration::from_secs(1));
    assert_eq!(repeated.availability, BacklightAvailability::Unavailable);
    assert!(!repeated.should_report_failure);
}

fn policy(
    period: DisplayPeriod,
    brightness_percent: u8,
    color_mode: FrameColorMode,
) -> DisplayPolicy {
    DisplayPolicy {
        period,
        brightness_percent,
        color_mode,
        next_transition: None,
        solar_status: SolarStatus::Disabled,
    }
}

fn brightness(level: u32, percent: u8) -> EffectiveBrightness {
    EffectiveBrightness { level, percent }
}

fn level(update: &planeradar::backlight::BacklightUpdate) -> u32 {
    update.brightness.expect("effective brightness").level
}

struct SysfsFixture {
    root: TempDir,
    class_root: PathBuf,
    class_device: PathBuf,
    device_root: PathBuf,
}

impl SysfsFixture {
    fn new(maximum: &str, current: &str) -> Self {
        let root = tempfile::tempdir().expect("temporary sysfs");
        let class_root = root.path().join("sys/class/backlight");
        let device_root = root.path().join("sys/devices/platform/pwm-backlight");
        fs::create_dir_all(&class_root).expect("class root");
        fs::create_dir_all(&device_root).expect("device root");
        fs::write(device_root.join("max_brightness"), maximum).expect("max brightness");
        fs::write(device_root.join("brightness"), current).expect("brightness");
        let class_device = class_root.join("planeradar-backlight");
        symlink(&device_root, &class_device).expect("class device link");
        Self {
            root,
            class_root,
            class_device,
            device_root,
        }
    }
}

#[derive(Clone)]
struct RecordingBacklight {
    state: Arc<Mutex<RecordingState>>,
}

struct RecordingState {
    availability: BacklightAvailability,
    max_level: u32,
    current_level: u32,
    fail_reads: bool,
    fail_writes: bool,
    writes: Vec<u32>,
}

impl RecordingBacklight {
    fn available(max_level: u32, current_level: u32) -> Self {
        Self {
            state: Arc::new(Mutex::new(RecordingState {
                availability: BacklightAvailability::Available,
                max_level,
                current_level,
                fail_reads: false,
                fail_writes: false,
                writes: Vec::new(),
            })),
        }
    }
}

impl Backlight for RecordingBacklight {
    fn availability(&self) -> BacklightAvailability {
        self.state.lock().expect("fake state").availability
    }

    fn current_level(&mut self) -> Result<u32, BacklightError> {
        let state = self.state.lock().expect("fake state");
        if state.fail_reads {
            Err(BacklightError::Unavailable)
        } else {
            Ok(state.current_level)
        }
    }

    fn max_level(&self) -> u32 {
        self.state.lock().expect("fake state").max_level
    }

    fn write_level(&mut self, level: u32) -> Result<(), BacklightError> {
        let mut state = self.state.lock().expect("fake state");
        if state.fail_writes {
            return Err(BacklightError::Unavailable);
        }
        state.current_level = level;
        state.writes.push(level);
        Ok(())
    }
}

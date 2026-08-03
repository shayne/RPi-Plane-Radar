use std::ffi::OsStr;
use std::fs::{self, OpenOptions};
use std::io::{self, Read, Write};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::model::{BacklightAvailability, DisplayPolicy, FrameColorMode};

const DEVICE_NAME: &str = "planeradar-backlight";
const MAX_BRIGHTNESS_ATTRIBUTE: &str = "max_brightness";
const BRIGHTNESS_ATTRIBUTE: &str = "brightness";
const RAMP_DURATION: Duration = Duration::from_secs(2);
const FAILURE_REPORT_INTERVAL: Duration = Duration::from_secs(30);

pub trait Backlight: Send {
    fn availability(&self) -> BacklightAvailability;
    fn current_level(&mut self) -> Result<u32, BacklightError>;
    fn max_level(&self) -> u32;
    fn write_level(&mut self, level: u32) -> Result<(), BacklightError>;
}

#[derive(Debug, thiserror::Error)]
pub enum BacklightError {
    #[error("invalid backlight device")]
    InvalidDevice,
    #[error("invalid backlight value")]
    InvalidValue,
    #[error("backlight I/O failed")]
    Io(#[source] io::Error),
    #[error("backlight unavailable")]
    Unavailable,
}

pub struct SysfsBacklight {
    brightness_path: PathBuf,
    max_level: u32,
    last_successful_level: Option<u32>,
}

impl SysfsBacklight {
    pub fn open(class_device: PathBuf) -> Result<Self, BacklightError> {
        if class_device.file_name() != Some(OsStr::new(DEVICE_NAME)) {
            return Err(BacklightError::InvalidDevice);
        }

        let device_root = fs::canonicalize(class_device).map_err(BacklightError::Io)?;
        if !fs::metadata(&device_root)
            .map_err(BacklightError::Io)?
            .is_dir()
        {
            return Err(BacklightError::InvalidDevice);
        }

        let max_path = device_root.join(MAX_BRIGHTNESS_ATTRIBUTE);
        let brightness_path = device_root.join(BRIGHTNESS_ATTRIBUTE);
        validate_attribute(&max_path, false)?;
        validate_attribute(&brightness_path, true)?;

        let max_level = read_level(&max_path)?;
        if max_level == 0 {
            return Err(BacklightError::InvalidValue);
        }
        let current_level = read_level(&brightness_path)?;
        if current_level > max_level {
            return Err(BacklightError::InvalidValue);
        }

        Ok(Self {
            brightness_path,
            max_level,
            last_successful_level: Some(current_level),
        })
    }
}

impl Backlight for SysfsBacklight {
    fn availability(&self) -> BacklightAvailability {
        BacklightAvailability::Available
    }

    fn current_level(&mut self) -> Result<u32, BacklightError> {
        let level = read_level(&self.brightness_path)?;
        if level > self.max_level {
            return Err(BacklightError::InvalidValue);
        }
        self.last_successful_level = Some(level);
        Ok(level)
    }

    fn max_level(&self) -> u32 {
        self.max_level
    }

    fn write_level(&mut self, level: u32) -> Result<(), BacklightError> {
        if level > self.max_level {
            return Err(BacklightError::InvalidValue);
        }
        if self.last_successful_level == Some(level) {
            return Ok(());
        }

        let mut brightness = OpenOptions::new()
            .write(true)
            .truncate(true)
            .open(&self.brightness_path)
            .map_err(BacklightError::Io)?;
        writeln!(brightness, "{level}").map_err(BacklightError::Io)?;
        self.last_successful_level = Some(level);
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub struct NoopBacklight;

impl Backlight for NoopBacklight {
    fn availability(&self) -> BacklightAvailability {
        BacklightAvailability::Unavailable
    }

    fn current_level(&mut self) -> Result<u32, BacklightError> {
        Err(BacklightError::Unavailable)
    }

    fn max_level(&self) -> u32 {
        1
    }

    fn write_level(&mut self, _level: u32) -> Result<(), BacklightError> {
        Err(BacklightError::Unavailable)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct EffectiveBrightness {
    pub level: u32,
    pub percent: u8,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct BacklightUpdate {
    pub availability: BacklightAvailability,
    pub brightness: Option<EffectiveBrightness>,
    pub color_mode: FrameColorMode,
    pub should_report_failure: bool,
}

pub struct BacklightController {
    backlight: Box<dyn Backlight>,
    known_level: Option<u32>,
    target_level: Option<u32>,
    ramp: Option<LinearRamp>,
    requested_color: Option<FrameColorMode>,
    safe_color: FrameColorMode,
    red_after_ramp: bool,
    last_failure_report: Option<Duration>,
}

impl BacklightController {
    pub fn new(backlight: Box<dyn Backlight>) -> Self {
        Self {
            backlight,
            known_level: None,
            target_level: None,
            ramp: None,
            requested_color: None,
            safe_color: FrameColorMode::FullColor,
            red_after_ramp: false,
            last_failure_report: None,
        }
    }

    pub fn update(&mut self, policy: &DisplayPolicy, now: Duration) -> BacklightUpdate {
        let first_policy = self.requested_color.is_none();
        self.apply_color_request(policy.color_mode, first_policy);

        if self.backlight.availability() != BacklightAvailability::Available {
            return self.failed_update(policy.color_mode, now);
        }

        let max_level = self.backlight.max_level();
        if max_level == 0 {
            return self.failed_update(policy.color_mode, now);
        }

        if self.known_level.is_none() {
            match self.backlight.current_level() {
                Ok(level) if level <= max_level => self.known_level = Some(level),
                Ok(_) | Err(_) => return self.failed_update(policy.color_mode, now),
            }
        }

        let target_level = percent_to_level(policy.brightness_percent, max_level);
        if self.target_level != Some(target_level) {
            let start_level = self
                .interpolated_level(now)
                .expect("known level initialized");
            self.target_level = Some(target_level);
            self.ramp = (start_level != target_level)
                .then(|| LinearRamp::new(start_level, target_level, now));
            if self.safe_color == FrameColorMode::FullColor
                && policy.color_mode == FrameColorMode::RedOnly
                && !first_policy
            {
                self.red_after_ramp = self.ramp.is_some();
            }
        }

        let desired_level = self
            .interpolated_level(now)
            .expect("known level initialized");
        let ramp_complete = self.ramp.is_some_and(|ramp| ramp.is_complete(now));
        if self.known_level != Some(desired_level) {
            if self.backlight.write_level(desired_level).is_err() {
                return self.failed_update(policy.color_mode, now);
            }
            self.known_level = Some(desired_level);
        }
        if ramp_complete {
            self.ramp = None;
        }
        if self.red_after_ramp && self.ramp.is_none() {
            self.safe_color = FrameColorMode::RedOnly;
            self.red_after_ramp = false;
        }

        self.available_update(max_level)
    }

    fn apply_color_request(&mut self, requested: FrameColorMode, first_policy: bool) {
        if first_policy {
            self.safe_color = requested;
            self.red_after_ramp = false;
        } else if requested == FrameColorMode::FullColor {
            self.safe_color = FrameColorMode::FullColor;
            self.red_after_ramp = false;
        } else if self.requested_color != Some(FrameColorMode::RedOnly) {
            self.red_after_ramp = true;
        }
        self.requested_color = Some(requested);
    }

    fn interpolated_level(&self, now: Duration) -> Option<u32> {
        self.ramp
            .map(|ramp| ramp.level_at(now))
            .or(self.known_level)
    }

    fn available_update(&self, max_level: u32) -> BacklightUpdate {
        BacklightUpdate {
            availability: BacklightAvailability::Available,
            brightness: self
                .known_level
                .map(|level| EffectiveBrightness::new(level, max_level)),
            color_mode: self.safe_color,
            should_report_failure: false,
        }
    }

    fn failed_update(&mut self, requested_color: FrameColorMode, now: Duration) -> BacklightUpdate {
        if requested_color == FrameColorMode::RedOnly {
            self.safe_color = FrameColorMode::RedOnly;
            self.red_after_ramp = false;
        }
        let should_report_failure = self.last_failure_report.is_none_or(|last| {
            now.checked_sub(last)
                .is_some_and(|age| age >= FAILURE_REPORT_INTERVAL)
        });
        if should_report_failure {
            self.last_failure_report = Some(now);
        }
        let max_level = self.backlight.max_level();
        BacklightUpdate {
            availability: BacklightAvailability::Unavailable,
            brightness: self
                .known_level
                .filter(|_| max_level > 0)
                .map(|level| EffectiveBrightness::new(level, max_level)),
            color_mode: self.safe_color,
            should_report_failure,
        }
    }
}

impl EffectiveBrightness {
    fn new(level: u32, max_level: u32) -> Self {
        let rounded = u64::from(level)
            .checked_mul(100)
            .and_then(|value| value.checked_add(u64::from(max_level) / 2))
            .expect("u32 brightness conversion fits u64")
            / u64::from(max_level);
        Self {
            level,
            percent: u8::try_from(rounded.min(100)).expect("percentage is at most 100"),
        }
    }
}

#[derive(Clone, Copy)]
struct LinearRamp {
    start_level: u32,
    target_level: u32,
    started_at: Duration,
}

impl LinearRamp {
    fn new(start_level: u32, target_level: u32, started_at: Duration) -> Self {
        Self {
            start_level,
            target_level,
            started_at,
        }
    }

    fn level_at(self, now: Duration) -> u32 {
        let elapsed = now
            .checked_sub(self.started_at)
            .unwrap_or(Duration::ZERO)
            .min(RAMP_DURATION);
        let elapsed_nanos = u64::try_from(elapsed.as_nanos())
            .unwrap_or(u64::MAX)
            .min(RAMP_DURATION.as_nanos() as u64);
        let ramp_nanos = RAMP_DURATION.as_nanos() as u64;
        let difference = u64::from(self.start_level.abs_diff(self.target_level));
        let adjustment = difference
            .checked_mul(elapsed_nanos)
            .and_then(|scaled| scaled.checked_add(ramp_nanos / 2))
            .expect("u32 level over a two-second ramp fits u64")
            / ramp_nanos;
        let adjustment = u32::try_from(adjustment).expect("adjustment is bounded by u32 levels");
        if self.target_level >= self.start_level {
            self.start_level
                .checked_add(adjustment)
                .expect("adjustment does not exceed target")
        } else {
            self.start_level
                .checked_sub(adjustment)
                .expect("adjustment does not exceed start")
        }
    }

    fn is_complete(self, now: Duration) -> bool {
        now.checked_sub(self.started_at)
            .is_some_and(|elapsed| elapsed >= RAMP_DURATION)
    }
}

pub fn percent_to_level(percent: u8, max_level: u32) -> u32 {
    let rounded = u64::from(percent)
        .checked_mul(u64::from(max_level))
        .and_then(|value| value.checked_add(50))
        .expect("u8 percentage and u32 maximum fit u64")
        / 100;
    u32::try_from(rounded).unwrap_or(u32::MAX)
}

fn validate_attribute(path: &Path, writable: bool) -> Result<(), BacklightError> {
    let metadata = fs::symlink_metadata(path).map_err(BacklightError::Io)?;
    if !metadata.file_type().is_file() {
        return Err(BacklightError::InvalidDevice);
    }
    let mode = metadata.permissions().mode();
    if mode & 0o444 == 0 || (writable && mode & 0o222 == 0) {
        return Err(BacklightError::InvalidDevice);
    }
    if writable {
        OpenOptions::new()
            .read(true)
            .write(true)
            .open(path)
            .map_err(BacklightError::Io)?;
    } else {
        OpenOptions::new()
            .read(true)
            .open(path)
            .map_err(BacklightError::Io)?;
    }
    Ok(())
}

fn read_level(path: &Path) -> Result<u32, BacklightError> {
    let mut value = String::new();
    OpenOptions::new()
        .read(true)
        .open(path)
        .map_err(BacklightError::Io)?
        .read_to_string(&mut value)
        .map_err(BacklightError::Io)?;
    value
        .trim()
        .parse::<u32>()
        .map_err(|_| BacklightError::InvalidValue)
}

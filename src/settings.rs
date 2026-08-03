use std::fs::{self, File};
use std::io::{self, Write};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use serde::Deserialize;
use serde_json::Value;
use thiserror::Error;

use crate::model::{
    BrightnessSettings, FooterSettings, Location, RadarSettings, SETTINGS_SCHEMA_VERSION, Units,
};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct LegacyRadarSettingsV1 {
    schema_version: u32,
    location: Option<Location>,
    units: Units,
    show_runways: bool,
    range_index: u8,
}

impl From<LegacyRadarSettingsV1> for RadarSettings {
    fn from(legacy: LegacyRadarSettingsV1) -> Self {
        RadarSettings {
            location: legacy.location,
            units: legacy.units,
            show_runways: legacy.show_runways,
            range_index: legacy.range_index,
            brightness: BrightnessSettings::default(),
            ..RadarSettings::default()
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct LegacyRadarSettingsV2 {
    schema_version: u32,
    location: Option<Location>,
    units: Units,
    show_runways: bool,
    range_index: u8,
    show_callsign: bool,
    show_route: bool,
    show_expanded_model: bool,
    radar_text_scale_percent: u8,
    minimum_altitude_feet: Option<i32>,
    maximum_altitude_feet: Option<i32>,
    footer: FooterSettings,
}

impl From<LegacyRadarSettingsV2> for RadarSettings {
    fn from(legacy: LegacyRadarSettingsV2) -> Self {
        Self {
            schema_version: SETTINGS_SCHEMA_VERSION,
            location: legacy.location,
            units: legacy.units,
            show_runways: legacy.show_runways,
            range_index: legacy.range_index,
            show_callsign: legacy.show_callsign,
            show_route: legacy.show_route,
            show_expanded_model: legacy.show_expanded_model,
            radar_text_scale_percent: legacy.radar_text_scale_percent,
            minimum_altitude_feet: legacy.minimum_altitude_feet,
            maximum_altitude_feet: legacy.maximum_altitude_feet,
            footer: legacy.footer,
            brightness: BrightnessSettings::default(),
        }
    }
}

#[derive(Debug, Error)]
pub enum SettingsError {
    #[error("invalid settings: {0}")]
    Invalid(&'static str),
    #[error("failed to read or write settings: {0}")]
    Io(#[from] io::Error),
    #[error("failed to parse or serialize settings JSON: {0}")]
    Json(#[from] serde_json::Error),
    #[error("failed to atomically persist settings: {0}")]
    Persist(#[from] tempfile::PersistError),
}

pub fn validate_settings(value: Value) -> Result<RadarSettings, SettingsError> {
    let schema_version =
        value
            .get("schema_version")
            .and_then(Value::as_u64)
            .ok_or(SettingsError::Invalid(
                "schema version must be an unsigned integer",
            ))?;
    let settings = match schema_version {
        1 => {
            let legacy: LegacyRadarSettingsV1 = serde_json::from_value(value)?;
            if legacy.schema_version != 1 {
                return Err(SettingsError::Invalid("unsupported schema version"));
            }
            legacy.into()
        }
        2 => {
            let legacy: LegacyRadarSettingsV2 = serde_json::from_value(value)?;
            if legacy.schema_version != 2 {
                return Err(SettingsError::Invalid("unsupported schema version"));
            }
            legacy.into()
        }
        version if version == u64::from(SETTINGS_SCHEMA_VERSION) => serde_json::from_value(value)?,
        _ => return Err(SettingsError::Invalid("unsupported schema version")),
    };
    validate_radar_settings(&settings)?;
    Ok(settings)
}

pub struct SettingsStore {
    path: PathBuf,
}

impl SettingsStore {
    pub fn new(path: PathBuf) -> Self {
        Self { path }
    }

    pub fn load(&self) -> Result<RadarSettings, SettingsError> {
        match fs::read(&self.path) {
            Ok(bytes) => validate_settings(serde_json::from_slice(&bytes)?),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(RadarSettings::default()),
            Err(error) => Err(error.into()),
        }
    }

    pub fn save(&self, settings: &RadarSettings) -> Result<(), SettingsError> {
        validate_radar_settings(settings)?;
        let mut serialized = serde_json::to_vec_pretty(settings)?;
        serialized.push(b'\n');

        let parent = parent_directory(&self.path);
        create_parent_if_missing(parent)?;

        let mut temporary = tempfile::Builder::new()
            .prefix(".planeradar-settings-")
            .tempfile_in(parent)?;
        temporary.write_all(&serialized)?;
        temporary.flush()?;
        temporary.as_file().sync_all()?;
        temporary.persist(&self.path)?;
        File::open(parent)?.sync_all()?;
        Ok(())
    }
}

fn validate_radar_settings(settings: &RadarSettings) -> Result<(), SettingsError> {
    if settings.schema_version != SETTINGS_SCHEMA_VERSION {
        return Err(SettingsError::Invalid("unsupported schema version"));
    }
    if settings.range_index > 3 {
        return Err(SettingsError::Invalid(
            "range index must be between 0 and 3",
        ));
    }
    if let Some(location) = &settings.location {
        validate_location(location)?;
    }
    if !matches!(
        settings.radar_text_scale_percent,
        80 | 90 | 100 | 110 | 120 | 130
    ) {
        return Err(SettingsError::Invalid(
            "radar text scale must be 80, 90, 100, 110, 120, or 130 percent",
        ));
    }
    if settings
        .minimum_altitude_feet
        .is_some_and(|altitude| !(-2000..=100_000).contains(&altitude))
        || settings
            .maximum_altitude_feet
            .is_some_and(|altitude| !(-2000..=100_000).contains(&altitude))
    {
        return Err(SettingsError::Invalid(
            "altitude bounds must be between -2000 and 100000 feet",
        ));
    }
    if matches!(
        (
            settings.minimum_altitude_feet,
            settings.maximum_altitude_feet
        ),
        (Some(minimum), Some(maximum)) if minimum > maximum
    ) {
        return Err(SettingsError::Invalid(
            "minimum altitude must not exceed maximum altitude",
        ));
    }
    if !valid_percent(settings.brightness.day_percent)
        || !valid_percent(settings.brightness.night.brightness_percent)
    {
        return Err(SettingsError::Invalid(
            "brightness must be between 5 and 100 percent in 5 percent steps",
        ));
    }
    if settings.brightness.night.start_hour > 23 {
        return Err(SettingsError::Invalid(
            "night mode start hour must be between 0 and 23",
        ));
    }
    if settings.brightness.night.start_minute > 59 {
        return Err(SettingsError::Invalid(
            "night mode start minute must be between 0 and 59",
        ));
    }
    Ok(())
}

#[allow(clippy::manual_is_multiple_of)]
fn valid_percent(value: u8) -> bool {
    (5..=100).contains(&value) && value % 5 == 0
}

fn validate_location(location: &Location) -> Result<(), SettingsError> {
    if !location.latitude.is_finite() {
        return Err(SettingsError::Invalid("latitude must be finite"));
    }
    if !location.longitude.is_finite() {
        return Err(SettingsError::Invalid("longitude must be finite"));
    }
    if !(-90.0..=90.0).contains(&location.latitude) {
        return Err(SettingsError::Invalid(
            "latitude must be between -90 and 90",
        ));
    }
    if !(-180.0..=180.0).contains(&location.longitude) {
        return Err(SettingsError::Invalid(
            "longitude must be between -180 and 180",
        ));
    }
    Ok(())
}

fn parent_directory(path: &Path) -> &Path {
    path.parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."))
}

fn create_parent_if_missing(parent: &Path) -> Result<(), SettingsError> {
    if parent.exists() {
        return Ok(());
    }

    fs::create_dir_all(parent)?;
    fs::set_permissions(parent, fs::Permissions::from_mode(0o750))?;
    Ok(())
}

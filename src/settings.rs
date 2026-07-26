use std::fs::{self, File};
use std::io::{self, Write};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use serde_json::Value;
use thiserror::Error;

use crate::model::{Location, RadarSettings};

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
    let settings = serde_json::from_value(value)?;
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
    if settings.schema_version != 1 {
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
    Ok(())
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

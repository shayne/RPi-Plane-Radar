use std::collections::HashSet;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::Path;
use std::time::Duration;

use jiff::Timestamp;
use jiff::civil::Date;
use jiff::tz::TimeZone;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;
use url::Url;

use crate::http::{HttpClient, HttpError, HttpRequest};
use crate::model::Location;

const DEFAULT_PROVIDER_BASE: &str = "https://api.open-meteo.com/v1/forecast";
const CONNECT_TIMEOUT: Duration = Duration::from_secs(2);
const READ_TIMEOUT: Duration = Duration::from_secs(4);
const MAX_RESPONSE_BYTES: usize = 64 * 1024;
const CACHE_SCHEMA_VERSION: u32 = 1;
const EXPECTED_DAYS: usize = 17;
const MAX_TIME_ZONE_BYTES: usize = 64;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SolarSchedule {
    pub schema_version: u32,
    pub latitude: f64,
    pub longitude: f64,
    pub time_zone: String,
    pub fetched_at_unix: u64,
    pub days: Vec<SolarDay>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SolarDay {
    pub date: String,
    pub sunrise_unix: Option<i64>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SolarErrorCategory {
    InvalidLocation,
    InvalidProvider,
    InvalidRequest,
    Tls,
    Timeout,
    Transport,
    Body,
    BodyTooLarge,
    Status,
    Json,
    Schema,
    Io,
    Persist,
}

impl SolarErrorCategory {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::InvalidLocation => "invalid-location",
            Self::InvalidProvider => "invalid-provider",
            Self::InvalidRequest => "invalid-request",
            Self::Tls => "tls",
            Self::Timeout => "timeout",
            Self::Transport => "transport",
            Self::Body => "body",
            Self::BodyTooLarge => "body-too-large",
            Self::Status => "status",
            Self::Json => "json",
            Self::Schema => "schema",
            Self::Io => "io",
            Self::Persist => "persist",
        }
    }
}

#[derive(Debug, Error)]
pub enum SolarError {
    #[error("solar location is invalid")]
    InvalidLocation,
    #[error("solar provider URL is invalid")]
    InvalidProvider,
    #[error("solar HTTP request failed: {0}")]
    Http(#[from] HttpError),
    #[error("solar provider returned HTTP status {0}")]
    Status(u16),
    #[error("solar response was not valid JSON")]
    Json,
    #[error("solar response or cache schema was invalid: {0}")]
    Schema(&'static str),
    #[error("solar cache input/output failed: {0}")]
    Io(#[from] io::Error),
    #[error("solar cache atomic persist failed: {0}")]
    Persist(#[from] tempfile::PersistError),
}

impl SolarError {
    pub const fn category(&self) -> SolarErrorCategory {
        match self {
            Self::InvalidLocation => SolarErrorCategory::InvalidLocation,
            Self::InvalidProvider => SolarErrorCategory::InvalidProvider,
            Self::Http(HttpError::InvalidTimeout | HttpError::InvalidBodyLimit) => {
                SolarErrorCategory::InvalidRequest
            }
            Self::Http(HttpError::TlsVerificationRequired) => SolarErrorCategory::Tls,
            Self::Http(HttpError::Timeout) => SolarErrorCategory::Timeout,
            Self::Http(HttpError::Transport) => SolarErrorCategory::Transport,
            Self::Http(HttpError::Body) => SolarErrorCategory::Body,
            Self::Http(HttpError::BodyTooLarge) => SolarErrorCategory::BodyTooLarge,
            Self::Status(_) => SolarErrorCategory::Status,
            Self::Json => SolarErrorCategory::Json,
            Self::Schema(_) => SolarErrorCategory::Schema,
            Self::Io(_) => SolarErrorCategory::Io,
            Self::Persist(_) => SolarErrorCategory::Persist,
        }
    }
}

pub struct SolarClient<C> {
    http: C,
    provider_base: String,
}

impl<C: HttpClient> SolarClient<C> {
    pub fn new(http: C) -> Self {
        Self::with_provider_base(http, DEFAULT_PROVIDER_BASE.to_owned())
    }

    pub fn with_provider_base(http: C, provider_base: String) -> Self {
        Self {
            http,
            provider_base: provider_base.trim_end_matches('/').to_owned(),
        }
    }

    pub fn fetch(
        &self,
        location: &Location,
        fetched_at_unix: u64,
    ) -> Result<SolarSchedule, SolarError> {
        validate_location(location)?;
        validate_provider_base(&self.provider_base)?;

        let request = HttpRequest {
            url: self.provider_base.clone(),
            query: vec![
                ("latitude".to_owned(), format_coordinate(location.latitude)),
                (
                    "longitude".to_owned(),
                    format_coordinate(location.longitude),
                ),
                ("daily".to_owned(), "sunrise".to_owned()),
                ("timezone".to_owned(), "auto".to_owned()),
                ("timeformat".to_owned(), "unixtime".to_owned()),
                ("past_days".to_owned(), "1".to_owned()),
                ("forecast_days".to_owned(), "16".to_owned()),
            ],
            headers: Vec::new(),
            connect_timeout: CONNECT_TIMEOUT,
            read_timeout: READ_TIMEOUT,
            max_response_bytes: MAX_RESPONSE_BYTES,
            verify_tls: true,
        };
        let response = self.http.execute(request)?;
        if response.status != 200 {
            return Err(SolarError::Status(response.status));
        }

        let value: Value = serde_json::from_slice(&response.body).map_err(|_| SolarError::Json)?;
        let wire: WireResponse =
            serde_json::from_value(value).map_err(|_| SolarError::Schema("wire schema"))?;
        wire.into_schedule(location, fetched_at_unix)
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WireResponse {
    latitude: f64,
    longitude: f64,
    generationtime_ms: f64,
    utc_offset_seconds: i32,
    timezone: String,
    timezone_abbreviation: String,
    elevation: f64,
    daily_units: WireDailyUnits,
    daily: WireDaily,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WireDailyUnits {
    time: String,
    sunrise: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WireDaily {
    time: Vec<i64>,
    sunrise: Vec<Option<i64>>,
}

impl WireResponse {
    fn into_schedule(
        self,
        location: &Location,
        fetched_at_unix: u64,
    ) -> Result<SolarSchedule, SolarError> {
        if !valid_coordinate(self.latitude, -90.0, 90.0)
            || !valid_coordinate(self.longitude, -180.0, 180.0)
            || !self.generationtime_ms.is_finite()
            || !self.elevation.is_finite()
            || !(-86_400..=86_400).contains(&self.utc_offset_seconds)
            || self.timezone_abbreviation.is_empty()
            || self.timezone_abbreviation.len() > 32
        {
            return Err(SolarError::Schema("provider metadata"));
        }
        if self.daily_units.time != "unixtime" || self.daily_units.sunrise != "unixtime" {
            return Err(SolarError::Schema("daily units"));
        }

        let zone = load_time_zone(&self.timezone)?;
        let days = validated_days(
            self.daily.time,
            self.daily.sunrise,
            self.utc_offset_seconds,
            &zone,
        )?;
        let schedule = SolarSchedule {
            schema_version: CACHE_SCHEMA_VERSION,
            latitude: location.latitude,
            longitude: location.longitude,
            time_zone: self.timezone,
            fetched_at_unix,
            days,
        };
        validate_schedule(&schedule)?;
        Ok(schedule)
    }
}

fn validated_days(
    dates: Vec<i64>,
    sunrises: Vec<Option<i64>>,
    utc_offset_seconds: i32,
    zone: &TimeZone,
) -> Result<Vec<SolarDay>, SolarError> {
    if dates.len() != EXPECTED_DAYS || dates.len() != sunrises.len() {
        return Err(SolarError::Schema("daily array length"));
    }

    let mut seen = HashSet::with_capacity(dates.len());
    let mut days = Vec::with_capacity(dates.len());
    for (date_unix, sunrise_unix) in dates.into_iter().zip(sunrises) {
        let parsed = provider_date_from_unix(date_unix, utc_offset_seconds)?;
        let date = parsed.to_string();
        if !seen.insert(date.clone()) {
            return Err(SolarError::Schema("duplicate daily date"));
        }
        validate_sunrise_date(sunrise_unix, parsed, zone)?;
        days.push(SolarDay { date, sunrise_unix });
    }
    Ok(days)
}

fn provider_date_from_unix(value: i64, utc_offset_seconds: i32) -> Result<Date, SolarError> {
    let shifted = value
        .checked_add(i64::from(utc_offset_seconds))
        .ok_or(SolarError::Schema("daily date"))?;
    if shifted.rem_euclid(86_400) != 0 {
        return Err(SolarError::Schema("daily date"));
    }
    let timestamp =
        Timestamp::from_second(shifted).map_err(|_| SolarError::Schema("daily date"))?;
    Ok(timestamp.to_zoned(TimeZone::UTC).date())
}

fn canonical_date(value: &str) -> Result<Date, SolarError> {
    let date = value
        .parse::<Date>()
        .map_err(|_| SolarError::Schema("daily date"))?;
    if date.to_string() != value {
        return Err(SolarError::Schema("daily date"));
    }
    Ok(date)
}

fn validate_sunrise_date(
    sunrise_unix: Option<i64>,
    date: Date,
    zone: &TimeZone,
) -> Result<(), SolarError> {
    let Some(seconds) = sunrise_unix else {
        return Ok(());
    };
    let timestamp =
        Timestamp::from_second(seconds).map_err(|_| SolarError::Schema("sunrise timestamp"))?;
    if timestamp.to_zoned(zone.clone()).date() != date {
        return Err(SolarError::Schema("sunrise date"));
    }
    Ok(())
}

fn validate_location(location: &Location) -> Result<(), SolarError> {
    if !valid_coordinate(location.latitude, -90.0, 90.0)
        || !valid_coordinate(location.longitude, -180.0, 180.0)
    {
        return Err(SolarError::InvalidLocation);
    }
    Ok(())
}

fn valid_coordinate(value: f64, minimum: f64, maximum: f64) -> bool {
    value.is_finite() && (minimum..=maximum).contains(&value)
}

fn format_coordinate(value: f64) -> String {
    let mut formatted = value.to_string();
    match formatted.find('.') {
        Some(decimal) => {
            let fractional_digits = formatted.len() - decimal - 1;
            if fractional_digits < 4 {
                formatted.extend(std::iter::repeat_n('0', 4 - fractional_digits));
            }
        }
        None => formatted.push_str(".0000"),
    }
    formatted
}

fn validate_provider_base(provider_base: &str) -> Result<(), SolarError> {
    let url = Url::parse(provider_base).map_err(|_| SolarError::InvalidProvider)?;
    if url.scheme() != "https"
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(SolarError::InvalidProvider);
    }
    Ok(())
}

fn load_time_zone(name: &str) -> Result<TimeZone, SolarError> {
    if !valid_time_zone_name(name) {
        return Err(SolarError::Schema("time zone"));
    }
    let zone = TimeZone::get(name).map_err(|_| SolarError::Schema("time zone"))?;
    if zone.is_unknown() || zone.iana_name().is_none() {
        return Err(SolarError::Schema("time zone"));
    }
    Ok(zone)
}

fn valid_time_zone_name(name: &str) -> bool {
    if name.is_empty() || name.len() > MAX_TIME_ZONE_BYTES || !name.is_ascii() {
        return false;
    }
    name.split('/').all(|part| {
        !part.is_empty()
            && part != "."
            && part != ".."
            && part
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'+'))
    })
}

fn validate_schedule(schedule: &SolarSchedule) -> Result<(), SolarError> {
    if schedule.schema_version != CACHE_SCHEMA_VERSION {
        return Err(SolarError::Schema("cache schema version"));
    }
    if !valid_coordinate(schedule.latitude, -90.0, 90.0)
        || !valid_coordinate(schedule.longitude, -180.0, 180.0)
    {
        return Err(SolarError::Schema("cache coordinates"));
    }
    let zone = load_time_zone(&schedule.time_zone)?;
    if schedule.days.len() != EXPECTED_DAYS {
        return Err(SolarError::Schema("cached day count"));
    }

    let mut seen = HashSet::with_capacity(schedule.days.len());
    for day in &schedule.days {
        let date = canonical_date(&day.date)?;
        if !seen.insert(day.date.as_str()) {
            return Err(SolarError::Schema("duplicate cached date"));
        }
        validate_sunrise_date(day.sunrise_unix, date, &zone)?;
    }
    Ok(())
}

pub fn load_cache(path: &Path, location: &Location) -> Option<SolarSchedule> {
    validate_location(location).ok()?;
    let bytes = read_regular_nofollow(path).ok().flatten()?;
    let value: Value = serde_json::from_slice(&bytes).ok()?;
    let schedule: SolarSchedule = serde_json::from_value(value).ok()?;
    validate_schedule(&schedule).ok()?;
    if schedule.latitude != location.latitude || schedule.longitude != location.longitude {
        return None;
    }
    Some(schedule)
}

fn read_regular_nofollow(path: &Path) -> io::Result<Option<Vec<u8>>> {
    read_regular_nofollow_with(path, || {})
}

fn read_regular_nofollow_with(
    path: &Path,
    after_open: impl FnOnce(),
) -> io::Result<Option<Vec<u8>>> {
    let mut file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)?;
    let metadata = file.metadata()?;
    if !metadata.is_file() {
        return Ok(None);
    }
    after_open();
    let mut bytes = Vec::with_capacity(usize::try_from(metadata.len()).unwrap_or(0));
    file.read_to_end(&mut bytes)?;
    Ok(Some(bytes))
}

pub fn save_cache(path: &Path, schedule: &SolarSchedule) -> Result<(), SolarError> {
    validate_schedule(schedule)?;
    let mut serialized = serde_json::to_vec_pretty(schedule).map_err(|_| SolarError::Json)?;
    serialized.push(b'\n');

    let parent = parent_directory(path);
    create_parent_if_missing(parent)?;
    let mut temporary = tempfile::Builder::new()
        .prefix(".planeradar-solar-cache-")
        .tempfile_in(parent)?;
    temporary.write_all(&serialized)?;
    temporary.flush()?;
    temporary.as_file().sync_all()?;
    temporary.persist(path)?;
    File::open(parent)?.sync_all()?;
    Ok(())
}

fn parent_directory(path: &Path) -> &Path {
    path.parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."))
}

fn create_parent_if_missing(parent: &Path) -> Result<(), SolarError> {
    if parent.exists() {
        return Ok(());
    }
    fs::create_dir_all(parent)?;
    fs::set_permissions(parent, fs::Permissions::from_mode(0o750))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::symlink;

    use super::*;

    #[test]
    fn cache_reader_rejects_a_symlink_at_the_single_open() {
        let temporary = tempfile::tempdir().expect("cache directory");
        let target = temporary.path().join("target.json");
        let linked = temporary.path().join("cache.json");
        fs::write(&target, b"target bytes").expect("target");
        symlink(&target, &linked).expect("symlink");

        assert!(read_regular_nofollow_with(&linked, || {}).is_err());
    }

    #[test]
    fn cache_reader_keeps_reading_the_open_descriptor_after_path_replacement() {
        let temporary = tempfile::tempdir().expect("cache directory");
        let cache = temporary.path().join("cache.json");
        let replacement = temporary.path().join("replacement.json");
        fs::write(&cache, b"opened cache bytes").expect("cache");
        fs::write(&replacement, b"replacement bytes").expect("replacement");

        let bytes = read_regular_nofollow_with(&cache, || {
            fs::remove_file(&cache).expect("unlink opened cache");
            symlink(&replacement, &cache).expect("replacement symlink");
        })
        .expect("descriptor read")
        .expect("regular file");

        assert_eq!(bytes, b"opened cache bytes");
        assert_eq!(
            fs::read(&cache).expect("path now follows replacement"),
            b"replacement bytes"
        );
    }
}

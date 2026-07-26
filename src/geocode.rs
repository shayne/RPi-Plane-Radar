use std::collections::BTreeMap;
use std::fs::{self, File};
use std::io::{self, Write};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;

use crate::http::{HttpClient, HttpError, HttpRequest};
use crate::model::Location;
use crate::time::{Clock, Sleeper};

const DEFAULT_PROVIDER_BASE: &str = "https://nominatim.openstreetmap.org/search";
const USER_AGENT: &str = "RPi-Plane-Radar/0.1 (+https://github.com/shayne/RPi-Plane-Radar)";
const CONNECT_TIMEOUT: Duration = Duration::from_millis(3050);
const READ_TIMEOUT: Duration = Duration::from_secs(10);
const MINIMUM_REQUEST_INTERVAL: Duration = Duration::from_millis(1050);
const CACHE_TTL_SECONDS: u64 = 7 * 24 * 60 * 60;
const CACHE_SCHEMA_VERSION: u32 = 1;
const MAX_RESULTS: usize = 5;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GeocodeResult {
    pub display_name: String,
    pub location: Location,
}

pub trait GeocodeService: Send {
    fn search(&mut self, query: &str) -> Result<Vec<GeocodeResult>, GeocodeError>;
}

#[derive(Debug, Error)]
pub enum GeocodeError {
    #[error("geocode query must be non-blank and contain no control characters")]
    InvalidQuery,
    #[error("geocoding HTTP request failed: {0}")]
    Http(#[from] HttpError),
    #[error("geocoding service returned HTTP status {0}")]
    Status(u16),
    #[error("geocoding response was not valid JSON: {0}")]
    Json(#[from] serde_json::Error),
    #[error("geocoding response schema was invalid: {0}")]
    Schema(&'static str),
    #[error("geocode cache failed: {0}")]
    Cache(#[from] GeocodeCacheError),
}

#[derive(Debug, Error)]
pub enum GeocodeCacheError {
    #[error("invalid cache: {0}")]
    Invalid(&'static str),
    #[error("failed to read or write cache: {0}")]
    Io(#[from] io::Error),
    #[error("failed to parse or serialize cache JSON: {0}")]
    Json(#[from] serde_json::Error),
    #[error("failed to atomically persist cache: {0}")]
    Persist(#[from] tempfile::PersistError),
}

pub struct Geocoder<C, K, S> {
    http: C,
    clock: K,
    sleeper: S,
    cache_path: PathBuf,
    provider_base: String,
    cache: Option<CacheFile>,
    last_request_at: Option<Duration>,
}

impl<C: HttpClient, K: Clock, S: Sleeper> Geocoder<C, K, S> {
    pub fn new(http: C, clock: K, sleeper: S, cache_path: PathBuf) -> Self {
        Self::with_provider_base(
            http,
            clock,
            sleeper,
            cache_path,
            DEFAULT_PROVIDER_BASE.to_owned(),
        )
    }

    pub fn with_provider_base(
        http: C,
        clock: K,
        sleeper: S,
        cache_path: PathBuf,
        provider_base: String,
    ) -> Self {
        Self {
            http,
            clock,
            sleeper,
            cache_path,
            provider_base,
            cache: None,
            last_request_at: None,
        }
    }

    pub fn search(&mut self, query: &str) -> Result<Vec<GeocodeResult>, GeocodeError> {
        let normalized_query = normalize_query(query)?;
        self.ensure_cache_loaded()?;
        let now_unix = self.clock.unix_seconds();
        if let Some(entry) = self
            .cache
            .as_ref()
            .and_then(|cache| cache.entries.get(&normalized_query))
            .filter(|entry| now_unix < entry.expires_at_unix)
        {
            return Ok(entry.results.clone());
        }

        self.wait_for_rate_limit();
        let request = HttpRequest {
            url: self.provider_base.clone(),
            query: vec![
                ("q".to_owned(), query.to_owned()),
                ("format".to_owned(), "jsonv2".to_owned()),
                ("limit".to_owned(), MAX_RESULTS.to_string()),
                ("addressdetails".to_owned(), "0".to_owned()),
            ],
            headers: vec![("User-Agent".to_owned(), USER_AGENT.to_owned())],
            connect_timeout: CONNECT_TIMEOUT,
            read_timeout: READ_TIMEOUT,
            verify_tls: true,
        };
        let response = self.http.execute(request)?;
        if response.status != 200 {
            return Err(GeocodeError::Status(response.status));
        }

        let value: Value = serde_json::from_slice(&response.body)?;
        let results = parse_results(&value)?;
        if results.is_empty() {
            return Ok(results);
        }

        let expires_at_unix = self.clock.unix_seconds().saturating_add(CACHE_TTL_SECONDS);
        let mut updated = self.cache.clone().expect("cache must be loaded");
        updated.entries.insert(
            normalized_query,
            CacheEntry {
                results: results.clone(),
                expires_at_unix,
            },
        );
        save_cache(&self.cache_path, &updated)?;
        self.cache = Some(updated);
        Ok(results)
    }

    fn ensure_cache_loaded(&mut self) -> Result<(), GeocodeCacheError> {
        if self.cache.is_none() {
            self.cache = Some(load_cache(&self.cache_path)?);
        }
        Ok(())
    }

    fn wait_for_rate_limit(&mut self) {
        let now = self.clock.monotonic();
        if let Some(last_request_at) = self.last_request_at {
            let elapsed = now.saturating_sub(last_request_at);
            if elapsed < MINIMUM_REQUEST_INTERVAL {
                self.sleeper.sleep(MINIMUM_REQUEST_INTERVAL - elapsed);
            }
        }
        self.last_request_at = Some(self.clock.monotonic());
    }
}

impl<C: HttpClient, K: Clock, S: Sleeper> GeocodeService for Geocoder<C, K, S> {
    fn search(&mut self, query: &str) -> Result<Vec<GeocodeResult>, GeocodeError> {
        Geocoder::search(self, query)
    }
}

fn normalize_query(query: &str) -> Result<String, GeocodeError> {
    normalized_text(query).ok_or(GeocodeError::InvalidQuery)
}

fn normalized_text(query: &str) -> Option<String> {
    if query.chars().any(char::is_control) {
        return None;
    }
    let normalized = query.split_whitespace().collect::<Vec<_>>().join(" ");
    if normalized.is_empty() {
        return None;
    }
    Some(normalized.to_lowercase())
}

fn parse_results(value: &Value) -> Result<Vec<GeocodeResult>, GeocodeError> {
    let records = value
        .as_array()
        .ok_or(GeocodeError::Schema("top level must be an array"))?;
    let mut results = Vec::with_capacity(MAX_RESULTS.min(records.len()));
    for record in records {
        if results.len() == MAX_RESULTS {
            break;
        }
        let Some(record) = record.as_object() else {
            continue;
        };
        let Some(display_name) = record
            .get("display_name")
            .and_then(Value::as_str)
            .filter(|display_name| !display_name.trim().is_empty())
        else {
            continue;
        };
        let Some(latitude) = coordinate(record.get("lat"), -90.0, 90.0) else {
            continue;
        };
        let Some(longitude) = coordinate(record.get("lon"), -180.0, 180.0) else {
            continue;
        };

        results.push(GeocodeResult {
            display_name: display_name.to_owned(),
            location: Location {
                latitude,
                longitude,
                label: display_name.to_owned(),
            },
        });
    }
    Ok(results)
}

fn coordinate(value: Option<&Value>, minimum: f64, maximum: f64) -> Option<f64> {
    let coordinate = value?.as_str()?.trim().parse::<f64>().ok()?;
    coordinate
        .is_finite()
        .then_some(coordinate)
        .filter(|coordinate| (minimum..=maximum).contains(coordinate))
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct CacheFile {
    schema_version: u32,
    entries: BTreeMap<String, CacheEntry>,
}

impl Default for CacheFile {
    fn default() -> Self {
        Self {
            schema_version: CACHE_SCHEMA_VERSION,
            entries: BTreeMap::new(),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct CacheEntry {
    results: Vec<GeocodeResult>,
    expires_at_unix: u64,
}

fn load_cache(path: &Path) -> Result<CacheFile, GeocodeCacheError> {
    let cache: CacheFile = match fs::read(path) {
        Ok(bytes) => serde_json::from_slice(&bytes)?,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(CacheFile::default()),
        Err(error) => return Err(error.into()),
    };
    if cache.schema_version != CACHE_SCHEMA_VERSION {
        return Err(GeocodeCacheError::Invalid("unsupported schema version"));
    }
    validate_cache_records(&cache)?;
    Ok(cache)
}

fn validate_cache_records(cache: &CacheFile) -> Result<(), GeocodeCacheError> {
    for (key, entry) in &cache.entries {
        if normalized_text(key).as_deref() != Some(key.as_str()) {
            return Err(GeocodeCacheError::Invalid("cache key is not normalized"));
        }
        if entry.results.is_empty() || entry.results.len() > MAX_RESULTS {
            return Err(GeocodeCacheError::Invalid(
                "cached result count must be between one and five",
            ));
        }
        for result in &entry.results {
            if result.display_name.trim().is_empty() {
                return Err(GeocodeCacheError::Invalid(
                    "cached display name must be non-blank",
                ));
            }
            if result.location.label != result.display_name {
                return Err(GeocodeCacheError::Invalid(
                    "cached location label must match display name",
                ));
            }
            if !valid_coordinate(result.location.latitude, -90.0, 90.0)
                || !valid_coordinate(result.location.longitude, -180.0, 180.0)
            {
                return Err(GeocodeCacheError::Invalid("cached coordinates are invalid"));
            }
        }
    }
    Ok(())
}

fn valid_coordinate(coordinate: f64, minimum: f64, maximum: f64) -> bool {
    coordinate.is_finite() && (minimum..=maximum).contains(&coordinate)
}

fn save_cache(path: &Path, cache: &CacheFile) -> Result<(), GeocodeCacheError> {
    let mut serialized = serde_json::to_vec_pretty(cache)?;
    serialized.push(b'\n');

    let parent = parent_directory(path);
    create_parent_if_missing(parent)?;

    let mut temporary = tempfile::Builder::new()
        .prefix(".planeradar-geocode-cache-")
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

fn create_parent_if_missing(parent: &Path) -> Result<(), GeocodeCacheError> {
    if parent.exists() {
        return Ok(());
    }

    fs::create_dir_all(parent)?;
    fs::set_permissions(parent, fs::Permissions::from_mode(0o750))?;
    Ok(())
}

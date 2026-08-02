use std::time::Duration;

use serde_json::{Map, Value};
use thiserror::Error;

use crate::http::{HttpClient, HttpError, HttpRequest};
use crate::model::{Aircraft, Location};

const API_BASE: &str = "https://opendata.adsb.fi/api/v3";
const KM_PER_NAUTICAL_MILE: f64 = 1.852;
const MAX_AIRCRAFT: usize = 64;
const CONNECT_TIMEOUT: Duration = Duration::from_millis(3050);
const READ_TIMEOUT: Duration = Duration::from_secs(10);
const MAX_RESPONSE_BYTES: usize = 2 * 1024 * 1024;

#[derive(Debug, Error)]
pub enum AdsbError {
    #[error("ADS-B HTTP request failed: {0}")]
    Http(#[from] HttpError),
    #[error("ADS-B service returned HTTP status {0}")]
    Status(u16),
    #[error("ADS-B response was not valid JSON: {0}")]
    Json(#[from] serde_json::Error),
    #[error("ADS-B response schema was invalid: {0}")]
    Schema(&'static str),
}

pub struct AdsbClient<C> {
    http: C,
}

impl<C: HttpClient> AdsbClient<C> {
    pub fn new(http: C) -> Self {
        Self { http }
    }

    pub fn fetch(&self, location: &Location, radius_km: f64) -> Result<Vec<Aircraft>, AdsbError> {
        let nautical_miles = radius_km / KM_PER_NAUTICAL_MILE;
        let request = HttpRequest {
            url: format!(
                "{API_BASE}/lat/{:.6}/lon/{:.6}/dist/{:.1}",
                location.latitude, location.longitude, nautical_miles
            ),
            query: Vec::new(),
            headers: Vec::new(),
            connect_timeout: CONNECT_TIMEOUT,
            read_timeout: READ_TIMEOUT,
            max_response_bytes: MAX_RESPONSE_BYTES,
            verify_tls: true,
        };
        let response = self.http.execute(request)?;
        if response.status != 200 {
            return Err(AdsbError::Status(response.status));
        }

        let value = serde_json::from_slice(&response.body)?;
        parse_aircraft(&value, MAX_AIRCRAFT, false)
    }
}

pub fn parse_aircraft(
    value: &Value,
    max: usize,
    show_ground: bool,
) -> Result<Vec<Aircraft>, AdsbError> {
    let root = value
        .as_object()
        .ok_or(AdsbError::Schema("top level must be an object"))?;
    let records = match root.get("ac") {
        None | Some(Value::Null) => return Ok(Vec::new()),
        Some(Value::Array(records)) => records,
        Some(_) => return Err(AdsbError::Schema("ac must be an array")),
    };

    let mut aircraft = Vec::with_capacity(max.min(records.len()));
    for record in records {
        if aircraft.len() == max {
            break;
        }
        let Some(record) = record.as_object() else {
            continue;
        };
        let (Some(latitude), Some(longitude)) = (number(record, "lat"), number(record, "lon"))
        else {
            continue;
        };
        if is_on_ground(record) && !show_ground {
            continue;
        }

        aircraft.push(Aircraft {
            latitude,
            longitude,
            nose_degrees: first_number(record, &["true_heading", "mag_heading", "track", "dir"])
                .unwrap_or(0.0),
            track_degrees: first_number(record, &["track", "true_heading", "mag_heading", "dir"])
                .unwrap_or(0.0),
            ground_speed_knots: first_number(record, &["gs", "tas", "ias"]).unwrap_or(0.0),
            callsign: callsign(record),
            aircraft_type: string(record, "t").unwrap_or_default().to_owned(),
            altitude: altitude(record),
        });
    }

    Ok(aircraft)
}

fn number(record: &Map<String, Value>, key: &str) -> Option<f64> {
    record.get(key)?.as_f64().filter(|value| value.is_finite())
}

fn first_number(record: &Map<String, Value>, keys: &[&str]) -> Option<f64> {
    keys.iter().find_map(|key| number(record, key))
}

fn string<'a>(record: &'a Map<String, Value>, key: &str) -> Option<&'a str> {
    record.get(key)?.as_str().map(str::trim)
}

fn callsign(record: &Map<String, Value>) -> String {
    string(record, "flight")
        .filter(|flight| !flight.is_empty())
        .or_else(|| string(record, "hex"))
        .unwrap_or_default()
        .to_owned()
}

fn is_on_ground(record: &Map<String, Value>) -> bool {
    record.get("alt_baro").and_then(Value::as_str) == Some("ground")
}

fn altitude(record: &Map<String, Value>) -> String {
    if is_on_ground(record) {
        return "GND".to_owned();
    }
    first_number(record, &["alt_baro", "alt_geom"])
        .map(|altitude| format!("{:.0} ft", altitude.round()))
        .unwrap_or_default()
}

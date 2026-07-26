use std::sync::Arc;
use std::time::Duration;

use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RadarSettings {
    pub schema_version: u32,
    pub location: Option<Location>,
    pub units: Units,
    pub show_runways: bool,
    pub range_index: u8,
}

impl Default for RadarSettings {
    fn default() -> Self {
        Self {
            schema_version: 1,
            location: None,
            units: Units::Kilometres,
            show_runways: true,
            range_index: 1,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Units {
    #[serde(rename = "km")]
    Kilometres,
    #[serde(rename = "mi")]
    Miles,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Location {
    pub latitude: f64,
    pub longitude: f64,
    #[serde(default)]
    pub label: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct GeoPoint {
    pub latitude: f64,
    pub longitude: f64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Aircraft {
    pub latitude: f64,
    pub longitude: f64,
    pub nose_degrees: f64,
    pub track_degrees: f64,
    pub ground_speed_knots: f64,
    pub callsign: String,
    pub aircraft_type: String,
    pub altitude: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Runway {
    pub low_end: GeoPoint,
    pub high_end: GeoPoint,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Airport {
    pub ident: String,
    pub location: GeoPoint,
    pub runways: Vec<Runway>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct RadarSnapshot {
    pub aircraft: Arc<[Aircraft]>,
    pub fetched_at: Option<Duration>,
    pub last_error_at: Option<Duration>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum AppState {
    SetupRequired,
    WaitingForNetwork,
    Radar,
    Settings,
}

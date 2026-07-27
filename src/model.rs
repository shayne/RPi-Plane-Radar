use std::sync::{Arc, RwLock};
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

#[derive(Clone, Debug)]
pub struct RuntimeSnapshot {
    pub settings: RadarSettings,
    pub aircraft: Arc<[Aircraft]>,
    pub fetched_at: Option<Duration>,
    pub has_successful_fetch_for_current_location: bool,
    pub last_error_at: Option<Duration>,
    pub local_url: String,
    pub ip_url: Option<String>,
    pub generation: u64,
}

#[derive(Clone)]
pub struct RuntimeModel {
    snapshot: Arc<RwLock<RuntimeSnapshot>>,
}

impl RuntimeModel {
    pub fn new(settings: RadarSettings, local_url: String) -> Self {
        Self {
            snapshot: Arc::new(RwLock::new(RuntimeSnapshot {
                settings,
                aircraft: Arc::from([]),
                fetched_at: None,
                has_successful_fetch_for_current_location: false,
                last_error_at: None,
                local_url,
                ip_url: None,
                generation: 0,
            })),
        }
    }

    pub fn snapshot(&self) -> RuntimeSnapshot {
        self.snapshot.read().expect("runtime model lock").clone()
    }

    pub fn replace_settings(&self, settings: RadarSettings) -> u64 {
        let mut snapshot = self.snapshot.write().expect("runtime model lock");
        let location_changed = snapshot.settings.location != settings.location;
        let query_changed =
            location_changed || snapshot.settings.range_index != settings.range_index;
        snapshot.settings = settings;
        if query_changed {
            snapshot.aircraft = Arc::from([]);
            snapshot.fetched_at = None;
        }
        if location_changed {
            snapshot.has_successful_fetch_for_current_location = false;
        }
        bump(&mut snapshot)
    }

    pub fn record_aircraft(&self, aircraft: Vec<Aircraft>, fetched_at: Duration) -> u64 {
        let mut snapshot = self.snapshot.write().expect("runtime model lock");
        snapshot.aircraft = Arc::from(aircraft);
        snapshot.fetched_at = Some(fetched_at);
        snapshot.has_successful_fetch_for_current_location = true;
        bump(&mut snapshot)
    }

    pub fn record_aircraft_if_query(
        &self,
        expected_location: &Location,
        expected_range_index: u8,
        aircraft: Vec<Aircraft>,
        fetched_at: Duration,
    ) -> Option<u64> {
        let mut snapshot = self.snapshot.write().expect("runtime model lock");
        if snapshot.settings.location.as_ref() != Some(expected_location)
            || snapshot.settings.range_index != expected_range_index
        {
            return None;
        }
        snapshot.aircraft = Arc::from(aircraft);
        snapshot.fetched_at = Some(fetched_at);
        snapshot.has_successful_fetch_for_current_location = true;
        Some(bump(&mut snapshot))
    }

    pub fn record_adsb_error(&self, at: Duration) -> u64 {
        let mut snapshot = self.snapshot.write().expect("runtime model lock");
        snapshot.last_error_at = Some(at);
        bump(&mut snapshot)
    }

    pub fn record_adsb_error_if_query(
        &self,
        expected_location: &Location,
        expected_range_index: u8,
        at: Duration,
    ) -> Option<u64> {
        let mut snapshot = self.snapshot.write().expect("runtime model lock");
        if snapshot.settings.location.as_ref() != Some(expected_location)
            || snapshot.settings.range_index != expected_range_index
        {
            return None;
        }
        snapshot.last_error_at = Some(at);
        Some(bump(&mut snapshot))
    }

    pub fn set_urls(&self, local_url: String, ip_url: Option<String>) -> u64 {
        let mut snapshot = self.snapshot.write().expect("runtime model lock");
        snapshot.local_url = local_url;
        snapshot.ip_url = ip_url;
        bump(&mut snapshot)
    }
}

fn bump(snapshot: &mut RuntimeSnapshot) -> u64 {
    snapshot.generation = snapshot.generation.saturating_add(1);
    snapshot.generation
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum AppState {
    SetupRequired,
    WaitingForNetwork,
    Radar,
    Settings,
}

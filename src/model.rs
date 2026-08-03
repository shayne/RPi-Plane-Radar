use std::collections::{HashMap, HashSet};
use std::sync::{Arc, RwLock};
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::flight_data::{AircraftEnrichment, EnrichmentNeeds};

pub const SETTINGS_SCHEMA_VERSION: u32 = 3;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TemperatureUnit {
    Celsius,
    Fahrenheit,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TimeZone {
    RadarLocal,
    Zulu,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClockFormat {
    Twelve,
    TwentyFour,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FooterSettings {
    pub show_condition: bool,
    pub show_temperature: bool,
    pub show_humidity: bool,
    pub show_time: bool,
    pub show_date: bool,
    pub temperature_unit: TemperatureUnit,
    pub time_zone: TimeZone,
    pub clock_format: ClockFormat,
}

impl Default for FooterSettings {
    fn default() -> Self {
        Self {
            show_condition: false,
            show_temperature: false,
            show_humidity: false,
            show_time: false,
            show_date: false,
            temperature_unit: TemperatureUnit::Celsius,
            time_zone: TimeZone::RadarLocal,
            clock_format: ClockFormat::TwentyFour,
        }
    }
}

impl FooterSettings {
    pub fn any_visible(&self) -> bool {
        self.show_condition
            || self.show_temperature
            || self.show_humidity
            || self.show_time
            || self.show_date
    }

    pub fn needs_environment(&self) -> bool {
        self.show_condition
            || self.show_temperature
            || self.show_humidity
            || ((self.show_time || self.show_date) && self.time_zone == TimeZone::RadarLocal)
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BrightnessSettings {
    pub day_percent: u8,
    pub night: NightModeSettings,
}

impl Default for BrightnessSettings {
    fn default() -> Self {
        Self {
            day_percent: 100,
            night: NightModeSettings::default(),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NightModeSettings {
    pub enabled: bool,
    pub brightness_percent: u8,
    pub start_hour: u8,
    pub start_minute: u8,
    pub red_mode: bool,
}

impl Default for NightModeSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            brightness_percent: 30,
            start_hour: 20,
            start_minute: 0,
            red_mode: false,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FrameColorMode {
    FullColor,
    RedOnly,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DisplayPeriod {
    Day,
    Night,
}

/// The next instant at which display output should enter `period`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Transition {
    pub at_unix: i64,
    pub period: DisplayPeriod,
}

/// Immutable wall-clock facts used to describe a resolved night interval.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ScheduleFacts {
    pub start_unix: i64,
    pub end_unix: i64,
}

/// Human-facing solar state. `Fallback` may describe either the active night
/// or the upcoming night; `DisplayPolicy::period` distinguishes those cases.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SolarStatus {
    Disabled,
    Waiting,
    Upcoming(ScheduleFacts),
    Active(ScheduleFacts),
    Fallback(ScheduleFacts),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DisplayPolicy {
    pub period: DisplayPeriod,
    pub brightness_percent: u8,
    pub color_mode: FrameColorMode,
    pub next_transition: Option<Transition>,
    pub solar_status: SolarStatus,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RadarSettings {
    pub schema_version: u32,
    pub location: Option<Location>,
    pub units: Units,
    pub show_runways: bool,
    pub range_index: u8,
    pub show_callsign: bool,
    pub show_route: bool,
    pub show_expanded_model: bool,
    pub radar_text_scale_percent: u8,
    pub minimum_altitude_feet: Option<i32>,
    pub maximum_altitude_feet: Option<i32>,
    pub footer: FooterSettings,
    pub brightness: BrightnessSettings,
}

impl Default for RadarSettings {
    fn default() -> Self {
        Self {
            schema_version: SETTINGS_SCHEMA_VERSION,
            location: None,
            units: Units::Kilometres,
            show_runways: true,
            range_index: 1,
            show_callsign: true,
            show_route: false,
            show_expanded_model: false,
            radar_text_scale_percent: 100,
            minimum_altitude_feet: None,
            maximum_altitude_feet: None,
            footer: FooterSettings::default(),
            brightness: BrightnessSettings::default(),
        }
    }
}

impl RadarSettings {
    pub fn altitude_filter_active(&self) -> bool {
        self.minimum_altitude_feet.is_some() || self.maximum_altitude_feet.is_some()
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
    pub hex: String,
    pub flight_callsign: String,
    pub latitude: f64,
    pub longitude: f64,
    pub nose_degrees: f64,
    pub track_degrees: f64,
    pub ground_speed_knots: f64,
    pub callsign: String,
    pub aircraft_type: String,
    pub altitude_feet: Option<i32>,
    pub altitude: String,
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct AircraftKey {
    pub hex: String,
    pub callsign: String,
}

impl Aircraft {
    pub fn key(&self) -> AircraftKey {
        AircraftKey {
            hex: self.hex.clone(),
            callsign: self.flight_callsign.clone(),
        }
    }
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
    pub enrichment: Arc<HashMap<AircraftKey, AircraftEnrichment>>,
    pub environment: Option<EnvironmentReading>,
    pub fetched_at: Option<Duration>,
    pub last_error_at: Option<Duration>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct EnvironmentReading {
    pub temperature_celsius: f64,
    pub humidity_percent: u8,
    pub weather_code: u8,
    pub utc_offset_seconds: i32,
    pub fetched_at: Duration,
}

#[derive(Clone, Debug)]
pub struct RuntimeSnapshot {
    pub settings: RadarSettings,
    pub aircraft: Arc<[Aircraft]>,
    pub enrichment: Arc<HashMap<AircraftKey, AircraftEnrichment>>,
    pub environment: Option<EnvironmentReading>,
    pub environment_last_error_at: Option<Duration>,
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
                enrichment: Arc::new(HashMap::new()),
                environment: None,
                environment_last_error_at: None,
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
        let old_filter = crate::adsb::AltitudeFilter::from(&snapshot.settings);
        let new_filter = crate::adsb::AltitudeFilter::from(&settings);
        snapshot.settings = settings;
        if query_changed {
            snapshot.aircraft = Arc::from([]);
            snapshot.fetched_at = None;
        } else if old_filter != new_filter {
            snapshot.aircraft = snapshot
                .aircraft
                .iter()
                .filter(|aircraft| new_filter.allows(aircraft.altitude_feet))
                .cloned()
                .collect::<Vec<_>>()
                .into();
        }
        retain_displayed_enrichment(&mut snapshot);
        if location_changed {
            snapshot.environment = None;
            snapshot.environment_last_error_at = None;
            snapshot.has_successful_fetch_for_current_location = false;
        }
        bump(&mut snapshot)
    }

    pub fn record_aircraft(&self, aircraft: Vec<Aircraft>, fetched_at: Duration) -> u64 {
        let mut snapshot = self.snapshot.write().expect("runtime model lock");
        snapshot.aircraft = Arc::from(aircraft);
        retain_displayed_enrichment(&mut snapshot);
        snapshot.fetched_at = Some(fetched_at);
        snapshot.has_successful_fetch_for_current_location = true;
        bump(&mut snapshot)
    }

    pub fn record_aircraft_if_query(
        &self,
        expected_location: &Location,
        expected_range_index: u8,
        expected_filter: crate::adsb::AltitudeFilter,
        aircraft: Vec<Aircraft>,
        fetched_at: Duration,
    ) -> Option<u64> {
        let mut snapshot = self.snapshot.write().expect("runtime model lock");
        if snapshot.settings.location.as_ref() != Some(expected_location)
            || snapshot.settings.range_index != expected_range_index
            || crate::adsb::AltitudeFilter::from(&snapshot.settings) != expected_filter
        {
            return None;
        }
        snapshot.aircraft = Arc::from(aircraft);
        retain_displayed_enrichment(&mut snapshot);
        snapshot.fetched_at = Some(fetched_at);
        snapshot.has_successful_fetch_for_current_location = true;
        Some(bump(&mut snapshot))
    }

    pub fn record_enrichment_if_aircraft(
        &self,
        key: &AircraftKey,
        enrichment: AircraftEnrichment,
    ) -> Option<u64> {
        let mut snapshot = self.snapshot.write().expect("runtime model lock");
        record_enrichment(&mut snapshot, key, enrichment)
    }

    pub fn record_enrichment_if_current(
        &self,
        expected_location: &Location,
        expected_needs: EnrichmentNeeds,
        key: &AircraftKey,
        enrichment: AircraftEnrichment,
    ) -> Option<u64> {
        let mut snapshot = self.snapshot.write().expect("runtime model lock");
        let current_needs = EnrichmentNeeds {
            route: snapshot.settings.show_route,
            model: snapshot.settings.show_expanded_model,
        };
        if snapshot.settings.location.as_ref() != Some(expected_location)
            || current_needs != expected_needs
        {
            return None;
        }
        record_enrichment(&mut snapshot, key, enrichment)
    }

    pub fn record_environment_if_location(
        &self,
        expected_location: &Location,
        reading: EnvironmentReading,
    ) -> Option<u64> {
        let mut snapshot = self.snapshot.write().expect("runtime model lock");
        if snapshot.settings.location.as_ref() != Some(expected_location) {
            return None;
        }
        snapshot.environment = Some(reading);
        snapshot.environment_last_error_at = None;
        Some(bump(&mut snapshot))
    }

    pub fn record_environment_if_current(
        &self,
        expected_location: &Location,
        reading: EnvironmentReading,
    ) -> Option<u64> {
        let mut snapshot = self.snapshot.write().expect("runtime model lock");
        if snapshot.settings.location.as_ref() != Some(expected_location)
            || !snapshot.settings.footer.needs_environment()
        {
            return None;
        }
        snapshot.environment = Some(reading);
        snapshot.environment_last_error_at = None;
        Some(bump(&mut snapshot))
    }

    pub fn record_environment_error_if_location(
        &self,
        expected_location: &Location,
        at: Duration,
    ) -> Option<u64> {
        let mut snapshot = self.snapshot.write().expect("runtime model lock");
        if snapshot.settings.location.as_ref() != Some(expected_location) {
            return None;
        }
        snapshot.environment_last_error_at = Some(at);
        Some(bump(&mut snapshot))
    }

    pub fn record_environment_error_if_current(
        &self,
        expected_location: &Location,
        at: Duration,
    ) -> Option<u64> {
        let mut snapshot = self.snapshot.write().expect("runtime model lock");
        if snapshot.settings.location.as_ref() != Some(expected_location)
            || !snapshot.settings.footer.needs_environment()
        {
            return None;
        }
        snapshot.environment_last_error_at = Some(at);
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
        expected_filter: crate::adsb::AltitudeFilter,
        at: Duration,
    ) -> Option<u64> {
        let mut snapshot = self.snapshot.write().expect("runtime model lock");
        if snapshot.settings.location.as_ref() != Some(expected_location)
            || snapshot.settings.range_index != expected_range_index
            || crate::adsb::AltitudeFilter::from(&snapshot.settings) != expected_filter
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

fn record_enrichment(
    snapshot: &mut RuntimeSnapshot,
    key: &AircraftKey,
    enrichment: AircraftEnrichment,
) -> Option<u64> {
    if !snapshot
        .aircraft
        .iter()
        .any(|aircraft| aircraft.hex == key.hex && aircraft.flight_callsign == key.callsign)
        || snapshot.enrichment.get(key) == Some(&enrichment)
    {
        return None;
    }
    Arc::make_mut(&mut snapshot.enrichment).insert(key.clone(), enrichment);
    Some(bump(snapshot))
}

fn retain_displayed_enrichment(snapshot: &mut RuntimeSnapshot) {
    let displayed_keys = snapshot
        .aircraft
        .iter()
        .map(Aircraft::key)
        .collect::<HashSet<_>>();
    Arc::make_mut(&mut snapshot.enrichment).retain(|key, _| displayed_keys.contains(key));
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

use std::collections::HashMap;
use std::time::Duration;

pub use crate::route_confidence::RouteCandidate;

use serde::Deserialize;
use serde_json::Value;
use thiserror::Error;
use url::Url;

use crate::http::{HttpClient, HttpError, HttpRequest, HttpResponse};
use crate::model::Aircraft;
use crate::time::{Clock, Sleeper};

const CONNECT_TIMEOUT: Duration = Duration::from_secs(2);
const READ_TIMEOUT: Duration = Duration::from_secs(3);
const MAX_RESPONSE_BYTES: usize = 256 * 1024;
const MINIMUM_REQUEST_INTERVAL: Duration = Duration::from_millis(750);
const SUCCESS_TTL: Duration = Duration::from_secs(6 * 60 * 60);
const MISSING_TTL: Duration = Duration::from_secs(10 * 60);
const MODE_S_HEX_LENGTH: usize = 6;
const FLIGHT_CALLSIGN_LENGTH: usize = 8;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct EnrichmentNeeds {
    pub route: bool,
    pub model: bool,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct AircraftEnrichment {
    pub route: Option<String>,
    pub model: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum LookupValue<T> {
    NotRequested,
    Found(T),
    Missing,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FlightLookup {
    pub route: LookupValue<String>,
    pub model: LookupValue<String>,
}

pub struct CacheResolution {
    pub enrichment: AircraftEnrichment,
    pub pending: EnrichmentNeeds,
}

pub struct EnrichmentCache {
    route_entries: HashMap<String, CacheEntry>,
    model_entries: HashMap<String, CacheEntry>,
    capacity: usize,
    access_serial: u64,
}

#[derive(Clone, Debug)]
struct CacheEntry {
    value: Option<String>,
    expires_at: Duration,
    access_serial: u64,
}

impl EnrichmentCache {
    pub fn new(capacity: usize) -> Self {
        Self {
            route_entries: HashMap::with_capacity(capacity),
            model_entries: HashMap::with_capacity(capacity),
            capacity,
            access_serial: 0,
        }
    }

    pub fn resolve(
        &mut self,
        aircraft: &Aircraft,
        needs: EnrichmentNeeds,
        now: Duration,
    ) -> CacheResolution {
        self.remove_expired(now);
        let key = normalized_aircraft_key(aircraft);
        let mut resolution = CacheResolution {
            enrichment: AircraftEnrichment::default(),
            pending: EnrichmentNeeds::default(),
        };

        if needs.route {
            let serial = self.next_access_serial();
            match self.route_entries.get_mut(&key.callsign) {
                Some(entry) => {
                    entry.access_serial = serial;
                    resolution.enrichment.route = entry.value.clone();
                }
                None => resolution.pending.route = true,
            }
        }
        if needs.model {
            let serial = self.next_access_serial();
            match self.model_entries.get_mut(&key.hex) {
                Some(entry) => {
                    entry.access_serial = serial;
                    resolution.enrichment.model = entry.value.clone();
                }
                None => resolution.pending.model = true,
            }
        }

        resolution
    }

    pub fn record(
        &mut self,
        aircraft: &Aircraft,
        requested: EnrichmentNeeds,
        lookup: &FlightLookup,
        now: Duration,
    ) {
        self.remove_expired(now);
        let key = normalized_aircraft_key(aircraft);

        if requested.route
            && let Some((value, ttl)) = cache_value(&lookup.route)
        {
            let access_serial = self.next_access_serial();
            self.route_entries.insert(
                key.callsign,
                CacheEntry {
                    value,
                    expires_at: now.saturating_add(ttl),
                    access_serial,
                },
            );
            evict_lru(&mut self.route_entries, self.capacity);
        }
        if requested.model
            && let Some((value, ttl)) = cache_value(&lookup.model)
        {
            let access_serial = self.next_access_serial();
            self.model_entries.insert(
                key.hex,
                CacheEntry {
                    value,
                    expires_at: now.saturating_add(ttl),
                    access_serial,
                },
            );
            evict_lru(&mut self.model_entries, self.capacity);
        }
    }

    fn remove_expired(&mut self, now: Duration) {
        self.route_entries.retain(|_, entry| now < entry.expires_at);
        self.model_entries.retain(|_, entry| now < entry.expires_at);
    }

    fn next_access_serial(&mut self) -> u64 {
        if self.access_serial == u64::MAX {
            self.access_serial = rebase_access_serials(&mut self.route_entries)
                .max(rebase_access_serials(&mut self.model_entries));
        }
        self.access_serial = self
            .access_serial
            .checked_add(1)
            .expect("bounded cache serial has room after rebasing");
        self.access_serial
    }
}

fn rebase_access_serials(entries: &mut HashMap<String, CacheEntry>) -> u64 {
    let mut oldest_first: Vec<_> = entries.values_mut().collect();
    oldest_first.sort_unstable_by_key(|entry| entry.access_serial);

    let mut highest = 0;
    for (index, entry) in oldest_first.into_iter().enumerate() {
        let serial = u64::try_from(index + 1).unwrap_or(u64::MAX);
        entry.access_serial = serial;
        highest = serial;
    }
    highest
}

pub(crate) fn normalized_aircraft_key(aircraft: &Aircraft) -> crate::model::AircraftKey {
    let key = aircraft.key();
    crate::model::AircraftKey {
        hex: normalize_aircraft_hex(&key.hex),
        callsign: normalize_flight_callsign(&key.callsign),
    }
}

fn cache_value(value: &LookupValue<String>) -> Option<(Option<String>, Duration)> {
    match value {
        LookupValue::NotRequested => None,
        LookupValue::Found(value) => Some((Some(value.clone()), SUCCESS_TTL)),
        LookupValue::Missing => Some((None, MISSING_TTL)),
    }
}

fn evict_lru(entries: &mut HashMap<String, CacheEntry>, capacity: usize) {
    while entries.len() > capacity {
        let Some(lru_key) = entries
            .iter()
            .min_by_key(|(_, entry)| entry.access_serial)
            .map(|(key, _)| key.clone())
        else {
            return;
        };
        entries.remove(&lru_key);
    }
}

#[derive(Debug, Error)]
pub enum FlightDataError {
    #[error("ADSBDB HTTP request failed: {0}")]
    Http(#[from] HttpError),
    #[error("ADSBDB returned HTTP status {0}")]
    Status(u16),
    #[error("ADSBDB response was not valid JSON: {0}")]
    Json(#[from] serde_json::Error),
    #[error("ADSBDB response schema was invalid: {0}")]
    Schema(&'static str),
}

pub trait FlightDataService: Send {
    fn lookup(
        &mut self,
        aircraft: &Aircraft,
        needs: EnrichmentNeeds,
    ) -> Result<FlightLookup, FlightDataError>;
}

pub struct FlightDataClient<C, K, S> {
    http: C,
    clock: K,
    sleeper: S,
    provider_base: String,
    last_request_at: Option<Duration>,
}

impl<C: HttpClient, K: Clock, S: Sleeper> FlightDataClient<C, K, S> {
    pub fn with_provider_base(http: C, clock: K, sleeper: S, provider_base: String) -> Self {
        Self {
            http,
            clock,
            sleeper,
            provider_base: provider_base.trim_end_matches('/').to_owned(),
            last_request_at: None,
        }
    }

    pub fn lookup(
        &mut self,
        aircraft: &Aircraft,
        needs: EnrichmentNeeds,
    ) -> Result<FlightLookup, FlightDataError> {
        validate_provider_base(&self.provider_base)?;

        let hex = normalize_aircraft_hex(&aircraft.hex);
        let callsign = normalize_flight_callsign(&aircraft.flight_callsign);
        let mut lookup = FlightLookup {
            route: if needs.route {
                LookupValue::Missing
            } else {
                LookupValue::NotRequested
            },
            model: if needs.model {
                LookupValue::Missing
            } else {
                LookupValue::NotRequested
            },
        };

        match (
            needs.route,
            needs.model,
            callsign.is_empty(),
            hex.is_empty(),
        ) {
            (false, false, _, _) => {}
            (true, false, false, _) | (true, true, false, true) => {
                let payload = self.request_callsign(&callsign)?;
                lookup.route = route_lookup(&payload);
            }
            (false, true, _, false) | (true, true, true, false) => {
                let payload = self.request_aircraft(&hex, None)?;
                lookup.model = model_lookup(&payload);
            }
            (true, true, false, false) => {
                let payload = self.request_aircraft(&hex, Some(&callsign))?;
                lookup.model = model_lookup(&payload);
                lookup.route = route_lookup(&payload);
                if matches!(lookup.route, LookupValue::Missing)
                    && !matches!(payload, ProviderPayload::NotFound)
                {
                    let fallback = self.request_callsign(&callsign)?;
                    lookup.route = route_lookup(&fallback);
                }
            }
            _ => {}
        }

        Ok(lookup)
    }

    fn request_callsign(&mut self, callsign: &str) -> Result<ProviderPayload, FlightDataError> {
        self.request(
            format!("{}/callsign/{callsign}", self.provider_base),
            Vec::new(),
        )
    }

    fn request_aircraft(
        &mut self,
        hex: &str,
        callsign: Option<&str>,
    ) -> Result<ProviderPayload, FlightDataError> {
        let query = callsign
            .map(|callsign| vec![("callsign".to_owned(), callsign.to_owned())])
            .unwrap_or_default();
        self.request(format!("{}/aircraft/{hex}", self.provider_base), query)
    }

    fn request(
        &mut self,
        url: String,
        query: Vec<(String, String)>,
    ) -> Result<ProviderPayload, FlightDataError> {
        let response = self.execute(HttpRequest {
            url,
            query,
            headers: Vec::new(),
            connect_timeout: CONNECT_TIMEOUT,
            read_timeout: READ_TIMEOUT,
            max_response_bytes: MAX_RESPONSE_BYTES,
            verify_tls: true,
        })?;
        match response.status {
            200 => parse_response(&response.body),
            404 => Ok(ProviderPayload::NotFound),
            status => Err(FlightDataError::Status(status)),
        }
    }

    fn execute(&mut self, request: HttpRequest) -> Result<HttpResponse, HttpError> {
        let now = self.clock.monotonic();
        if let Some(last_request_at) = self.last_request_at {
            let elapsed = now.saturating_sub(last_request_at);
            if elapsed < MINIMUM_REQUEST_INTERVAL {
                self.sleeper.sleep(MINIMUM_REQUEST_INTERVAL - elapsed);
            }
        }
        self.last_request_at = Some(self.clock.monotonic());
        self.http.execute(request)
    }
}

impl<C: HttpClient, K: Clock, S: Sleeper> FlightDataService for FlightDataClient<C, K, S> {
    fn lookup(
        &mut self,
        aircraft: &Aircraft,
        needs: EnrichmentNeeds,
    ) -> Result<FlightLookup, FlightDataError> {
        FlightDataClient::lookup(self, aircraft, needs)
    }
}

fn validate_provider_base(provider_base: &str) -> Result<(), FlightDataError> {
    let url = Url::parse(provider_base)
        .map_err(|_| FlightDataError::Schema("provider base must be a valid HTTPS URL"))?;
    if url.scheme() != "https"
        || !url.has_host()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(FlightDataError::Schema(
            "provider base must be a valid HTTPS URL",
        ));
    }
    Ok(())
}

pub(crate) fn normalize_aircraft_hex(identifier: &str) -> String {
    identifier
        .chars()
        .filter(char::is_ascii_hexdigit)
        .map(|character| character.to_ascii_uppercase())
        .take(MODE_S_HEX_LENGTH)
        .collect()
}

pub(crate) fn normalize_flight_callsign(identifier: &str) -> String {
    identifier
        .chars()
        .filter(char::is_ascii_alphanumeric)
        .map(|character| character.to_ascii_uppercase())
        .take(FLIGHT_CALLSIGN_LENGTH)
        .collect()
}

#[derive(Debug)]
enum ProviderPayload {
    Response(ResponseObject),
    Missing,
    NotFound,
}

#[derive(Debug, Deserialize)]
struct ResponseObject {
    #[serde(default)]
    aircraft: Option<ProviderAircraft>,
    #[serde(default)]
    flightroute: Option<FlightRoute>,
}

#[derive(Debug, Deserialize)]
struct ProviderAircraft {
    #[serde(default, rename = "type")]
    model_type: Option<String>,
    #[serde(default)]
    icao_type: Option<String>,
}

#[derive(Debug, Deserialize)]
struct FlightRoute {
    #[serde(default)]
    origin: Option<RouteEndpoint>,
    #[serde(default)]
    destination: Option<RouteEndpoint>,
}

#[derive(Debug, Deserialize)]
struct RouteEndpoint {
    #[serde(default)]
    iata_code: Option<String>,
    #[serde(default)]
    icao_code: Option<String>,
}

fn parse_response(body: &[u8]) -> Result<ProviderPayload, FlightDataError> {
    let root: Value = serde_json::from_slice(body)?;
    let root = root
        .as_object()
        .ok_or(FlightDataError::Schema("top level must be an object"))?;
    let response = root
        .get("response")
        .ok_or(FlightDataError::Schema("response field is required"))?;
    match response {
        Value::Object(_) => serde_json::from_value(response.clone())
            .map(ProviderPayload::Response)
            .map_err(|_| FlightDataError::Schema("response object has invalid field types")),
        Value::String(message)
            if message.trim().eq_ignore_ascii_case("unknown aircraft")
                || message.trim().eq_ignore_ascii_case("unknown callsign") =>
        {
            Ok(ProviderPayload::Missing)
        }
        Value::String(_) => Err(FlightDataError::Schema(
            "response string must identify a definite miss",
        )),
        _ => Err(FlightDataError::Schema(
            "response field must be an object or definite miss string",
        )),
    }
}

fn route_lookup(payload: &ProviderPayload) -> LookupValue<String> {
    match payload {
        ProviderPayload::Response(response) => response
            .flightroute
            .as_ref()
            .and_then(parse_route)
            .map(LookupValue::Found)
            .unwrap_or(LookupValue::Missing),
        ProviderPayload::Missing | ProviderPayload::NotFound => LookupValue::Missing,
    }
}

fn model_lookup(payload: &ProviderPayload) -> LookupValue<String> {
    match payload {
        ProviderPayload::Response(response) => response
            .aircraft
            .as_ref()
            .and_then(compact_aircraft_model)
            .map(LookupValue::Found)
            .unwrap_or(LookupValue::Missing),
        ProviderPayload::Missing | ProviderPayload::NotFound => LookupValue::Missing,
    }
}

fn parse_route(route: &FlightRoute) -> Option<String> {
    let origin = route.origin.as_ref().and_then(endpoint_code)?;
    let destination = route.destination.as_ref().and_then(endpoint_code)?;
    Some(format!("{origin}→{destination}"))
}

fn endpoint_code(endpoint: &RouteEndpoint) -> Option<&str> {
    non_blank(endpoint.iata_code.as_deref()).or_else(|| non_blank(endpoint.icao_code.as_deref()))
}

fn non_blank(value: Option<&str>) -> Option<&str> {
    value.map(str::trim).filter(|value| !value.is_empty())
}

fn compact_aircraft_model(aircraft: &ProviderAircraft) -> Option<String> {
    let model = normalized_text(aircraft.model_type.as_deref().unwrap_or(""));
    let model = if model.is_empty() {
        normalized_text(aircraft.icao_type.as_deref().unwrap_or(""))
    } else {
        compact_model(&model)
    };
    (!model.is_empty()).then_some(model)
}

fn compact_model(model: &str) -> String {
    let normalized = normalized_text(model);
    for manufacturer in [
        "De Havilland Canada",
        "Bombardier",
        "Embraer",
        "Airbus",
        "Boeing",
    ] {
        let Some(prefix) = normalized.get(..manufacturer.len()) else {
            continue;
        };
        if prefix.eq_ignore_ascii_case(manufacturer)
            && normalized
                .get(manufacturer.len()..)
                .is_some_and(|rest| rest.starts_with(' '))
        {
            return normalized[manufacturer.len()..].trim_start().to_owned();
        }
    }
    normalized
}

fn normalized_text(value: &str) -> String {
    value.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn access_serial_rebases_at_u64_max_without_losing_lru_order() {
        let mut cache = EnrichmentCache::new(2);
        cache.route_entries.insert(
            "OLDER".to_owned(),
            CacheEntry {
                value: Some("JFK→LAX".to_owned()),
                expires_at: Duration::from_secs(u64::MAX),
                access_serial: u64::MAX - 1,
            },
        );
        cache.route_entries.insert(
            "NEWER".to_owned(),
            CacheEntry {
                value: Some("SFO→SEA".to_owned()),
                expires_at: Duration::from_secs(u64::MAX),
                access_serial: u64::MAX,
            },
        );
        cache.access_serial = u64::MAX;

        let next = cache.next_access_serial();

        assert_eq!(next, 3);
        assert_eq!(cache.route_entries["OLDER"].access_serial, 1);
        assert_eq!(cache.route_entries["NEWER"].access_serial, 2);
    }
}

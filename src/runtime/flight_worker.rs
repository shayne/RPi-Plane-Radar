use std::cmp::Ordering as Comparison;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Receiver;
use std::time::Duration;

use crate::flight_data::{
    AircraftEnrichment, EnrichmentCache, EnrichmentNeeds, FlightDataError, FlightDataService,
    normalize_aircraft_hex, normalize_flight_callsign, normalized_aircraft_key,
};
use crate::geometry::offset_km;
use crate::http::HttpError;
use crate::model::{Aircraft, AircraftKey, Location, RadarSettings, RuntimeModel};
use crate::time::Clock;

use super::{CommandDrain, Waiter, WorkerCommand, drain_commands, wait_for_command};

const CACHE_CAPACITY: usize = 256;
const LOOKUP_INTERVAL: Duration = Duration::from_millis(750);
const NO_CANDIDATE_INTERVAL: Duration = Duration::from_secs(3);
const FAILURE_INTERVAL: Duration = Duration::from_secs(30);

#[derive(Default)]
struct FailureLogWindow {
    logged_at: Option<Duration>,
}

struct FailureLogRecord {
    provider: &'static str,
    category: &'static str,
    identity: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct LookupIdentity {
    aircraft: AircraftKey,
    needs: EnrichmentNeeds,
}

struct FailureBackoff {
    identity: LookupIdentity,
    deadline: Duration,
}

impl FailureLogWindow {
    fn record(
        &mut self,
        error: &FlightDataError,
        aircraft: &Aircraft,
        now: Duration,
    ) -> Option<FailureLogRecord> {
        if self
            .logged_at
            .is_some_and(|logged_at| now.saturating_sub(logged_at) < FAILURE_INTERVAL)
        {
            return None;
        }
        self.logged_at = Some(now);
        Some(FailureLogRecord {
            provider: "ADSBDB",
            category: error_category(error),
            identity: normalized_identity(aircraft),
        })
    }
}

pub struct FlightDataWorker<D, K, W> {
    service: D,
    cache: EnrichmentCache,
    model: RuntimeModel,
    clock: K,
    waiter: W,
}

impl<D: FlightDataService, K: Clock, W: Waiter> FlightDataWorker<D, K, W> {
    pub fn new(service: D, model: RuntimeModel, clock: K, waiter: W) -> Self {
        Self {
            service,
            cache: EnrichmentCache::new(CACHE_CAPACITY),
            model,
            clock,
            waiter,
        }
    }

    pub fn run(mut self, commands: Receiver<WorkerCommand>, stop: Arc<AtomicBool>) {
        let mut failure_logs = FailureLogWindow::default();
        let mut failure_backoff: Option<FailureBackoff> = None;

        loop {
            if stop.load(Ordering::Acquire)
                || matches!(drain_commands(&commands, &stop), CommandDrain::Stop)
            {
                return;
            }

            let snapshot = self.model.snapshot();
            let needs = needs_for(&snapshot.settings);
            let Some(location) = snapshot.settings.location.clone() else {
                failure_backoff = None;
                if !wait_for_command(&self.waiter, &commands, &stop, FAILURE_INTERVAL) {
                    return;
                }
                continue;
            };
            if needs == EnrichmentNeeds::default() {
                failure_backoff = None;
                if !wait_for_command(&self.waiter, &commands, &stop, FAILURE_INTERVAL) {
                    return;
                }
                continue;
            }

            let now = self.clock.monotonic();
            let mut candidate = None;
            for aircraft in snapshot.aircraft.iter() {
                let resolution = self.cache.resolve(aircraft, needs, now);
                if has_cached_field(needs, resolution.pending) {
                    self.publish_if_current(&location, needs, aircraft, resolution.enrichment);
                }
                if resolution.pending == EnrichmentNeeds::default() {
                    continue;
                }

                let offset = offset_km(&location, aircraft.latitude, aircraft.longitude);
                let distance = offset.east.hypot(offset.north);
                if !distance.is_finite() {
                    continue;
                }
                let replace = candidate.as_ref().is_none_or(|(current_distance, _, _)| {
                    distance
                        .partial_cmp(current_distance)
                        .is_some_and(|ordering| ordering == Comparison::Less)
                });
                if replace {
                    candidate = Some((distance, aircraft.clone(), resolution.pending));
                }
            }

            let Some((_, aircraft, pending)) = candidate else {
                failure_backoff = None;
                if !wait_for_command(&self.waiter, &commands, &stop, NO_CANDIDATE_INTERVAL) {
                    return;
                }
                continue;
            };

            let lookup_identity = LookupIdentity {
                aircraft: normalized_aircraft_key(&aircraft),
                needs: pending,
            };
            if let Some(backoff) = failure_backoff.as_ref()
                && backoff.identity == lookup_identity
            {
                let remaining = backoff.deadline.saturating_sub(self.clock.monotonic());
                if !remaining.is_zero() {
                    if !wait_for_command(&self.waiter, &commands, &stop, remaining) {
                        return;
                    }
                    continue;
                }
            }

            let result = self.service.lookup(&aircraft, pending);
            let command_drain = drain_commands(&commands, &stop);
            if stop.load(Ordering::Acquire) || matches!(command_drain, CommandDrain::Stop) {
                return;
            }

            match result {
                Ok(lookup) => {
                    failure_backoff = None;
                    let now = self.clock.monotonic();
                    self.cache.record(&aircraft, pending, &lookup, now);
                    if !matches!(command_drain, CommandDrain::Changed) {
                        let resolution = self.cache.resolve(&aircraft, needs, now);
                        self.publish_if_current(&location, needs, &aircraft, resolution.enrichment);
                    }
                    if matches!(command_drain, CommandDrain::Changed) {
                        continue;
                    }
                    if !wait_for_command(&self.waiter, &commands, &stop, LOOKUP_INTERVAL) {
                        return;
                    }
                }
                Err(error) => {
                    failure_backoff = Some(FailureBackoff {
                        identity: lookup_identity,
                        deadline: self.clock.monotonic().saturating_add(FAILURE_INTERVAL),
                    });
                    if matches!(command_drain, CommandDrain::Changed) {
                        continue;
                    }
                    if let Some(record) =
                        failure_logs.record(&error, &aircraft, self.clock.monotonic())
                    {
                        log::warn!(
                            "provider={} category={} aircraft={}",
                            record.provider,
                            record.category,
                            record.identity,
                        );
                    }
                    if !wait_for_command(&self.waiter, &commands, &stop, FAILURE_INTERVAL) {
                        return;
                    }
                }
            }
        }
    }

    fn publish_if_current(
        &self,
        expected_location: &Location,
        expected_needs: EnrichmentNeeds,
        aircraft: &Aircraft,
        enrichment: AircraftEnrichment,
    ) {
        let key = aircraft.key();
        let _ = self.model.record_enrichment_if_current(
            expected_location,
            expected_needs,
            &key,
            enrichment,
        );
    }
}

fn needs_for(settings: &RadarSettings) -> EnrichmentNeeds {
    EnrichmentNeeds {
        route: settings.show_route,
        model: settings.show_expanded_model,
    }
}

fn has_cached_field(needs: EnrichmentNeeds, pending: EnrichmentNeeds) -> bool {
    (needs.route && !pending.route) || (needs.model && !pending.model)
}

fn error_category(error: &FlightDataError) -> &'static str {
    match error {
        FlightDataError::Http(HttpError::InvalidTimeout) => "invalid-timeout",
        FlightDataError::Http(HttpError::InvalidBodyLimit) => "invalid-body-limit",
        FlightDataError::Http(HttpError::Timeout) => "timeout",
        FlightDataError::Http(HttpError::Transport) => "transport",
        FlightDataError::Http(HttpError::Body) => "response-body",
        FlightDataError::Http(HttpError::BodyTooLarge) => "response-too-large",
        FlightDataError::Http(HttpError::TlsVerificationRequired) => "tls-verification",
        FlightDataError::Status(_) => "http-status",
        FlightDataError::Json(_) => "invalid-json",
        FlightDataError::Schema(_) => "invalid-schema",
    }
}

fn normalized_identity(aircraft: &Aircraft) -> String {
    let hex = normalize_aircraft_hex(&aircraft.hex);
    let callsign = normalize_flight_callsign(&aircraft.flight_callsign);
    format!(
        "{}/{}",
        if hex.is_empty() { "-" } else { &hex },
        if callsign.is_empty() { "-" } else { &callsign },
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn aircraft() -> Aircraft {
        Aircraft {
            hex: "a-b c".to_owned(),
            flight_callsign: "aa 12!".to_owned(),
            latitude: 40.7128,
            longitude: -74.006,
            nose_degrees: 0.0,
            track_degrees: 0.0,
            ground_speed_knots: 0.0,
            callsign: "AA12".to_owned(),
            aircraft_type: "B738".to_owned(),
            altitude_feet: Some(10_000),
            altitude: "10000 ft".to_owned(),
        }
    }

    #[test]
    fn failure_log_window_emits_only_sanitized_fields_once_per_thirty_seconds() {
        let mut window = FailureLogWindow::default();
        let error = FlightDataError::Http(HttpError::Timeout);

        let mut noisy_aircraft = aircraft();
        noisy_aircraft.hex = "ab-c123def456☃".to_owned();
        noisy_aircraft.flight_callsign = "aa-l 12345✈xyz".to_owned();

        let first = window
            .record(&error, &noisy_aircraft, Duration::ZERO)
            .expect("first failure log");
        assert_eq!(first.provider, "ADSBDB");
        assert_eq!(first.category, "timeout");
        assert_eq!(first.identity, "ABC123/AAL12345");
        assert!(!first.identity.contains("40.7128"));
        assert!(!first.identity.contains("-74.006"));

        assert!(
            window
                .record(&error, &aircraft(), Duration::from_millis(29_999))
                .is_none()
        );
        assert!(
            window
                .record(&error, &aircraft(), Duration::from_secs(30))
                .is_some()
        );
    }
}

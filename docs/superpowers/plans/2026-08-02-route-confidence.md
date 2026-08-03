# Route Confidence Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Suppress geographically impossible ADSBDB route labels while preserving plausible two- and three-airport route candidates and unchanged aircraft-model enrichment.

**Architecture:** Add a focused `RouteCandidate` geometry unit that owns validated airport points and position confidence. ADSBDB parsing produces structured candidates, the route cache stores those candidates by callsign, and every cache resolution derives an optional display label from the live aircraft position. The renderer-facing enrichment type remains unchanged.

**Tech Stack:** Rust 2024 edition, `serde_json`, existing `Aircraft` and `GeoPoint` models, great-circle geometry using the standard library, cargo-nextest, cargo-clippy, GitButler CLI.

## Global Constraints

- Do not add a provider, API key, account, dependency, or privacy boundary.
- Keep ADSBDB aircraft-model enrichment behavior unchanged.
- Keep route enrichment optional and independent of the three-second ADS-B worker.
- Keep successful candidates cached for six hours and definite misses cached for ten minutes.
- Keep both route and model caches bounded to 256 least-recently-used entries.
- Keep ADSBDB request spacing at 750 milliseconds and preserve existing timeout, response-size, TLS, and failure-backoff behavior.
- Accept only three-character ASCII-alphanumeric IATA codes or four-character ASCII-alphanumeric ICAO fallback codes, normalized to upper case.
- Require finite airport and aircraft coordinates with latitude in `[-90, 90]` and longitude in `[-180, 180]`.
- Use a route corridor equal to 20 percent of each segment length, clamped to 200 through 500 km inclusive.
- Reject segments shorter than 1 km or within `0.000001` radians of exact antipodal separation.
- Render a valid midpoint as `ORIGIN→MIDPOINT→DESTINATION`; do not infer the active leg from heading.
- Do not change settings, defaults, tag order, typography, or browser UX.
- Run `mise run verify` before completion.

---

## File Structure

- Create `src/route_confidence.rs`: validated route-candidate representation and great-circle segment confidence.
- Modify `src/lib.rs`: register the focused private module.
- Modify `src/flight_data.rs`: re-export `RouteCandidate`, isolate provider route JSON, parse airport codes and coordinates, type the route cache, and evaluate cached candidates against live aircraft positions.
- Create `tests/route_confidence.rs`: public-contract and geographical edge-case tests.
- Modify `tests/flight_data.rs`: provider parsing, malformed-route isolation, typed-cache, expiry, and position re-evaluation tests.
- Modify `tests/runtime_flight.rs`: worker-level proof that a cached route disappears after the same aircraft moves to an implausible position without a second provider request.
- Modify `tests/fixtures/adsbdb/callsign.json`: add valid JFK and LAX airport coordinates.
- Modify `tests/fixtures/adsbdb/combined.json`: add the same bounded route geometry to the combined fixture.
- Modify `README.md`: disclose that routes are static candidates subject to a geographical confidence gate.
- Modify `docs/architecture.md`: describe structured route caching and per-position evaluation.

---

### Task 1: Route-Candidate Geometry

**Files:**

- Create: `src/route_confidence.rs`
- Modify: `src/lib.rs`
- Create: `tests/route_confidence.rs`

**Interfaces:**

- Consumes: `crate::geometry::EARTH_RADIUS_KM`, `crate::model::{Aircraft, GeoPoint}`.
- Produces: `RouteCandidate::new(label: String, points: Vec<GeoPoint>) -> Option<RouteCandidate>`.
- Produces: `RouteCandidate::label_for(&self, aircraft: &Aircraft) -> Option<&str>`.
- Invariant: a constructed candidate has exactly two or three valid points and one or two nondegenerate great-circle segments.

- [ ] **Step 1: Write the failing public geometry tests**

Create `tests/route_confidence.rs` with helpers and cases that exercise the approved confidence boundary:

```rust
use planeradar::flight_data::RouteCandidate;
use planeradar::model::{Aircraft, GeoPoint};

fn point(latitude: f64, longitude: f64) -> GeoPoint {
    GeoPoint {
        latitude,
        longitude,
    }
}

fn aircraft(latitude: f64, longitude: f64) -> Aircraft {
    Aircraft {
        hex: "abc123".to_owned(),
        flight_callsign: "AAL1".to_owned(),
        latitude,
        longitude,
        nose_degrees: 0.0,
        track_degrees: 0.0,
        ground_speed_knots: 0.0,
        callsign: "AAL1".to_owned(),
        aircraft_type: "B738".to_owned(),
        altitude_feet: Some(10_000),
        altitude: "10000 ft".to_owned(),
    }
}

fn candidate(label: &str, points: Vec<GeoPoint>) -> RouteCandidate {
    RouteCandidate::new(label.to_owned(), points).expect("valid route candidate")
}

#[test]
fn accepts_new_york_on_a_jfk_to_lax_candidate() {
    let route = candidate(
        "JFK→LAX",
        vec![point(40.6413, -73.7781), point(33.9416, -118.4085)],
    );
    assert_eq!(
        route.label_for(&aircraft(40.792_283, -73.972_639_1)),
        Some("JFK→LAX")
    );
}

#[test]
fn rejects_new_york_for_iah_to_abq() {
    let route = candidate(
        "IAH→ABQ",
        vec![point(29.9902, -95.3368), point(35.0402, -106.6090)],
    );
    assert_eq!(
        route.label_for(&aircraft(40.792_283, -73.972_639_1)),
        None
    );
}

#[test]
fn midpoint_candidate_is_accepted_near_either_leg() {
    let route = candidate(
        "SFO→HNL→NRT",
        vec![
            point(37.6213, -122.3790),
            point(21.3187, -157.9225),
            point(35.7720, 140.3929),
        ],
    );
    assert_eq!(
        route.label_for(&aircraft(21.35, -157.90)),
        Some("SFO→HNL→NRT")
    );
    assert_eq!(
        route.label_for(&aircraft(34.0, 145.0)),
        Some("SFO→HNL→NRT")
    );
}

#[test]
fn date_line_segment_uses_the_short_wrapped_path() {
    let route = candidate(
        "AAA→BBB",
        vec![point(10.0, 170.0), point(10.0, -170.0)],
    );
    assert_eq!(route.label_for(&aircraft(10.0, 179.0)), Some("AAA→BBB"));
}

#[test]
fn point_beyond_an_endpoint_does_not_use_the_infinite_great_circle() {
    let route = candidate(
        "AAA→BBB",
        vec![point(0.0, 0.0), point(0.0, 2.0)],
    );
    assert_eq!(route.label_for(&aircraft(0.0, 5.0)), None);
}

#[test]
fn invalid_live_aircraft_coordinates_fail_closed() {
    let route = candidate(
        "AAA→BBB",
        vec![point(0.0, 0.0), point(0.0, 2.0)],
    );
    assert_eq!(route.label_for(&aircraft(f64::NAN, 1.0)), None);
    assert_eq!(route.label_for(&aircraft(0.0, 181.0)), None);
}

#[test]
fn invalid_points_and_degenerate_segments_are_rejected() {
    assert!(RouteCandidate::new("BAD".to_owned(), vec![point(91.0, 0.0), point(0.0, 0.0)]).is_none());
    assert!(RouteCandidate::new("BAD".to_owned(), vec![point(0.0, f64::NAN), point(0.0, 1.0)]).is_none());
    assert!(RouteCandidate::new("BAD".to_owned(), vec![point(0.0, 0.0)]).is_none());
    assert!(RouteCandidate::new("BAD".to_owned(), vec![point(0.0, 0.0), point(0.0, 0.000_001)]).is_none());
    assert!(RouteCandidate::new("BAD".to_owned(), vec![point(0.0, 0.0), point(0.0, 180.0)]).is_none());
}
```

- [ ] **Step 2: Register the module scaffold and run the focused test to verify failure**

Create `src/route_confidence.rs` with only this module documentation:

```rust
//! Position-aware confidence for static ADSBDB route candidates.
```

Add `mod route_confidence;` near `pub mod range;` in `src/lib.rs`, then run:

```bash
cargo nextest run --test route_confidence
```

Expected: compilation fails because `planeradar::flight_data::RouteCandidate` does not exist.

- [ ] **Step 3: Implement the validated candidate and great-circle helpers**

Replace the scaffold in `src/route_confidence.rs` with the following structure and formulas:

```rust
use std::f64::consts::{PI, TAU};

use crate::geometry::EARTH_RADIUS_KM;
use crate::model::{Aircraft, GeoPoint};

const CORRIDOR_FRACTION: f64 = 0.20;
const MIN_CORRIDOR_KM: f64 = 200.0;
const MAX_CORRIDOR_KM: f64 = 500.0;
const MIN_SEGMENT_KM: f64 = 1.0;
const ANTIPODAL_EPSILON_RADIANS: f64 = 0.000_001;

#[derive(Clone, Debug, PartialEq)]
pub struct RouteCandidate {
    label: String,
    points: Box<[GeoPoint]>,
}

impl RouteCandidate {
    pub fn new(label: String, points: Vec<GeoPoint>) -> Option<Self> {
        if label.is_empty()
            || !(2..=3).contains(&points.len())
            || !points.iter().all(valid_point)
            || points.windows(2).any(|segment| {
                let Some(angle) = angular_distance(&segment[0], &segment[1]) else {
                    return true;
                };
                angle * EARTH_RADIUS_KM < MIN_SEGMENT_KM
                    || (PI - angle).abs() <= ANTIPODAL_EPSILON_RADIANS
            })
        {
            return None;
        }
        Some(Self {
            label,
            points: points.into_boxed_slice(),
        })
    }

    pub fn label_for(&self, aircraft: &Aircraft) -> Option<&str> {
        let live = GeoPoint {
            latitude: aircraft.latitude,
            longitude: aircraft.longitude,
        };
        if !valid_point(&live) {
            return None;
        }
        self.points
            .windows(2)
            .any(|segment| {
                let Some(length_radians) = angular_distance(&segment[0], &segment[1]) else {
                    return false;
                };
                let corridor = corridor_width_km(length_radians * EARTH_RADIUS_KM);
                distance_to_segment_km(&live, &segment[0], &segment[1])
                    .is_some_and(|distance| distance <= corridor)
            })
            .then_some(self.label.as_str())
    }
}

fn valid_point(point: &GeoPoint) -> bool {
    point.latitude.is_finite()
        && point.longitude.is_finite()
        && (-90.0..=90.0).contains(&point.latitude)
        && (-180.0..=180.0).contains(&point.longitude)
}

fn angular_distance(a: &GeoPoint, b: &GeoPoint) -> Option<f64> {
    let lat_a = a.latitude.to_radians();
    let lat_b = b.latitude.to_radians();
    let delta_lat = lat_b - lat_a;
    let delta_lon = normalize_radians(b.longitude.to_radians() - a.longitude.to_radians());
    let haversine = (delta_lat / 2.0).sin().powi(2)
        + lat_a.cos() * lat_b.cos() * (delta_lon / 2.0).sin().powi(2);
    let angle = 2.0 * haversine.clamp(0.0, 1.0).sqrt().asin();
    angle.is_finite().then_some(angle)
}

fn initial_bearing(a: &GeoPoint, b: &GeoPoint) -> Option<f64> {
    let lat_a = a.latitude.to_radians();
    let lat_b = b.latitude.to_radians();
    let delta_lon = normalize_radians(b.longitude.to_radians() - a.longitude.to_radians());
    let y = delta_lon.sin() * lat_b.cos();
    let x = lat_a.cos() * lat_b.sin() - lat_a.sin() * lat_b.cos() * delta_lon.cos();
    let bearing = y.atan2(x);
    bearing.is_finite().then_some(bearing)
}

fn distance_to_segment_km(point: &GeoPoint, start: &GeoPoint, end: &GeoPoint) -> Option<f64> {
    let segment_angle = angular_distance(start, end)?;
    let point_angle = angular_distance(start, point)?;
    let segment_bearing = initial_bearing(start, end)?;
    let point_bearing = initial_bearing(start, point)?;
    let bearing_delta = point_bearing - segment_bearing;
    let cross_track = (point_angle.sin() * bearing_delta.sin())
        .clamp(-1.0, 1.0)
        .asin();
    let along_track = (point_angle.sin() * bearing_delta.cos()).atan2(point_angle.cos());
    let distance = if (0.0..=segment_angle).contains(&along_track) {
        cross_track.abs() * EARTH_RADIUS_KM
    } else {
        angular_distance(point, start)?
            .min(angular_distance(point, end)?)
            * EARTH_RADIUS_KM
    };
    distance.is_finite().then_some(distance)
}

fn normalize_radians(value: f64) -> f64 {
    (value + PI).rem_euclid(TAU) - PI
}

fn corridor_width_km(segment_length_km: f64) -> f64 {
    (segment_length_km * CORRIDOR_FRACTION).clamp(MIN_CORRIDOR_KM, MAX_CORRIDOR_KM)
}

#[cfg(test)]
mod tests {
    use super::corridor_width_km;

    #[test]
    fn corridor_width_uses_the_exact_minimum_and_maximum() {
        assert_eq!(corridor_width_km(500.0), 200.0);
        assert_eq!(corridor_width_km(1_500.0), 300.0);
        assert_eq!(corridor_width_km(5_000.0), 500.0);
    }
}
```

Re-export the type at the top of `src/flight_data.rs`:

```rust
pub use crate::route_confidence::RouteCandidate;
```

- [ ] **Step 4: Run formatting, focused tests, and strict clippy**

```bash
cargo fmt
cargo nextest run --test route_confidence
cargo clippy --all-targets --all-features -- -D warnings
```

Expected: all commands pass; the new module performs no I/O and has no dependencies beyond existing models and geometry constants.

- [ ] **Step 5: Commit only the route geometry files with GitButler**

```bash
but stage src/route_confidence.rs codex/route-confidence
but stage src/lib.rs codex/route-confidence
but stage src/flight_data.rs codex/route-confidence
but stage tests/route_confidence.rs codex/route-confidence
but commit codex/route-confidence --only -m "feat: add route confidence geometry"
```

Expected: the concurrent settings-UI files remain uncommitted or assigned to their existing stack.

---

### Task 2: ADSBDB Candidate Parsing and Position-Aware Cache

**Files:**

- Modify: `src/flight_data.rs`
- Modify: `tests/flight_data.rs`
- Modify: `tests/runtime_flight.rs`
- Modify: `tests/fixtures/adsbdb/callsign.json`
- Modify: `tests/fixtures/adsbdb/combined.json`

**Interfaces:**

- Consumes: `RouteCandidate::new` and `RouteCandidate::label_for` from Task 1.
- Changes: `FlightLookup.route` from `LookupValue<String>` to `LookupValue<RouteCandidate>`.
- Preserves: `AircraftEnrichment.route: Option<String>` as the renderer-facing boundary.
- Preserves: `FlightDataService::lookup(&mut self, &Aircraft, EnrichmentNeeds) -> Result<FlightLookup, FlightDataError>`.
- Produces: route cache entries typed as `CacheEntry<RouteCandidate>` and model entries typed as `CacheEntry<String>`.

- [ ] **Step 1: Extend fixtures and write failing provider-isolation tests**

Add the official airport coordinates to both ADSBDB fixtures:

```json
"origin": {
  "iata_code": "JFK",
  "icao_code": "KJFK",
  "latitude": 40.6413,
  "longitude": -73.7781
},
"destination": {
  "iata_code": "LAX",
  "icao_code": "KLAX",
  "latitude": 33.9416,
  "longitude": -118.4085
}
```

In `tests/flight_data.rs`, import `RouteCandidate` and `GeoPoint`, add a helper that extracts the position-approved label, then update route assertions to use it:

```rust
fn route_label<'a>(lookup: &'a FlightLookup, aircraft: &Aircraft) -> Option<&'a str> {
    match &lookup.route {
        LookupValue::Found(candidate) => candidate.label_for(aircraft),
        LookupValue::NotRequested | LookupValue::Missing => None,
    }
}
```

Add explicit tests for midpoint parsing and malformed-route isolation:

```rust
#[test]
fn midpoint_is_preserved_in_the_candidate_label() {
    let response = serde_json::json!({
        "response": {"flightroute": {
            "origin": {"iata_code":"SFO","icao_code":"KSFO","latitude":37.6213,"longitude":-122.3790},
            "midpoint": {"iata_code":"HNL","icao_code":"PHNL","latitude":21.3187,"longitude":-157.9225},
            "destination": {"iata_code":"NRT","icao_code":"RJAA","latitude":35.7720,"longitude":140.3929}
        }}
    });
    let plane = aircraft("abc123", "aal1");
    let mut near_midpoint = plane.clone();
    near_midpoint.latitude = 21.35;
    near_midpoint.longitude = -157.90;
    let (mut client, _, _) = client([json_response(response)]);
    let lookup = client.lookup(&near_midpoint, EnrichmentNeeds { route: true, model: false }).expect("midpoint route");
    assert_eq!(route_label(&lookup, &near_midpoint), Some("SFO→HNL→NRT"));
}

#[test]
fn malformed_route_does_not_discard_a_valid_combined_model() {
    let response = serde_json::json!({
        "response": {
            "aircraft": {"type":"Boeing 737-800","icao_type":"B738"},
            "flightroute": {
                "origin": {"iata_code":42,"latitude":"north","longitude":-73.7781},
                "destination": {"iata_code":"LAX","latitude":33.9416,"longitude":-118.4085}
            }
        }
    });
    let (mut client, http, _) = client([
        json_response(response),
        ok(include_bytes!("fixtures/adsbdb/unknown.json")),
    ]);
    let lookup = client.lookup(&aircraft("abc123", "aal1"), both()).expect("combined lookup");
    assert_eq!(lookup.route, LookupValue::Missing);
    assert_eq!(lookup.model, LookupValue::Found("737-800".to_owned()));
    assert_eq!(http.request_count(), 2);
}
```

Add the following validation cases. The explicit null midpoint must produce `LookupValue::Missing`, while an invalid IATA code may use a valid ICAO fallback:

```rust
#[test]
fn route_endpoint_validation_fails_closed_with_icao_fallback() {
    let valid_icao = serde_json::json!({
        "response": {"flightroute": {
            "origin": {"iata_code":"too-long","icao_code":"KJFK","latitude":40.6413,"longitude":-73.7781},
            "destination": {"iata_code":null,"icao_code":"KLAX","latitude":33.9416,"longitude":-118.4085}
        }}
    });
    let invalid_routes = [
        serde_json::json!({"origin":{"iata_code":"JFK","longitude":-73.7781},"destination":{"iata_code":"LAX","latitude":33.9416,"longitude":-118.4085}}),
        serde_json::json!({"origin":{"iata_code":"JFK","latitude":91.0,"longitude":-73.7781},"destination":{"iata_code":"LAX","latitude":33.9416,"longitude":-118.4085}}),
        serde_json::json!({"origin":{"iata_code":"J@K","icao_code":"BAD","latitude":40.6413,"longitude":-73.7781},"destination":{"iata_code":"LAX","latitude":33.9416,"longitude":-118.4085}}),
        serde_json::json!({"origin":{"iata_code":"JFK","latitude":40.6413,"longitude":-73.7781},"midpoint":null,"destination":{"iata_code":"LAX","latitude":33.9416,"longitude":-118.4085}}),
    ];
    let responses = std::iter::once(json_response(valid_icao)).chain(
        invalid_routes.into_iter().map(|flightroute| {
            json_response(serde_json::json!({"response":{"flightroute":flightroute}}))
        }),
    );
    let (mut client, _, _) = client(responses);
    let plane = aircraft("abc123", "aal1");
    let needs = EnrichmentNeeds { route: true, model: false };

    let fallback = client.lookup(&plane, needs).expect("ICAO fallback");
    assert_eq!(route_label(&fallback, &plane), Some("KJFK→KLAX"));
    for _ in 0..4 {
        assert_eq!(
            client.lookup(&plane, needs).expect("invalid route").route,
            LookupValue::Missing
        );
    }
}
```

- [ ] **Step 2: Write the failing cache and worker movement tests**

Replace the string-only `found` test helper with a structured helper whose segment contains the default test aircraft at `40.0, -74.0`:

```rust
fn route_candidate(label: &str) -> RouteCandidate {
    RouteCandidate::new(
        label.to_owned(),
        vec![
            GeoPoint { latitude: 40.0, longitude: -75.0 },
            GeoPoint { latitude: 40.0, longitude: -73.0 },
        ],
    )
    .expect("route candidate")
}

fn found(route: &str, model: &str) -> FlightLookup {
    FlightLookup {
        route: LookupValue::Found(route_candidate(route)),
        model: LookupValue::Found(model.to_owned()),
    }
}
```

Add a cache test proving that one stored candidate is evaluated independently for two positions using the same callsign:

```rust
#[test]
fn cached_route_is_rechecked_for_each_aircraft_position() {
    let mut cache = EnrichmentCache::new(256);
    let near = aircraft("hex-one", "flight-one");
    cache.record(&near, EnrichmentNeeds { route: true, model: false }, &FlightLookup {
        route: LookupValue::Found(route_candidate("JFK→LAX")),
        model: LookupValue::NotRequested,
    }, Duration::ZERO);

    assert_eq!(cache.resolve(&near, EnrichmentNeeds { route: true, model: false }, Duration::from_secs(1)).enrichment.route.as_deref(), Some("JFK→LAX"));

    let mut far = near.clone();
    far.latitude = 0.0;
    far.longitude = 0.0;
    assert_eq!(cache.resolve(&far, EnrichmentNeeds { route: true, model: false }, Duration::from_secs(2)).enrichment.route, None);
    assert!(!cache.resolve(&far, EnrichmentNeeds { route: true, model: false }, Duration::from_secs(3)).pending.route);
}
```

In `tests/runtime_flight.rs`, make `found_both` construct a `RouteCandidate`, add coordinates to inline route JSON, and add this worker-level cache test:

```rust
#[test]
fn cached_route_is_suppressed_after_the_same_aircraft_moves_off_corridor() {
    let clock = FakeClock::default();
    let model = RuntimeModel::new(configured(true, false), "http://local".to_owned());
    let near_iah = aircraft("abc123", "ual1", 29.9902, -95.3368);
    model.record_aircraft(vec![near_iah], Duration::ZERO);
    let candidate = RouteCandidate::new(
        "IAH→ABQ".to_owned(),
        vec![
            GeoPoint { latitude: 29.9902, longitude: -95.3368 },
            GeoPoint { latitude: 35.0402, longitude: -106.6090 },
        ],
    )
    .expect("route candidate");
    let result = Ok(FlightLookup {
        route: LookupValue::Found(candidate),
        model: LookupValue::NotRequested,
    });
    let (service, calls) = FakeService::new(clock.clone(), [result]);
    let moved_model = model.clone();
    let manhattan = aircraft("abc123", "ual1", 40.792_283, -73.972_639_1);
    let key = manhattan.key();
    let (waiter, _) = ScriptedWaiter::new(
        clock.clone(),
        [
            WaitOutcome::Action(Box::new(move || {
                moved_model.record_aircraft(vec![manhattan], Duration::from_millis(750));
            })),
            WaitOutcome::Stop,
        ],
    );

    run(service, model.clone(), clock, waiter);

    assert_eq!(calls.lock().expect("calls").len(), 1);
    assert_eq!(
        model
            .snapshot()
            .enrichment
            .get(&key)
            .and_then(|enrichment| enrichment.route.as_deref()),
        None
    );
}
```

Run the affected tests before production changes:

```bash
cargo nextest run --test flight_data --test runtime_flight
```

Expected: compilation fails because `FlightLookup.route` still accepts strings and the cache cannot store or evaluate `RouteCandidate`.

- [ ] **Step 3: Isolate and parse ADSBDB route JSON into candidates**

In `src/flight_data.rs`, change the lookup and provider response boundary:

```rust
#[derive(Clone, Debug, PartialEq)]
pub struct FlightLookup {
    pub route: LookupValue<RouteCandidate>,
    pub model: LookupValue<String>,
}

#[derive(Debug, Deserialize)]
struct ResponseObject {
    #[serde(default)]
    aircraft: Option<ProviderAircraft>,
    #[serde(default)]
    flightroute: Option<Value>,
}
```

Replace `FlightRoute`, `RouteEndpoint`, `parse_route`, and `endpoint_code` with tolerant isolated parsing:

```rust
fn route_lookup(payload: &ProviderPayload) -> LookupValue<RouteCandidate> {
    match payload {
        ProviderPayload::Response(response) => response
            .flightroute
            .as_ref()
            .and_then(parse_route_candidate)
            .map(LookupValue::Found)
            .unwrap_or(LookupValue::Missing),
        ProviderPayload::Missing | ProviderPayload::NotFound => LookupValue::Missing,
    }
}

fn parse_route_candidate(value: &Value) -> Option<RouteCandidate> {
    let route = value.as_object()?;
    let (origin_code, origin) = parse_endpoint(route.get("origin")?)?;
    let (destination_code, destination) = parse_endpoint(route.get("destination")?)?;
    let mut codes = vec![origin_code];
    let mut points = vec![origin];
    if let Some(midpoint) = route.get("midpoint") {
        let (midpoint_code, midpoint) = parse_endpoint(midpoint)?;
        codes.push(midpoint_code);
        points.push(midpoint);
    }
    codes.push(destination_code);
    points.push(destination);
    RouteCandidate::new(codes.join("→"), points)
}

fn parse_endpoint(value: &Value) -> Option<(String, GeoPoint)> {
    let endpoint = value.as_object()?;
    let code = valid_code(endpoint.get("iata_code"), 3)
        .or_else(|| valid_code(endpoint.get("icao_code"), 4))?;
    let point = GeoPoint {
        latitude: endpoint.get("latitude")?.as_f64()?,
        longitude: endpoint.get("longitude")?.as_f64()?,
    };
    Some((code, point))
}

fn valid_code(value: Option<&Value>, length: usize) -> Option<String> {
    let normalized = value?.as_str()?.trim().to_ascii_uppercase();
    (normalized.len() == length && normalized.chars().all(|character| character.is_ascii_alphanumeric()))
        .then_some(normalized)
}
```

Import `GeoPoint` beside `Aircraft`. Because the entire route remains a `Value`, wrong nested field types produce `LookupValue::Missing` and do not turn a valid combined model response into a schema error.

- [ ] **Step 4: Type the cache and evaluate candidates at resolution time**

Make `CacheEntry` generic and give each map its correct type:

```rust
pub struct EnrichmentCache {
    route_entries: HashMap<String, CacheEntry<RouteCandidate>>,
    model_entries: HashMap<String, CacheEntry<String>>,
    capacity: usize,
    access_serial: u64,
}

#[derive(Clone, Debug)]
struct CacheEntry<T> {
    value: Option<T>,
    expires_at: Duration,
    access_serial: u64,
}
```

In `resolve`, derive the display string from the live aircraft every time:

```rust
match self.route_entries.get_mut(&key.callsign) {
    Some(entry) => {
        entry.access_serial = serial;
        resolution.enrichment.route = entry
            .value
            .as_ref()
            .and_then(|candidate| candidate.label_for(aircraft))
            .map(str::to_owned);
    }
    None => resolution.pending.route = true,
}
```

Generalize the existing helpers without changing their behavior:

```rust
fn cache_value<T: Clone>(value: &LookupValue<T>) -> Option<(Option<T>, Duration)> {
    match value {
        LookupValue::NotRequested => None,
        LookupValue::Found(value) => Some((Some(value.clone()), SUCCESS_TTL)),
        LookupValue::Missing => Some((None, MISSING_TTL)),
    }
}

fn evict_lru<T>(entries: &mut HashMap<String, CacheEntry<T>>, capacity: usize) {
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

fn rebase_access_serials<T>(entries: &mut HashMap<String, CacheEntry<T>>) -> u64 {
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
```

Do not convert an implausible candidate into a pending lookup or a ten-minute miss. A successful cached candidate remains resolved while its display label is independently optional.

- [ ] **Step 5: Update all typed test helpers and run focused verification**

Update every string-literal route construction in `tests/flight_data.rs` and `tests/runtime_flight.rs` to use a validated `RouteCandidate`. Leave model strings unchanged. Update direct lookup assertions to inspect `candidate.label_for(&aircraft)` instead of comparing a candidate to a string.

In the private `access_serial_rebases_at_u64_max_without_losing_lru_order` test at the bottom of `src/flight_data.rs`, use `cache.model_entries` for both inserted string entries and assertions. The test covers serial rebasing rather than route semantics, so the model map keeps it focused and avoids constructing unrelated route geometry.

Run:

```bash
cargo fmt
cargo nextest run --test route_confidence --test flight_data --test runtime_flight
cargo clippy --all-targets --all-features -- -D warnings
```

Expected: all focused tests and strict clippy pass; the worker movement test records exactly one provider call and clears only the implausible route label.

- [ ] **Step 6: Commit only the provider, cache, fixture, and focused test files**

```bash
but stage src/flight_data.rs codex/route-confidence
but stage tests/flight_data.rs codex/route-confidence
but stage tests/runtime_flight.rs codex/route-confidence
but stage tests/fixtures/adsbdb/callsign.json codex/route-confidence
but stage tests/fixtures/adsbdb/combined.json codex/route-confidence
but commit codex/route-confidence --only -m "fix: suppress implausible ADSBDB routes"
```

Expected: no `src/web.rs` or `tests/web.rs` hunks are assigned to this stack.

---

### Task 3: Accuracy Documentation and Full Verification

**Files:**

- Modify: `README.md`
- Modify: `docs/architecture.md`

**Interfaces:**

- Consumes: the approved behavior implemented by Tasks 1 and 2.
- Produces: an accurate public explanation of candidate routes and a maintainer-facing cache/data-flow description.

- [ ] **Step 1: Update public route wording**

After the existing ADSBDB privacy-boundary paragraph in `README.md`, add:

```markdown
ADSBDB route records are static candidates, not live flight plans. Plane Radar
uses the returned airport coordinates and each aircraft's live position to hide
clearly incompatible routes, and it shows a returned midpoint explicitly. A
plausible label can still be outdated or have the wrong direction; authoritative
operational routes require a current-flight provider.
```

- [ ] **Step 2: Update the architecture description**

Replace the string-only cache sentence in `docs/architecture.md` with:

```markdown
Old aircraft replies are discarded when their location or range no longer
matches. The enrichment cache keeps structured ADSBDB route candidates and
model strings for six hours and definite misses for ten minutes, with bounded
least-recently-used eviction. Cached route candidates are re-evaluated against
every live aircraft position using a conservative great-circle corridor;
implausible candidates publish no route, while valid midpoints remain visible
in the compact label.
```

- [ ] **Step 3: Run documentation checks and the complete repository gate**

```bash
git diff --check -- README.md docs/architecture.md
mise run verify
```

Expected: formatting, all-target/all-feature strict clippy, cargo-deny, and the complete nextest suite pass. Existing radar golden images remain unchanged because no approved route fixture is added to a golden frame.

- [ ] **Step 4: Review the final diff against the specification**

Run:

```bash
but diff codex/route-confidence
but status
```

Confirm all of the following before committing:

- `IAH→ABQ` is rejected at the configured Manhattan position.
- cached candidates are evaluated per live position rather than once per provider response;
- midpoint labels contain all three codes;
- malformed routes cannot discard valid model data;
- route and model TTLs, LRU capacity, request cadence, and worker backoff remain unchanged;
- no settings or web UX file belongs to the route-confidence stack; and
- there are no placeholders, debugging output, new dependencies, or unrelated edits.

- [ ] **Step 5: Commit the documentation with GitButler**

```bash
but stage README.md codex/route-confidence
but stage docs/architecture.md codex/route-confidence
but commit codex/route-confidence --only -m "docs: explain route confidence boundary"
```

- [ ] **Step 6: Re-run the exact final gate on the committed stack**

```bash
mise run verify
but status
```

Expected: `mise run verify` passes on the committed route-confidence stack. Any concurrent settings-UI changes may remain assigned to their own stack, but the route-confidence stack itself has no uncommitted work.

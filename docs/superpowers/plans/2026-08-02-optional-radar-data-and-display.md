# Optional Radar Data and Display Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add opt-in ADSBDB routes and compact models, a configurable weather/time footer, radar text sizing, callsign visibility, and altitude filtering without changing the default radar or slowing its ADS-B feed.

**Architecture:** Preserve the current three-second ADS-B worker and immutable runtime snapshot, then add independent bounded workers for ADSBDB and Open-Meteo. Store optional service results alongside base aircraft, render them only when settings enable them, and migrate settings version 1 to compatibility-defaulted version 2 in memory.

**Tech Stack:** Rust 2024, serde/serde_json, ureq with rustls/WebPKI verification, `time` 0.3 for safe UTC-offset formatting, tiny-skia/fontdue rendering, tiny_http server-rendered HTML/CSS, cargo-nextest, PNG golden tests.

## Global Constraints

- Existing settings version 1 must load without a startup write and save as version 2 on the next successful mutation.
- Compatibility defaults are callsign on; route, expanded model, every footer item, and altitude bounds off; text size 100%; Celsius; 24-hour; radar-local time.
- Compatibility defaults must make no ADSBDB or Open-Meteo request and must preserve the existing radar goldens byte-for-byte.
- ADS-B positions retain the current three-second success cadence and failure backoff.
- ADSBDB and Open-Meteo I/O runs on independent worker threads and never holds a runtime-model lock.
- Every external request uses verified HTTPS, a bounded timeout, and a strict response-body limit.
- Route lookups send a normalized callsign; model lookups send a normalized aircraft hex; a combined request is permitted only when both features are enabled.
- ADSBDB requests are spaced by at least 750 ms, use a five-second timeout, back off 30 seconds after failure, cache successes six hours, and cache misses ten minutes.
- Open-Meteo refreshes no more often than every 15 minutes after success and uses a six-second timeout.
- Footer item order is condition, temperature, humidity, time, date; item selection is independent and order is not configurable.
- Time zone and 12/24-hour format are independent. Zulu-only time/date must not require Open-Meteo.
- Altitude bounds are inclusive feet values from -2,000 through 100,000; blank is unbounded; unknown altitude is excluded whenever either bound is active.
- Radar text scale is restricted to 80, 90, 100, 110, 120, or 130 percent and never scales radar geometry or aircraft symbols.
- Expanded model rendering strips manufacturer names but preserves meaningful variants such as `737-800`, `737 MAX 8`, and `A320-214`.
- Weather tokens are METAR-style labels derived from Open-Meteo WMO codes, not airport METAR observations; never invent cloud bases or `BKN`.
- The settings page remains dependency-free, server-rendered, accessible, responsive, and free of JavaScript or remote assets.
- Preserve all current HTTP session, CSRF, host, Origin/Referer, body-size, worker-count, escaping, and no-store controls.
- Rust remains `#![forbid(unsafe_code)]` and the project remains compatible with Rust 1.97.1.

---

## Scope and file structure

This remains one ordered plan because settings version 2 and `RuntimeSnapshot`
are shared foundations. Each task ends at a reviewer-sized, independently
testable commit.

### New files

| Path | Responsibility |
|---|---|
| `src/flight_data.rs` | ADSBDB request selection, strict response parsing, route/model normalization, and bounded positive/negative caches |
| `src/weather.rs` | Open-Meteo client, environment data parsing, WMO-to-aviation tokens, temperature/time/date formatting, and footer content assembly |
| `src/runtime/flight_worker.rs` | Independent nearest-first ADSBDB scheduling and model publication |
| `src/runtime/weather_worker.rs` | Independent weather/location-time scheduling and model publication |
| `src/render/footer.rs` | Measured one/two-row footer layout, rounded rail drawing, and footer avoidance bounds |
| `tests/flight_data.rs` | ADSBDB contract, normalization, cache, timeout, and fallback tests |
| `tests/weather.rs` | Open-Meteo contract, weather mapping, units, clocks, rollover, and staleness tests |
| `tests/runtime_flight.rs` | Enrichment worker cadence, priority, wake, failure, and stale-result tests |
| `tests/runtime_weather.rs` | Environment worker dependency, cadence, retry, location, and isolation tests |
| `tests/fixtures/adsbdb/combined.json` | Deterministic combined aircraft/route response |
| `tests/fixtures/adsbdb/callsign.json` | Deterministic callsign route response |
| `tests/fixtures/adsbdb/unknown.json` | Deterministic successful no-match response |
| `tests/fixtures/open_meteo/current.json` | Deterministic current weather and UTC-offset response |
| `tests/fixtures/settings/v1.json` | Exact legacy settings migration input |
| `tests/fixtures/settings/optional.json` | Fully enabled settings input for browser inspection |
| `tests/goldens/radar-enriched.png` | Callsign, route, compact detailed model, and altitude fixture |
| `tests/goldens/radar-footer.png` | Fully selected two-row footer fixture |
| `tests/goldens/radar-footer-large-stale.png` | 130% text and stale-weather fitting fixture |

### Existing files with focused changes

| Path | Responsibility after this plan |
|---|---|
| `Cargo.toml`, `Cargo.lock` | Add only the `time` formatting dependency and lock it |
| `src/http.rs` | Enforce a caller-selected maximum response body before allocating unbounded memory |
| `src/model.rs` | Own settings v2 types, aircraft identity/altitude, optional enrichment/environment snapshot data, and conditional publications |
| `src/settings.rs` | Strictly migrate v1 input and validate/save v2 |
| `src/adsb.rs` | Preserve hex/raw callsign/numeric altitude and filter before the 64-aircraft limit |
| `src/runtime.rs` | Broadcast settings changes, coordinate three workers, expose wall time, and join all workers on shutdown |
| `src/app.rs` | Pass enrichment/environment/wall time into the renderer and redraw on visible minute changes |
| `src/render/text.rs` | Fit measured text with deterministic ellipsis |
| `src/render/theme.rs` | Add footer metrics/colors while retaining all current 100% metrics |
| `src/render/radar.rs` | Scale typography, assemble conditional tag lines, draw the footer below traffic, and avoid footer bounds |
| `src/render/mod.rs`, `src/lib.rs` | Register the new focused modules |
| `src/web.rs` | Parse, validate, preserve, and render all new settings groups |
| `src/install.rs` | Seed settings JSON version 2 without changing the installer ownership-marker version |
| `src/main.rs` | Preserve configurable provider bases when constructing `RuntimeConfig` |
| `tests/http.rs` or `src/http.rs` unit tests | Prove body-limit behavior and TLS enforcement |
| `tests/settings.rs`, `tests/adsb.rs`, `tests/runtime.rs`, `tests/app.rs`, `tests/web.rs`, `tests/render_radar.rs`, `tests/install.rs` | Extend the existing boundary suites |
| `README.md`, `docs/install.md`, `docs/architecture.md`, `tests/docs_contract.rs` | Document controls, providers, privacy, worker isolation, and operational behavior |

### Cross-task interfaces

Use these names consistently:

```rust
pub const SETTINGS_SCHEMA_VERSION: u32 = 2;

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

pub struct AircraftKey {
    pub hex: String,
    pub callsign: String,
}

pub struct AircraftEnrichment {
    pub route: Option<String>,
    pub model: Option<String>,
}

pub struct EnvironmentReading {
    pub temperature_celsius: f64,
    pub humidity_percent: u8,
    pub weather_code: u8,
    pub utc_offset_seconds: i32,
    pub fetched_at: Duration,
}

pub struct EnrichmentNeeds {
    pub route: bool,
    pub model: bool,
}

pub struct FooterContent {
    pub environment: Vec<FooterItem>,
    pub temporal: Vec<FooterItem>,
}

pub struct FooterBounds {
    pub left: f32,
    pub top: f32,
    pub right: f32,
    pub bottom: f32,
}
```

`AircraftKey` uses normalized Mode S hex plus the raw trimmed flight callsign.
The existing `Aircraft.callsign` remains the display fallback of flight then
hex, preserving current behavior for aircraft without a flight identifier.

---

### Task 1: Bound every provider response body

**Files:**
- Modify: `src/http.rs:8-72`
- Modify: `src/adsb.rs:6-52`
- Modify: `src/geocode.rs:14-135`
- Modify: `tests/adsb.rs:175-315`
- Modify: `tests/geocode.rs` request assertions

**Interfaces:**
- Produces: `HttpRequest::max_response_bytes: usize`
- Produces: `HttpError::BodyTooLarge`
- Consumed by: ADS-B, Nominatim, ADSBDB, and Open-Meteo clients

- [ ] **Step 1: Write failing HTTP body-limit tests**

Add a focused unit seam in `src/http.rs` so a body can be read through the same
limit logic without network I/O:

```rust
fn read_response_body(
    body: &mut ureq::Body,
    max_response_bytes: usize,
) -> Result<Vec<u8>, HttpError> {
    body.with_config()
        .limit(max_response_bytes as u64)
        .read_to_vec()
        .map_err(map_body_error)
}
```

Add tests asserting that `ureq::Error::BodyExceedsLimit(64)` maps to
`HttpError::BodyTooLarge`, ordinary body errors remain `HttpError::Body`, and a
request with `max_response_bytes == 0` is rejected as `HttpError::InvalidBodyLimit`.

- [ ] **Step 2: Run the focused test and verify RED**

Run:

```bash
mise exec -- cargo test --locked --lib http::tests::body_limits_are_explicit_and_bounded -- --exact
```

Expected: compilation fails because the request field and new error variants do
not exist.

- [ ] **Step 3: Implement the bounded request contract**

Extend the public request and error types:

```rust
pub struct HttpRequest {
    pub url: String,
    pub query: Vec<(String, String)>,
    pub headers: Vec<(String, String)>,
    pub connect_timeout: Duration,
    pub read_timeout: Duration,
    pub max_response_bytes: usize,
    pub verify_tls: bool,
}

pub enum HttpError {
    InvalidTimeout,
    InvalidBodyLimit,
    Timeout,
    Transport,
    Body,
    BodyTooLarge,
    TlsVerificationRequired,
}
```

Reject a zero limit before building the agent, call `read_response_body` from
`UreqHttpClient::execute`, and map `ureq::Error::BodyExceedsLimit(_)` without
retaining provider data:

```rust
fn map_body_error(error: ureq::Error) -> HttpError {
    match error {
        ureq::Error::Timeout(_) => HttpError::Timeout,
        ureq::Error::BodyExceedsLimit(_) => HttpError::BodyTooLarge,
        _ => HttpError::Body,
    }
}
```

Set explicit existing-provider limits:

```rust
// src/adsb.rs
const MAX_RESPONSE_BYTES: usize = 2 * 1024 * 1024;

// src/geocode.rs
const MAX_RESPONSE_BYTES: usize = 256 * 1024;
```

- [ ] **Step 4: Update every existing `HttpRequest` fixture and assertion**

Add the exact expected limit to request constructors in `src/adsb.rs`,
`src/geocode.rs`, `src/http.rs` tests, `tests/adsb.rs`, and `tests/geocode.rs`.
Keep `verify_tls: true` unchanged.

- [ ] **Step 5: Run focused and provider suites for GREEN**

Run:

```bash
mise exec -- cargo test --locked --lib http::tests -- --nocapture
mise exec -- cargo test --locked --test adsb
mise exec -- cargo test --locked --test geocode
```

Expected: all tests pass, including exact ADS-B and Nominatim request bounds.

- [ ] **Step 6: Commit**

```bash
git add src/http.rs src/adsb.rs src/geocode.rs tests/adsb.rs tests/geocode.rs
git commit -m "fix: bound provider response bodies"
```

---

### Task 2: Add settings schema version 2 and strict migration

**Files:**
- Modify: `src/model.rs:5-45`
- Modify: `src/settings.rs:1-100`
- Modify: `src/install.rs:27-35`
- Modify: `src/web.rs:534-580`
- Modify: `src/render/radar.rs:576-642`
- Modify: explicit `RadarSettings` fixtures throughout `src/` and `tests/`
- Modify: `tests/settings.rs`
- Modify: `tests/install.rs:990-1030`
- Create: `tests/fixtures/settings/v1.json`
- Create: `tests/fixtures/settings/optional.json`

**Interfaces:**
- Produces: `SETTINGS_SCHEMA_VERSION`, `FooterSettings`, `TemperatureUnit`, `TimeZone`, `ClockFormat`
- Produces: settings fields named in the plan's cross-task interface
- Consumed by: every remaining task

- [ ] **Step 1: Add failing defaults, migration, and validation tests**

Create `tests/fixtures/settings/v1.json` with the exact legacy shape:

```json
{
  "schema_version": 1,
  "location": {
    "latitude": 40.7128,
    "longitude": -74.006,
    "label": "New York, NY"
  },
  "units": "mi",
  "show_runways": false,
  "range_index": 3
}
```

In `tests/settings.rs`, add tests that assert:

```rust
let migrated = validate_settings(
    serde_json::from_str(include_str!("fixtures/settings/v1.json")).unwrap(),
)
.expect("v1 migration");
assert_eq!(migrated.schema_version, SETTINGS_SCHEMA_VERSION);
assert!(migrated.show_callsign);
assert!(!migrated.show_route);
assert!(!migrated.show_expanded_model);
assert_eq!(migrated.radar_text_scale_percent, 100);
assert_eq!(migrated.minimum_altitude_feet, None);
assert_eq!(migrated.maximum_altitude_feet, None);
assert_eq!(migrated.footer, FooterSettings::default());
```

Also reject scales `79`, `81`, and `140`; bounds below `-2000` or above
`100000`; minimum above maximum; unknown fields inside `footer`; and schemas
`0` and `3`. Add a store-level migration test that writes the exact v1 bytes,
calls `SettingsStore::load`, verifies the file bytes are unchanged, then saves
the returned value and verifies the persisted JSON has schema 2 and every
compatibility default.

- [ ] **Step 2: Run the settings test and verify RED**

```bash
mise exec -- cargo test --locked --test settings
```

Expected: compilation fails because settings-v2 types and fields do not exist.

- [ ] **Step 3: Define settings-v2 types and compatibility defaults**

Add to `src/model.rs`:

```rust
pub const SETTINGS_SCHEMA_VERSION: u32 = 2;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TemperatureUnit { Celsius, Fahrenheit }

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TimeZone { RadarLocal, Zulu }

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClockFormat { Twelve, TwentyFour }

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
```

Extend `RadarSettings` with this exact shape and compatibility default:

```rust
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
        }
    }
}
```

Add helpers used later:

```rust
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

impl RadarSettings {
    pub fn altitude_filter_active(&self) -> bool {
        self.minimum_altitude_feet.is_some() || self.maximum_altitude_feet.is_some()
    }
}
```

- [ ] **Step 4: Implement strict v1-to-v2 parsing**

In `src/settings.rs`, inspect `schema_version` before deserializing. Define a
private `LegacyRadarSettingsV1` with `#[serde(deny_unknown_fields)]`, the five
legacy fields, and this conversion:

```rust
impl From<LegacyRadarSettingsV1> for RadarSettings {
    fn from(legacy: LegacyRadarSettingsV1) -> Self {
        RadarSettings {
            location: legacy.location,
            units: legacy.units,
            show_runways: legacy.show_runways,
            range_index: legacy.range_index,
            ..RadarSettings::default()
        }
    }
}
```

`validate_settings` accepts exactly versions 1 and 2, returns a version-2
value, then calls `validate_radar_settings`. `SettingsStore::save` accepts only
version 2 and retains the atomic tempfile/fsync path.

- [ ] **Step 5: Update installer defaults without changing ownership protocol**

Change only `DEFAULT_SETTINGS` to the pretty-printed version-2 default. Keep
`INSTALL_SETTINGS_MARKER`, the `settings-owned-v1` path, and `SETTINGS_MARKER`
unchanged because their version identifies installer ownership, not the runtime
settings schema.

Create `tests/fixtures/settings/optional.json` containing valid coordinates,
every optional display feature enabled, Fahrenheit, Zulu, twelve-hour time,
130% text, and `1000`/`45000` altitude bounds. Add an installer test that parses
the seeded file through `validate_settings` and receives schema 2.

- [ ] **Step 6: Make all existing constructors compile against schema 2**

Use `..RadarSettings::default()` in fixtures that do not need to assert every
field. Keep explicit legacy JSON only in migration tests. Update
`candidate_from_form` and renderer validation to use
`SETTINGS_SCHEMA_VERSION`; preserve the current 100% cap heights and rendering.

- [ ] **Step 7: Run settings, installer, web, app, and renderer suites**

```bash
mise exec -- cargo test --locked --test settings
mise exec -- cargo test --locked --test install
mise exec -- cargo test --locked --test web
mise exec -- cargo test --locked --test app
mise exec -- cargo test --locked --test render_radar
```

Expected: all suites pass and the three existing radar goldens remain exact.

- [ ] **Step 8: Commit**

```bash
git add src/model.rs src/settings.rs src/install.rs src/web.rs src/render/radar.rs src/runtime.rs src/app.rs tests/settings.rs tests/install.rs tests/web.rs tests/app.rs tests/render_radar.rs tests/runtime.rs tests/fixtures/settings/v1.json tests/fixtures/settings/optional.json
git commit -m "feat: migrate radar settings schema"
```

---

### Task 3: Build the optional settings UX

**Files:**
- Modify: `src/web.rs:520-570, 650-820, 1020-1410`
- Modify: `tests/web.rs:417-540, 880-970, 1100-1190`

**Interfaces:**
- Consumes: all settings-v2 fields and enums from Task 2
- Produces: complete atomic form parsing/rendering for the new preferences

- [ ] **Step 1: Write failing semantic and round-trip tests**

Add web tests for:

```rust
for expected in [
    "Aircraft labels",
    "Show callsign",
    "Show origin and destination",
    "Show expanded aircraft model",
    "Footer",
    "Weather condition",
    "Temperature",
    "Humidity",
    "Time",
    "Date",
    "Radar location",
    "Zulu",
    "12-hour",
    "24-hour",
    "Traffic filter",
    "Minimum altitude",
    "Maximum altitude",
    "ADSBDB",
    "Open-Meteo",
] {
    assert!(response.body.contains(expected), "missing {expected:?}");
}
assert!(!response.body.contains("<script"));
```

Post a fully enabled form and assert every stored field. Post every checkbox
with its `*_present` sentinel but without the checked value and assert it saves
false. Post duplicate, invalid enum, invalid scale, out-of-range altitude, and
minimum-above-maximum values and assert HTTP 400 with no replacement.

- [ ] **Step 2: Run focused web tests and verify RED**

```bash
mise exec -- cargo test --locked --test web optional_settings -- --nocapture
```

Expected: tests fail because the controls and parser branches are absent.

- [ ] **Step 3: Add strict reusable form parsers**

Add private helpers:

```rust
fn checkbox(form: &Form, name: &str, current: bool) -> Result<bool, FormError>;
fn optional_i32(form: &Form, name: &str) -> Result<Option<i32>, FormError>;
fn parse_temperature_unit(value: &str) -> Result<TemperatureUnit, FormError>;
fn parse_time_zone(value: &str) -> Result<TimeZone, FormError>;
fn parse_clock_format(value: &str) -> Result<ClockFormat, FormError>;
```

`checkbox` changes a value only when `{name}_present` exists, accepts one
checked value of `true` or `on`, rejects duplicates and unexpected values, and
otherwise returns `current` for search-result forms. `optional_i32` maps a
missing or trimmed empty value to `None` and rejects non-integers.

Define structured form errors:

```rust
#[derive(Clone, Copy)]
enum SettingsSection { Aircraft, Footer, Traffic }

struct FormError {
    section: Option<SettingsSection>,
    message: &'static str,
}
```

Return `Traffic` plus `Minimum altitude cannot exceed maximum altitude.` for
the crossed-bound case. Other invalid submissions retain the existing generic
copy.

- [ ] **Step 4: Extend `candidate_from_form` atomically**

Clone the current settings first, set `schema_version` to
`SETTINGS_SCHEMA_VERSION`, update only fields represented in the form, and run
the same `validate_settings(serde_json::to_value(candidate)?)` path before
`SettingsService::replace`.

Use exact submitted values:

```text
show_callsign
show_route
show_expanded_model
radar_text_scale_percent
footer_show_condition
footer_show_temperature
footer_show_humidity
footer_show_time
footer_show_date
temperature_unit = celsius|fahrenheit
time_zone = radar_local|zulu
clock_format = twelve|twenty_four
minimum_altitude_feet
maximum_altitude_feet
```

- [ ] **Step 5: Render progressive-disclosure groups**

Add native `<details>` groups with `data-section="aircraft"`, `footer`, and
`traffic`. Keep the existing units, range, and runway controls in `Radar
display`, and add text size there rather than inside a disclosure. Render each
disclosure `open` when its group is non-default or owns the current error. Use
existing `.switch` markup and hidden presence sentinels. Render text size as an
accessible `<select>`:

```html
<label class="field" for="radar-text-size">
  Radar text size
  <select id="radar-text-size" name="radar_text_scale_percent">
    <option value="80">80% — Small</option>
    <option value="90">90%</option>
    <option value="100">100% — Current</option>
    <option value="110">110%</option>
    <option value="120">120%</option>
    <option value="130">130% — Large</option>
  </select>
</label>
```

The Aircraft copy states that route sends the flight callsign to ADSBDB, model
sends the aircraft identifier, and enabling both may combine them. Footer copy
states that weather and radar-local time send configured coordinates to
Open-Meteo, while Zulu-only time/date do not.

- [ ] **Step 6: Extend responsive CSS without JavaScript**

Include `select` in font, 44-pixel minimum-height, focus-visible, and input
styling selectors. Give option groups border separators instead of nested SaaS
cards. Use two-column `.paired-fields` only above 34rem, retain the 52rem
location/preferences split, and ensure long disclosure copy wraps.

- [ ] **Step 7: Prove search selection and validation behavior**

Run:

```bash
mise exec -- cargo test --locked --test web
```

Expected: all existing security/privacy tests and all new settings tests pass.
Search-result selection changes location while preserving every optional
preference from the cloned current settings.

- [ ] **Step 8: Inspect the settings page at target viewports**

Start the headless server with the tracked optional fixture:

```bash
preview_dir="$(mktemp -d)"
cp tests/fixtures/settings/optional.json "$preview_dir/settings.json"
mise exec -- cargo run --locked -- run --headless \
  --settings "$preview_dir/settings.json" \
  --geocode-cache "$preview_dir/geocode.json" \
  --http 127.0.0.1:8080 \
  --local-url http://127.0.0.1:8080
```

Open `http://127.0.0.1:8080` and inspect 375×812, 768×1024, and 1440×900.
Verify control labels, keyboard focus, expanded summaries, no horizontal
scrolling, and a reachable Apply button. Stop the server with Ctrl-C and print
the exact `preview_dir` path in the execution notes; automated cleanup is not
required for this temporary preview artifact.

- [ ] **Step 9: Commit**

```bash
git add src/web.rs tests/web.rs
git commit -m "feat: add optional radar settings UX"
```

---

### Task 4: Preserve ADS-B identity and filter altitude

**Files:**
- Modify: `src/model.rs:45-75`
- Modify: `src/adsb.rs:20-145`
- Modify: `src/runtime.rs:105-180`
- Modify: `tests/fixtures/adsb/aircraft.json`
- Modify: `tests/adsb.rs`
- Modify: `tests/runtime.rs`
- Modify: Aircraft fixtures in `src/render/radar.rs`, `tests/app.rs`, and `tests/render_radar.rs`

**Interfaces:**
- Produces: `Aircraft.hex`, `Aircraft.flight_callsign`, `Aircraft.altitude_feet`
- Produces: `AircraftKey` and `Aircraft::key()`
- Produces: `AltitudeFilter::from(&RadarSettings)` and `AltitudeFilter::allows(Option<i32>)`
- Consumed by: ADSBDB client, runtime model, workers, and tag renderer

- [ ] **Step 1: Write failing identity and filter tests**

Extend parser assertions:

```rust
assert_eq!(first.hex, "a835af");
assert_eq!(first.flight_callsign, "UAL123");
assert_eq!(first.callsign, "UAL123");
assert_eq!(first.altitude_feet, Some(33_000));
```

For the record with no flight, assert `flight_callsign == ""`, `hex ==
"abc009"`, and display `callsign == "abc009"`. Add table tests for minimum,
maximum, equal bounds, negative altitude, geometric fallback, unknown altitude,
and no bounds. Put an unknown-altitude record before valid records and prove
filtering occurs before the accepted-aircraft `max` limit. Add a model test
showing that enabling or tightening a bound immediately removes currently
stored disallowed aircraft before the next provider response.

- [ ] **Step 2: Run the ADS-B suite and verify RED**

```bash
mise exec -- cargo test --locked --test adsb
```

Expected: compilation fails because identity, numeric altitude, and filter
interfaces do not exist.

- [ ] **Step 3: Add stable aircraft identity and numeric altitude**

In `src/model.rs`:

```rust
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
```

Add `hex`, `flight_callsign`, and `altitude_feet: Option<i32>` to `Aircraft`.
Keep `callsign` as trimmed flight with hex fallback. Parse numeric barometric
then geometric altitude, round once, use the same value for display formatting
and filtering, and reject non-finite/out-of-`i32` values.

- [ ] **Step 4: Implement inclusive filtering before truncation**

Define:

```rust
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AltitudeFilter {
    pub minimum_feet: Option<i32>,
    pub maximum_feet: Option<i32>,
}

impl AltitudeFilter {
    pub fn allows(self, altitude: Option<i32>) -> bool {
        if self.minimum_feet.is_none() && self.maximum_feet.is_none() {
            return true;
        }
        let Some(altitude) = altitude else { return false; };
        self.minimum_feet.is_none_or(|minimum| altitude >= minimum)
            && self.maximum_feet.is_none_or(|maximum| altitude <= maximum)
    }
}
```

Pass the filter through these exact signatures:

```rust
pub fn fetch(
    &self,
    location: &Location,
    radius_km: f64,
    filter: AltitudeFilter,
) -> Result<Vec<Aircraft>, AdsbError>;

pub fn parse_aircraft(
    value: &Value,
    max: usize,
    show_ground: bool,
    filter: AltitudeFilter,
) -> Result<Vec<Aircraft>, AdsbError>;
```

Continue scanning records until `max` accepted aircraft are collected. In
`AdsbWorker`, derive the filter from the same settings snapshot as
location/range, and extend both conditional-publication methods to include it:

```rust
pub fn record_aircraft_if_query(
    &self,
    expected_location: &Location,
    expected_range_index: u8,
    expected_filter: AltitudeFilter,
    aircraft: Vec<Aircraft>,
    fetched_at: Duration,
) -> Option<u64>;

pub fn record_adsb_error_if_query(
    &self,
    expected_location: &Location,
    expected_range_index: u8,
    expected_filter: AltitudeFilter,
    at: Duration,
) -> Option<u64>;
```

Reject an in-flight response whenever the current settings derive a different
filter from `expected_filter`. In `RuntimeModel::replace_settings`, compare the
old and new `AltitudeFilter`; when only the filter changes, immediately retain
only current aircraft allowed by the new filter and keep the current fetch
timestamp. Loosening a filter may remain temporarily under-populated until the
already-woken ADS-B worker completes its next fetch, but must never show an
aircraft disallowed by the newly saved bounds.

- [ ] **Step 5: Update model fixtures explicitly**

Give every synthetic aircraft a bounded hex/raw-callsign combination and
numeric altitude matching its display altitude. Empty/unrenderable fixtures use
empty identifiers and `None` altitude rather than arbitrary production data.

- [ ] **Step 6: Run ADS-B, runtime, app, and renderer suites**

```bash
mise exec -- cargo test --locked --test adsb
mise exec -- cargo test --locked --test runtime
mise exec -- cargo test --locked --test app
mise exec -- cargo test --locked --test render_radar
```

Expected: all pass; unbounded defaults preserve existing renderer goldens.

- [ ] **Step 7: Commit**

```bash
git add src/model.rs src/adsb.rs src/runtime.rs src/render/radar.rs tests/adsb.rs tests/runtime.rs tests/app.rs tests/render_radar.rs tests/fixtures/adsb/aircraft.json
git commit -m "feat: filter aircraft by altitude"
```

---

### Task 5: Implement the ADSBDB client and bounded cache

**Files:**
- Create: `src/flight_data.rs`
- Modify: `src/lib.rs`
- Create: `tests/flight_data.rs`
- Create: `tests/fixtures/adsbdb/combined.json`
- Create: `tests/fixtures/adsbdb/callsign.json`
- Create: `tests/fixtures/adsbdb/unknown.json`

**Interfaces:**
- Consumes: `HttpClient`, `Clock`, `Sleeper`, `Aircraft`, `AircraftKey`
- Produces: `AircraftEnrichment`, `EnrichmentNeeds`, `LookupValue<T>`, `FlightLookup`
- Produces: `FlightDataClient<C, K, S>::lookup(&Aircraft, EnrichmentNeeds)`
- Produces: `EnrichmentCache::resolve` and `EnrichmentCache::record`
- Consumed by: Task 7 runtime model and Task 8 flight worker

- [ ] **Step 1: Create exact response fixtures and failing parser tests**

Use this combined fixture shape:

```json
{
  "response": {
    "aircraft": {
      "type": "Boeing 737-800",
      "icao_type": "B738"
    },
    "flightroute": {
      "origin": { "iata_code": "JFK", "icao_code": "KJFK" },
      "destination": { "iata_code": "LAX", "icao_code": "KLAX" }
    }
  }
}
```

Use the same `flightroute` object in `callsign.json` and
`{"response":"unknown aircraft"}` in `unknown.json`. Test IATA preference,
ICAO fallback, rejection of one-ended routes, manufacturer stripping, preserved
`737 MAX 8`, whitespace normalization, `icao_type` fallback, malformed JSON,
wrong root types, 404 misses, and non-200/non-404 errors.
Also prove that a non-HTTPS provider base is rejected before `HttpClient` is
called. With fake monotonic time and a fake sleeper, prove that a combined
lookup's callsign fallback starts at least 750 ms after its first request.

- [ ] **Step 2: Run the new suite and verify RED**

```bash
mise exec -- cargo test --locked --test flight_data
```

Expected: compilation fails because `planeradar::flight_data` is absent.

- [ ] **Step 3: Define the strict client result types**

In `src/flight_data.rs`:

```rust
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct EnrichmentNeeds { pub route: bool, pub model: bool }

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct AircraftEnrichment {
    pub route: Option<String>,
    pub model: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum LookupValue<T> { NotRequested, Found(T), Missing }

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FlightLookup {
    pub route: LookupValue<String>,
    pub model: LookupValue<String>,
}
```

Define `FlightDataError` variants for HTTP, status, JSON, and schema failures.
Set `connect_timeout: Duration::from_secs(2)`,
`read_timeout: Duration::from_secs(3)`, a 256 KiB body limit, and
`verify_tls: true`. `FlightDataClient<C, K, S>` stores `HttpClient`, `Clock`,
`Sleeper`, and `last_request_at: Option<Duration>`.
`FlightDataClient::with_provider_base(http, clock, sleeper, base)` stores a base
with trailing slashes removed; `lookup` parses it with `url::Url` and requires
the `https` scheme before constructing a request. Expose the client operation
as:

```rust
pub fn lookup(
    &mut self,
    aircraft: &Aircraft,
    needs: EnrichmentNeeds,
) -> Result<FlightLookup, FlightDataError>;
```

Use this small worker seam and implement it for `FlightDataClient` by forwarding
to the inherent method:

```rust
pub trait FlightDataService: Send {
    fn lookup(
        &mut self,
        aircraft: &Aircraft,
        needs: EnrichmentNeeds,
    ) -> Result<FlightLookup, FlightDataError>;
}
```

Route every HTTP call, including the combined-response callsign fallback,
through one private `execute` method. Before a call, sleep for
`750 ms - clock.monotonic().saturating_sub(last_request_at)` when positive;
immediately before `HttpClient::execute`, store that monotonic start time. This
guarantees spacing for both normal worker scans and the otherwise-hidden
fallback request.

- [ ] **Step 4: Implement minimum-data endpoint selection**

Normalize identifiers with ASCII alphanumeric uppercase only. Return a
definite missing value when an enabled field lacks its required identifier.
Build requests as follows:

```text
route only:  {base}/callsign/{CALLSIGN}
model only:  {base}/aircraft/{HEX}
both URL:    {base}/aircraft/{HEX}
both query:  [("callsign", CALLSIGN)]
```

When both are requested but one identifier is absent, issue only the request
that can produce a requested field. If a combined 200 response has no route,
perform the callsign fallback. Never send callsign on a model-only request.
Build the combined request with `HttpRequest.query`; do not concatenate or
percent-encode the query string by hand.

- [ ] **Step 5: Implement normalization and strict parsing**

`compact_model` removes only these case-insensitive leading manufacturer names:
`Boeing`, `Airbus`, `Embraer`, `Bombardier`, and `De Havilland Canada`. It
collapses internal whitespace, preserves the rest verbatim, and falls back to
`icao_type` when `type` is blank. `parse_route` chooses non-empty IATA then ICAO
for each endpoint and returns `ORIGIN→DESTINATION` only when both exist.

- [ ] **Step 6: Write failing cache tests**

Test separate route/callsign and model/hex identity, six-hour success expiry,
ten-minute missing expiry, exact-boundary expiration, callsign change on one
hex, cache hits that request no field, partial hits that request one field, and
bounded least-recently-used eviction at a capacity of 256 entries per map.

- [ ] **Step 7: Implement the separate-key cache**

Use private cache entries containing `Option<String>`, `expires_at: Duration`,
and a monotonic access serial. Expose:

```rust
pub struct CacheResolution {
    pub enrichment: AircraftEnrichment,
    pub pending: EnrichmentNeeds,
}

impl EnrichmentCache {
    pub fn new(capacity: usize) -> Self;
    pub fn resolve(
        &mut self,
        aircraft: &Aircraft,
        needs: EnrichmentNeeds,
        now: Duration,
    ) -> CacheResolution;
    pub fn record(
        &mut self,
        aircraft: &Aircraft,
        requested: EnrichmentNeeds,
        lookup: &FlightLookup,
        now: Duration,
    );
}
```

Expired entries are removed before resolution. `Missing` stores `None` for ten
minutes; `Found` stores the value for six hours; `NotRequested` leaves that map
untouched.

- [ ] **Step 8: Run the complete ADSBDB suite**

```bash
mise exec -- cargo test --locked --test flight_data
```

Expected: every request, parser, normalization, fallback, and cache test passes.

- [ ] **Step 9: Commit**

```bash
git add src/flight_data.rs src/lib.rs tests/flight_data.rs tests/fixtures/adsbdb
git commit -m "feat: add ADSBDB enrichment client"
```

---

### Task 6: Implement Open-Meteo data and footer formatting

**Files:**
- Modify: `Cargo.toml`, `Cargo.lock`
- Create: `src/weather.rs`
- Modify: `src/lib.rs`
- Modify: `src/model.rs`
- Create: `tests/weather.rs`
- Create: `tests/fixtures/open_meteo/current.json`

**Interfaces:**
- Consumes: settings-v2 footer enums, `HttpClient`, `Location`
- Produces: `EnvironmentReading`, `WeatherClient<C>`
- Produces: `FooterTone`, `FooterItem`, `FooterContent`, `footer_content`, `environment_is_stale`
- Consumed by: runtime environment worker and footer renderer

- [ ] **Step 1: Add the safe time-formatting dependency**

Add exactly:

```toml
time = { version = "0.3", features = ["formatting"] }
```

Run `mise exec -- cargo check` to resolve and update `Cargo.lock`, then run
`mise exec -- cargo check --locked` and confirm the selected `time` release
supports Rust 1.97.1.

- [ ] **Step 2: Create the Open-Meteo fixture and failing client tests**

Create:

```json
{
  "utc_offset_seconds": -14400,
  "current": {
    "time": "2026-08-02T10:15",
    "temperature_2m": 22.2,
    "relative_humidity_2m": 54,
    "weather_code": 2
  }
}
```

Assert the exact request base, query keys, `temperature_unit=celsius`,
`timezone=auto`, one forecast day, six-second global budget, 64 KiB body limit,
and verified TLS. The exact timeouts are
`connect_timeout: Duration::from_secs(2)` and
`read_timeout: Duration::from_secs(4)`. Test malformed JSON, absent current
fields, humidity outside 0–100, weather code outside `u8`, UTC offset outside
±86,400, and non-200 status.

The request query is exactly:

```text
latitude={location.latitude}
longitude={location.longitude}
current=temperature_2m,relative_humidity_2m,weather_code
temperature_unit=celsius
timezone=auto
forecast_days=1
```

Also prove that a non-HTTPS provider base is rejected before `HttpClient` is
called.

- [ ] **Step 3: Run the weather suite and verify RED**

```bash
mise exec -- cargo test --locked --test weather
```

Expected: compilation fails because `planeradar::weather` and environment types
do not exist.

- [ ] **Step 4: Define and implement `WeatherClient`**

Put `EnvironmentReading` in `src/model.rs` with the exact cross-task fields.
`WeatherClient<C>` stores an HTTP client and provider base through
`WeatherClient::with_provider_base(http, base)`, strips trailing slashes, and
requires an `https` `url::Url` before any request. It then exposes:

```rust
pub fn fetch(
    &self,
    location: &Location,
    fetched_at: Duration,
) -> Result<EnvironmentReading, WeatherError>;
```

Parse only numeric finite fields, convert humidity/weather code with checked
integer conversions, and keep temperature in Celsius regardless of display
preference.

- [ ] **Step 5: Write failing METAR-style mapping tests**

Use a complete table:

```rust
let cases = [
    (0, "CLR"), (1, "FEW"), (2, "SCT"), (3, "OVC"),
    (45, "FG"), (48, "FG"),
    (51, "-DZ"), (53, "DZ"), (55, "+DZ"),
    (56, "-FZDZ"), (57, "+FZDZ"),
    (61, "-RA"), (63, "RA"), (65, "+RA"),
    (66, "-FZRA"), (67, "+FZRA"),
    (71, "-SN"), (73, "SN"), (75, "+SN"), (77, "SG"),
    (80, "-SHRA"), (81, "SHRA"), (82, "+SHRA"),
    (85, "-SHSN"), (86, "+SHSN"),
    (95, "TS"), (96, "TSGR"), (99, "TSGR"),
    (4, "WX"),
];
```

Assert that no output is `BKN` and no token contains an invented cloud height.

- [ ] **Step 6: Implement pure footer formatting**

Define:

```rust
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FooterTone { Status, Condition, Temperature, Humidity, Time, Date }

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FooterItem { pub text: String, pub tone: FooterTone }

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct FooterContent {
    pub environment: Vec<FooterItem>,
    pub temporal: Vec<FooterItem>,
}

pub const ENVIRONMENT_STALE_AFTER: Duration = Duration::from_secs(45 * 60);

pub fn environment_is_stale(
    reading: Option<&EnvironmentReading>,
    monotonic_now: Duration,
) -> bool {
    reading.is_some_and(|value| {
        monotonic_now.saturating_sub(value.fetched_at) >= ENVIRONMENT_STALE_AFTER
    })
}

pub fn footer_content(
    settings: &FooterSettings,
    reading: Option<&EnvironmentReading>,
    monotonic_now: Duration,
    unix_seconds: u64,
) -> FooterContent;
```

Use `time::OffsetDateTime::from_unix_timestamp` and
`time::UtcOffset::from_whole_seconds`; do not use libc or unsafe code. Format
Celsius/Fahrenheit with zero decimals, humidity as `RH54%`, Zulu time with `Z`,
local time without a suffix, twelve-hour time with `AM`/`PM`, and dates as
`DD MON`.

Before the first environment result, selected weather collapses to one `WX --`
item. Zulu temporal values still render. Radar-local time/date use `--:--` and
`-- ---` until an offset exists. At age `>= 45 minutes`, prepend `WX STALE` and
retain every selected last-known value. Use `environment_is_stale` for that
decision so app redraw keying and footer formatting share the exact boundary.

- [ ] **Step 7: Test units, clocks, rollover, and staleness**

Construct test epochs with the `time` crate rather than hand-written Unix
numbers. Cover Celsius/Fahrenheit rounding, midnight date rollover for positive
and negative offsets, noon/midnight twelve-hour formatting, Zulu independence,
unavailable local time, 44:59 freshness, 45:00 staleness, and monotonic
underflow.

Run:

```bash
mise exec -- cargo test --locked --test weather
```

Expected: all client and pure-formatting tests pass.

- [ ] **Step 8: Commit**

```bash
git add Cargo.toml Cargo.lock src/model.rs src/weather.rs src/lib.rs tests/weather.rs tests/fixtures/open_meteo
git commit -m "feat: add weather and time formatting"
```

---

### Task 7: Extend the immutable runtime model

**Files:**
- Modify: `src/model.rs:80-210`
- Modify: `tests/runtime.rs:1-260`
- Modify: `tests/app.rs` snapshot fixtures
- Modify: `tests/render_radar.rs` snapshot fixtures
- Modify: `src/render/radar.rs` fixture snapshots

**Interfaces:**
- Consumes: `AircraftKey`, `AircraftEnrichment`, `EnvironmentReading`
- Produces: `RuntimeSnapshot.enrichment`, `RuntimeSnapshot.environment`
- Produces: matching fields in `RadarSnapshot`
- Produces: conditional enrichment/environment publication methods
- Consumed by: workers, app, and renderer

- [ ] **Step 1: Write failing immutable-publication tests**

Add tests that assert:

```rust
assert!(model.record_enrichment_if_aircraft(&key, enrichment.clone()).is_some());
assert_eq!(model.snapshot().enrichment.get(&key), Some(&enrichment));
assert!(model.record_enrichment_if_aircraft(&departed_key, enrichment).is_none());
```

Test that replacing aircraft prunes departed display enrichment while retaining
entries for aircraft still present. Test that environment publication accepts
only the requested current location, location change clears environment, and a
weather error retains the last good reading. Every accepted visible mutation
bumps generation exactly once; rejected stale mutations do not bump.

- [ ] **Step 2: Run runtime tests and verify RED**

```bash
mise exec -- cargo test --locked --test runtime snapshot -- --nocapture
```

Expected: compilation fails because optional snapshot data and publications are
absent.

- [ ] **Step 3: Add immutable optional-data fields**

Use:

```rust
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

pub struct RadarSnapshot {
    pub aircraft: Arc<[Aircraft]>,
    pub enrichment: Arc<HashMap<AircraftKey, AircraftEnrichment>>,
    pub environment: Option<EnvironmentReading>,
    pub fetched_at: Option<Duration>,
    pub last_error_at: Option<Duration>,
}
```

Keep `environment_last_error_at` out of `RadarSnapshot`; footer staleness is
derived from the last successful reading's `fetched_at`. Keep service errors
separate from `last_error_at`, which remains ADS-B-only.

- [ ] **Step 4: Implement conditional publications under one write lock**

Add:

```rust
pub fn record_enrichment_if_aircraft(
    &self,
    key: &AircraftKey,
    enrichment: AircraftEnrichment,
) -> Option<u64>;

pub fn record_environment_if_location(
    &self,
    expected_location: &Location,
    reading: EnvironmentReading,
) -> Option<u64>;

pub fn record_environment_error_if_location(
    &self,
    expected_location: &Location,
    at: Duration,
) -> Option<u64>;
```

Do not bump when enrichment is byte-for-byte unchanged. On aircraft replacement,
or immediate altitude-filter pruning in `replace_settings`, retain only display
enrichment whose exact `AircraftKey` is still present. On location change,
clear environment and its error timestamp. Renderer settings, not model
deletion, remain responsible for immediately hiding disabled optional fields.

- [ ] **Step 5: Update every snapshot constructor explicitly**

Initialize enrichment to `Arc::new(HashMap::new())`, environment to `None`, and
environment error to `None` in runtime and renderer fixtures. Use helper
constructors to avoid repetitive fields in tests while preserving explicit
assertions at the model boundary.

- [ ] **Step 6: Run model consumers for GREEN**

```bash
mise exec -- cargo test --locked --test runtime
mise exec -- cargo test --locked --test app
mise exec -- cargo test --locked --test render_radar
```

Expected: all pass and existing default frames remain exact.

- [ ] **Step 7: Commit**

```bash
git add src/model.rs src/render/radar.rs tests/runtime.rs tests/app.rs tests/render_radar.rs
git commit -m "feat: publish optional radar data"
```

---

### Task 8: Run ADSBDB enrichment independently

**Files:**
- Create: `src/runtime/flight_worker.rs`
- Modify: `src/runtime.rs`
- Create: `tests/runtime_flight.rs`

**Interfaces:**
- Consumes: mutable `FlightDataClient`, `EnrichmentCache`, `RuntimeModel`, `Clock`, `Waiter`, `WorkerCommand`
- Produces: `FlightDataWorker<D, K, W>` where `D` implements the lookup seam used by tests
- Consumed by: `RuntimeCoordinator` in Task 9

- [ ] **Step 1: Write failing scheduler tests**

Use fake HTTP, clock, and waiter types following `tests/runtime.rs`. Cover:

- no request with both enrichment settings off;
- model-only sends hex without callsign;
- route-only sends callsign without hex;
- nearest uncached aircraft is first;
- successful/missing cache entries suppress duplicate requests;
- starts are at least 750 ms apart;
- network failure waits 30 seconds;
- settings change wakes disabled/failed waits;
- disabling during an in-flight result keeps it hidden;
- a departed aircraft rejects late publication; and
- cached results can publish without a network request.

- [ ] **Step 2: Run the new worker suite and verify RED**

```bash
mise exec -- cargo test --locked --test runtime_flight
```

Expected: compilation fails because `FlightDataWorker` is absent.

- [ ] **Step 3: Register the runtime submodule**

At the top of `src/runtime.rs`:

```rust
mod flight_worker;
pub use flight_worker::FlightDataWorker;
```

Make `wait_for_command` and command-draining helpers `pub(crate)` so the worker
shares the current interruptible stop/settings behavior without copying it.

- [ ] **Step 4: Implement the nearest-first worker loop**

`FlightDataWorker<D, K, W>` owns its mutable lookup service and
`EnrichmentCache::new(256)`. Production supplies
`FlightDataClient<UreqHttpClient, SharedClock, ThreadSleeper>`; tests supply a
deterministic fake. Each scan:

1. snapshots settings and aircraft without holding the lock afterward;
2. derives `EnrichmentNeeds` from settings;
3. waits 30 seconds when disabled or unconfigured;
4. resolves cached values for current aircraft and conditionally publishes them;
5. selects the closest aircraft with a pending requested field using
   `geometry::offset_km` and finite hypotenuse distance;
6. performs one lookup;
7. records success/miss in cache and publishes only if aircraft/settings remain
   current; and
8. waits 750 ms after success/miss or 30 seconds after failure.

When enabled but no candidate exists, scan again after three seconds so newly
published base aircraft are discovered within one ADS-B interval without a
second cross-worker notification channel.

- [ ] **Step 5: Rate-limit sanitized service logs**

Log provider name, error category, and normalized aircraft identity only at the
start of a new 30-second failure window. Do not log URLs, response bodies,
coordinates, or repeated failures inside the window.

- [ ] **Step 6: Run worker and cache suites**

```bash
mise exec -- cargo test --locked --test flight_data
mise exec -- cargo test --locked --test runtime_flight
```

Expected: all pass with deterministic monotonic timestamps.

- [ ] **Step 7: Commit**

```bash
git add src/runtime.rs src/runtime/flight_worker.rs tests/runtime_flight.rs
git commit -m "feat: run aircraft enrichment independently"
```

---

### Task 9: Run environment updates independently and coordinate workers

**Files:**
- Create: `src/runtime/weather_worker.rs`
- Modify: `src/runtime.rs:20-500`
- Modify: `src/main.rs:50-75`
- Modify: `src/app.rs:20-205`
- Modify: `tests/runtime.rs`
- Create: `tests/runtime_weather.rs`
- Modify: `tests/app.rs`

**Interfaces:**
- Consumes: `WeatherClient`, `EnvironmentReading`, runtime publications
- Produces: `WeatherWorker<C, K, W>`
- Produces: `RuntimeHandle::unix_seconds()` and `AppRuntime::unix_seconds()`
- Produces: three-channel settings broadcast and joined shutdown
- Consumed by: renderer integration in Tasks 10 and 11

- [ ] **Step 1: Write failing weather-worker tests**

Cover this dependency table:

```text
all footer items off                         no request
Zulu time/date only                          no request
condition, temperature, or humidity          request
radar-local time or date                     request
```

Also test immediate first fetch, immediate location-change fetch, 15-minute
success interval, failure waits of 30 s, 60 s, 5 min, then 15 min, reset after
success, settings wake, disabled in-flight result rejection, last-good reading
retention, and stop interruption.

- [ ] **Step 2: Run the new suite and verify RED**

```bash
mise exec -- cargo test --locked --test runtime_weather
```

Expected: compilation fails because `WeatherWorker` and coordinator channels
are absent.

- [ ] **Step 3: Implement `WeatherWorker`**

Register and re-export the submodule in `src/runtime.rs`:

```rust
mod weather_worker;
pub use weather_worker::WeatherWorker;
```

`WeatherWorker<C, K, W>` snapshots location/footer settings, calls
`FooterSettings::needs_environment`, and uses the existing interruptible
waiter. Pass `clock.monotonic()` into `WeatherClient::fetch`; publish only when
the exact location still matches. Record service errors separately and never
touch ADS-B `last_error_at`.

Track consecutive failures and wait 30 seconds, 60 seconds, 5 minutes, then 15
minutes for the fourth and every later failure. Reset that counter after a
success. Log `Open-Meteo` plus only the sanitized error category at the start
of each new retry window; do not log coordinates, URLs, bodies, or repeated
errors within one window.

- [ ] **Step 4: Broadcast settings to all workers**

Replace the single channel notifier with:

```rust
struct ChannelSettingsNotifier {
    senders: Vec<Sender<WorkerCommand>>,
}

impl SettingsNotifier for ChannelSettingsNotifier {
    fn settings_changed(&self, settings: RadarSettings) -> Result<(), ()> {
        for sender in &self.senders {
            sender
                .send(WorkerCommand::SettingsChanged(settings.clone()))
                .map_err(|_| ())?;
        }
        Ok(())
    }
}
```

Construct separate ADS-B, flight, and weather channels before building
`RuntimeSettingsService`. On shutdown, set the shared stop flag, send `Stop` to
all three senders, join all three workers plus web, and report any panic through
the existing `WorkerPanic` error. Replace the public single `commands` field and
two-worker handle layout with these exact private coordination fields:

```rust
pub struct RuntimeHandle {
    pub model: RuntimeModel,
    pub stop: Arc<AtomicBool>,
    settings: Arc<RuntimeSettingsService>,
    clock: SharedClock,
    commands: Vec<Sender<WorkerCommand>>,
    web_worker: Option<JoinHandle<Result<(), WebError>>>,
    adsb_worker: Option<JoinHandle<()>>,
    flight_worker: Option<JoinHandle<()>>,
    weather_worker: Option<JoinHandle<()>>,
}
```

`RuntimeHandle::shutdown` iterates over `commands` to send `Stop`, then takes
and joins each named worker exactly once.

- [ ] **Step 5: Add configurable provider bases**

Extend `RuntimeConfig`:

```rust
pub flight_data_url: String,
pub weather_url: String,
```

Defaults are `https://api.adsbdb.com/v0` and
`https://api.open-meteo.com/v1/forecast`. In `src/main.rs`, begin with
`RuntimeConfig::default()`, overwrite the existing CLI-controlled paths/address/
URL fields, and retain provider defaults. Do not add public CLI flags.
Construct the workers with
`FlightDataClient::with_provider_base(UreqHttpClient, clock.clone(),
ThreadSleeper, config.flight_data_url)` and
`WeatherClient::with_provider_base(UreqHttpClient, config.weather_url)`.

- [ ] **Step 6: Expose wall time without unsafe code**

Add `RuntimeHandle::unix_seconds()` forwarding to `SharedClock` and add the
same method to `AppRuntime`. Test fakes keep separate atomics for monotonic
milliseconds and Unix seconds. No renderer behavior changes in this step.

- [ ] **Step 7: Prove optional I/O cannot block ADS-B publication**

In `tests/runtime_weather.rs`, start a weather worker whose fake HTTP call blocks
on a channel. While it remains blocked, run an ADS-B worker with an immediate
`{"ac":[]}` response on another thread and assert the model reaches successful
ADS-B state before releasing weather. Repeat with a blocked flight worker in
`tests/runtime_flight.rs`.

- [ ] **Step 8: Run runtime and app suites**

```bash
mise exec -- cargo test --locked --test runtime
mise exec -- cargo test --locked --test runtime_flight
mise exec -- cargo test --locked --test runtime_weather
mise exec -- cargo test --locked --test app
```

Expected: all workers stop cleanly; optional blocks never delay ADS-B state.

- [ ] **Step 9: Commit**

```bash
git add src/runtime.rs src/runtime/weather_worker.rs src/main.rs src/app.rs tests/runtime.rs tests/runtime_flight.rs tests/runtime_weather.rs tests/app.rs
git commit -m "feat: run weather updates independently"
```

---

### Task 10: Render configurable text and enriched aircraft tags

**Files:**
- Modify: `src/render/text.rs`
- Modify: `src/render/radar.rs`
- Modify: `src/render/theme.rs`
- Modify: `tests/render_radar.rs`
- Create: `tests/goldens/radar-enriched.png`

**Interfaces:**
- Consumes: settings text/callsign/route/model fields and snapshot enrichment
- Produces: `TextRasterizer::fit_with_ellipsis`
- Produces: scaled conditional aircraft tags and text-aware `BackgroundKey`
- Consumed by: footer renderer

- [ ] **Step 1: Write failing text-fit and background-key tests**

Add unit tests in `src/render/text.rs` asserting a fitting string is unchanged,
an overlong string ends with one Unicode ellipsis, the result measures within
the requested width, an impossibly narrow width returns empty, and control
characters still sanitize through the existing glyph path.

Extend `background_key_uses_exact_float_bits_and_every_static_setting` so 100%
and 110% keys differ while callsign/route/footer/altitude settings do not alter
the static key.

- [ ] **Step 2: Write failing enriched-tag tests**

Construct one aircraft plus enrichment and assert color/text regions differ for:

- callsign plus route plus compact model plus altitude;
- hidden callsign with route first;
- expanded-model miss falling back to current short type;
- route miss creating no blank line;
- 80%, 100%, and 130% measured block sizes; and
- an overlong model staying on-screen.

Add `fixture_enriched()` and a golden assertion named `radar-enriched`.

- [ ] **Step 3: Run the renderer suite and verify RED**

```bash
mise exec -- cargo test --locked --test render_radar
```

Expected: compilation or assertions fail because text fitting, scaling, and
conditional tags are absent; the new golden is missing.

- [ ] **Step 4: Implement measured ellipsis**

Add:

```rust
pub fn fit_with_ellipsis(&self, text: &str, cap_height: f32, max_width: f32) -> String;
```

Return sanitized display characters only. If full text does not fit, remove
characters until `prefix + "…"` measures within `max_width`. Never slice UTF-8
by byte index and never return more than `MAX_TEXT_GLYPHS` displayed glyphs.

- [ ] **Step 5: Scale every existing radar text metric**

Add one helper:

```rust
fn text_scale(settings: &RadarSettings, cap_height: f32) -> f32 {
    cap_height * f32::from(settings.radar_text_scale_percent) / 100.0
}
```

Apply it to cardinal, range, runway, aircraft-tag, and `DATA STALE` cap heights.
Leave all theme source metrics and non-text geometry unchanged. Include
`radar_text_scale_percent` in `BackgroundKey`.

- [ ] **Step 6: Assemble dynamic aircraft tag lines**

Look up enrichment by `aircraft.key()` and build a `Vec<(&str, [u8; 4])>` in
this order: non-empty callsign when `show_callsign`; known route when
`show_route`; enriched model when `show_expanded_model` and known, otherwise
the current short type; then non-empty altitude. If callsign is hidden, route
is therefore first. Compute block height from `lines.len()`, fit each line to
the available width on its selected side, and omit empty fitted strings.

- [ ] **Step 7: Generate and inspect only the new enriched golden**

Extend `write_fixtures` with `radar-enriched.png`, then run:

```bash
fixture_dir="$(mktemp -d)"
mise exec -- cargo run --locked -- render-fixtures --output "$fixture_dir"
cp "$fixture_dir/radar-enriched.png" tests/goldens/radar-enriched.png
cmp "$fixture_dir/radar-empty.png" tests/goldens/radar-empty.png
cmp "$fixture_dir/radar-traffic.png" tests/goldens/radar-traffic.png
cmp "$fixture_dir/radar-stale.png" tests/goldens/radar-stale.png
```

Expected: all three `cmp` commands exit 0. Inspect the enriched PNG at original
480×480 resolution and verify `737-800` remains compact and distinct.

- [ ] **Step 8: Run renderer and app suites**

```bash
mise exec -- cargo test --locked --test render_radar
mise exec -- cargo test --locked --test app
```

Expected: all tests and four radar goldens pass.

- [ ] **Step 9: Commit**

```bash
git add src/render/text.rs src/render/theme.rs src/render/radar.rs tests/render_radar.rs tests/goldens/radar-enriched.png
git commit -m "feat: render configurable aircraft labels"
```

---

### Task 11: Draw the adaptive radar footer

**Files:**
- Create: `src/render/footer.rs`
- Modify: `src/render/mod.rs`
- Modify: `src/render/theme.rs`
- Modify: `src/render/radar.rs`
- Modify: `src/app.rs`
- Modify: `tests/render_radar.rs`
- Modify: `tests/app.rs`
- Create: `tests/goldens/radar-footer.png`
- Create: `tests/goldens/radar-footer-large-stale.png`

**Interfaces:**
- Consumes: `FooterContent`, settings text scale, environment reading, Unix/monotonic time
- Produces: `FooterBounds` and `draw_footer(...) -> Option<FooterBounds>`
- Produces: wall-clock minute redraw key
- Consumed by: aircraft-tag avoidance in `RadarRenderer`

- [ ] **Step 1: Write failing footer layout tests**

Test pure row layout and returned bounds for:

- no selected items returns `None` and draws no pixels;
- one short semantic group uses one row;
- all five items use two rows in fixed semantic order;
- Zulu `Z` and date remain visible;
- 80% and 130% text stay within the safe round-display chord;
- condition is ellipsized before numeric items;
- selected numeric items are never dropped; and
- bounds never cover the south cardinal label.

Add golden fixtures `fixture_footer()` and `fixture_footer_large_stale()` with
deterministic environment readings and Unix time. In `tests/app.rs`, add a fake
wall clock and assert that visible time/date redraws exactly when its Unix
minute changes, while a disabled footer retains the current generation/stale-
only redraw behavior. With weather selected and no new generation, assert the
display redraws when monotonic age crosses from 44:59 to 45:00.

- [ ] **Step 2: Run the renderer suite and verify RED**

```bash
mise exec -- cargo test --locked --test render_radar footer -- --nocapture
```

Expected: compilation fails because footer layout and theme constants are absent.

- [ ] **Step 3: Add footer theme constants**

Add these exact source values:

```rust
pub const FOOTER_CAP_HEIGHT: f32 = 18.0;
pub const FOOTER_BOTTOM_Y: f32 = 420.0;
pub const FOOTER_PADDING_X: f32 = 12.0;
pub const FOOTER_PADDING_Y: f32 = 8.0;
pub const FOOTER_ROW_GAP: f32 = 4.0;
pub const FOOTER_CORNER_RADIUS: f32 = 12.0;
pub const FOOTER_BORDER_WIDTH: f32 = 1.0;
pub const FOOTER_CHORD_INSET: f32 = 16.0;
pub const FOOTER_BACKGROUND: [u8; 4] = [3, 16, 32, 255];
pub const FOOTER_BORDER: [u8; 4] = GRID;
```

Derive maximum width from the circle chord at the panel's bottom edge:

```rust
let dy = FOOTER_BOTTOM_Y - CENTER.1;
let radius = RIM_RADIUS as f32;
let chord = 2.0 * (radius.powi(2) - dy.powi(2)).max(0.0).sqrt();
let maximum_width = chord - FOOTER_CHORD_INSET;
```

Do not use a fixed full-width rectangle.

- [ ] **Step 4: Implement measured segments and adaptive rows**

Expose:

```rust
pub fn draw_footer(
    pixmap: &mut Pixmap,
    font: &Font,
    settings: &RadarSettings,
    reading: Option<&EnvironmentReading>,
    monotonic_now: Duration,
    unix_seconds: u64,
) -> Option<FooterBounds>;
```

Call `weather::footer_content`, measure each item plus ` · ` separators, and
combine all items into one centered row when it fits. Otherwise first try the
preferred environment/temporal split. If either preferred row is too wide,
enumerate every split point in the fixed ordered sequence `status, condition,
temperature, humidity, time, date` and select the two-row split with the
smallest maximum measured row width that remains within `maximum_width`.
Ellipsize status and condition only after choosing the best split; never remove
or truncate selected temperature, humidity, time, or date.

Build the rounded rail without relying on a nonexistent rounded-rectangle API:
fill a horizontal rectangle, a vertical rectangle, and four corner circles for
the outer border; repeat inset by `FOOTER_BORDER_WIDTH` for the background.
Use `tiny_skia::Rect::from_xywh`, `Pixmap::fill_rect`, and
`PathBuilder::from_circle`. Draw per-item tones with existing
amber/cyan/white palette colors.

- [ ] **Step 5: Add wall-clock rendering and minute invalidation**

Extend the renderer entry point exactly once in this task:

```rust
pub fn render(
    &mut self,
    snapshot: &RadarSnapshot,
    settings: &RadarSettings,
    airports: &[Airport],
    monotonic_now: Duration,
    unix_seconds: u64,
) -> Result<Frame, RenderError>;
```

Add `minute: Option<u64>` and `environment_stale: bool` to `app::RenderKey` and
calculate them with:

```rust
let minute = (self.state() == AppState::Radar
    && (self.snapshot.settings.footer.show_time
        || self.snapshot.settings.footer.show_date))
    .then_some(unix_seconds / 60);

let environment_stale = self.state() == AppState::Radar
    && self.snapshot.settings.footer.needs_environment()
    && weather::environment_is_stale(self.snapshot.environment.as_ref(), monotonic_now);
```

Read `unix_seconds` once per display tick from `AppRuntime`, use it for both the
render key and `RadarRenderer::render`, and continue using monotonic time for
ADS-B/weather ages. Change the private app helpers to
`render_key(&self, monotonic_now: Duration, unix_seconds: u64)` and
`render_frame(&mut self, monotonic_now: Duration, unix_seconds: u64)` so the
same sampled times reach keying and drawing.

- [ ] **Step 6: Draw footer below traffic and avoid it with tags**

In `RadarRenderer::render`, copy the static background, draw the footer, then
draw aircraft and finally the top `DATA STALE` label. Pass `FooterBounds` into
`draw_aircraft_tag`. If the natural tag rectangle intersects the footer, try a
clamped position immediately above the footer, then immediately below it; use
the natural clamped position only if neither alternative fits. Aircraft symbols
and vectors remain unmodified and may cross the footer rather than disappear.

- [ ] **Step 7: Prove compatibility and draw order**

Add pixel tests showing footer-disabled output equals the existing empty/traffic
fixtures, grid pixels remain behind the rail, aircraft pixels can overwrite the
rail, and tag text is shifted away when a valid alternative exists.

- [ ] **Step 8: Generate and inspect only new footer goldens**

```bash
fixture_dir="$(mktemp -d)"
mise exec -- cargo run --locked -- render-fixtures --output "$fixture_dir"
cp "$fixture_dir/radar-footer.png" tests/goldens/radar-footer.png
cp "$fixture_dir/radar-footer-large-stale.png" tests/goldens/radar-footer-large-stale.png
cmp "$fixture_dir/radar-empty.png" tests/goldens/radar-empty.png
cmp "$fixture_dir/radar-traffic.png" tests/goldens/radar-traffic.png
cmp "$fixture_dir/radar-stale.png" tests/goldens/radar-stale.png
```

Expected: existing golden comparisons are exact. Inspect both new files at
480×480: the rail is compact, every selected item is readable, the large stale
case remains inside the round panel, and the `S` label remains clear.

- [ ] **Step 9: Run weather, renderer, and app suites**

```bash
mise exec -- cargo test --locked --test weather
mise exec -- cargo test --locked --test render_radar
mise exec -- cargo test --locked --test app
```

Expected: all tests and six radar goldens pass.

- [ ] **Step 10: Commit**

```bash
git add src/render/footer.rs src/render/mod.rs src/render/theme.rs src/render/radar.rs src/app.rs tests/render_radar.rs tests/app.rs tests/goldens/radar-footer.png tests/goldens/radar-footer-large-stale.png
git commit -m "feat: add adaptive radar footer"
```

---

### Task 12: Document, audit, and verify the integrated feature

**Files:**
- Modify: `README.md:58-68`
- Modify: `docs/install.md:144-157`
- Modify: `docs/architecture.md:72-110, 111-126, 135-150`
- Modify: `tests/docs_contract.rs`
- Review: `docs/superpowers/specs/2026-08-02-optional-radar-data-and-display-design.md`
- Review: all files and goldens changed by Tasks 1-11

**Interfaces:**
- Consumes: completed product behavior
- Produces: public owner guidance, architecture truth, and final verification evidence

- [ ] **Step 1: Write failing documentation-contract tests**

Require README/install text for `Show callsign`, origin/destination, expanded
model, footer items, Celsius/Fahrenheit, radar-local/Zulu, 12/24-hour, text
size, and altitude bounds in feet. Require provider/privacy text containing
`ADSBDB`, `Open-Meteo`, aircraft callsign/identifier, configured coordinates,
and that all new provider features are optional.

Require architecture text naming all three workers, independent failure
isolation, six-hour/ten-minute enrichment caching, 15-minute environment
refresh, version-1 migration, immutable enrichment/environment snapshot fields,
and minute-based clock redraws.

- [ ] **Step 2: Run docs contracts and verify RED**

```bash
mise exec -- cargo test --locked --test docs_contract
```

Expected: the new public and architecture strings are absent.

- [ ] **Step 3: Update owner-facing documentation**

In README and install guidance, keep location/range/runway as the default path,
then describe the expandable Aircraft labels, Footer, and Traffic filter groups.
State that altitude is always feet, blank bounds are open, and unknown-altitude
aircraft are hidden only while a bound is active.

State the exact privacy boundary: routes send callsign, models send aircraft
identifier, both can share a request, weather/radar-local time sends configured
coordinates, Zulu-only time/date sends nothing to Open-Meteo, and every new
provider feature is off by default.

- [ ] **Step 4: Update the architecture diagram and state flow**

Replace the one-worker Mermaid portion with three independent workers, three
settings wake channels, and their separate paths into one immutable snapshot.
Document that service errors do not set `DATA STALE`; weather shows `WX --` or
`WX STALE`, while enrichment silently falls back.

Clarify that `tests/goldens/settings.png` is the physical QR settings screen and
must remain unchanged; the web settings UX is verified by HTML contracts and
viewport inspection rather than that device golden.

- [ ] **Step 5: Run all focused feature suites**

```bash
mise exec -- cargo test --locked --test settings
mise exec -- cargo test --locked --test web
mise exec -- cargo test --locked --test adsb
mise exec -- cargo test --locked --test flight_data
mise exec -- cargo test --locked --test weather
mise exec -- cargo test --locked --test runtime
mise exec -- cargo test --locked --test runtime_flight
mise exec -- cargo test --locked --test runtime_weather
mise exec -- cargo test --locked --test app
mise exec -- cargo test --locked --test render_radar
mise exec -- cargo test --locked --test install
mise exec -- cargo test --locked --test docs_contract
```

Expected: every suite passes without live third-party availability.

- [ ] **Step 6: Inspect every changed visual at original resolution**

Inspect:

```text
tests/goldens/radar-enriched.png
tests/goldens/radar-footer.png
tests/goldens/radar-footer-large-stale.png
```

Confirm the existing `radar-empty.png`, `radar-traffic.png`,
`radar-stale.png`, `settings.png`, and `setup-required.png` hashes did not
change. Re-run the 375×812, 768×1024, and 1440×900 web settings inspection if
later integration commits changed HTML or CSS.

- [ ] **Step 7: Run repository-wide verification**

```bash
mise run verify
git diff --check
git status --short
```

Expected: formatting, clippy with `-D warnings`, all workspace tests, and
`cargo deny` pass; diff check is silent; status contains only intentional files.

- [ ] **Step 8: Audit provider and default boundaries**

Use repository searches:

```bash
rg -n "setInsecure|verify_tls: false|unsafe \{" src tests
rg -n "api\.adsbdb\.com|api\.open-meteo\.com" src tests README.md docs
rg -n "schema_version.*1" src tests/fixtures/settings tests/settings.rs
```

Expected: no insecure TLS or unsafe block; provider URLs exist only in defaults,
tests, and disclosure docs; settings schema 1 appears only in the intentional
legacy fixture/migration path and installer ownership protocol.

- [ ] **Step 9: Commit documentation and final integration corrections**

```bash
git add README.md docs/install.md docs/architecture.md tests/docs_contract.rs
git commit -m "docs: describe optional radar data"
git show --check --stat HEAD
```

Expected: the final commit contains only documentation contracts and public/
architecture prose. If full verification exposed an integration defect, return
to its owning task, make a separate narrowly scoped code commit, rerun that
task's focused tests and Steps 5-8 here, then create this documentation-only
commit.

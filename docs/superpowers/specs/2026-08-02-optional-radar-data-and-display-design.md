# Optional Radar Data and Display Features

Status: approved in conversation

Date: 2026-08-02

Target repository: `shayne/RPi-Plane-Radar`

Reference implementation: `ironicbadger/ESP32-Plane-Radar`

## Feature summary

Add optional aircraft enrichment, environment and time information, radar text
sizing, callsign visibility, and altitude filtering to the existing Plane Radar
settings experience. The work ports the useful ideas from the ESP32 reference
while preserving this project's responsive local settings page, verified HTTPS
behavior, rendering style, and three-second ADS-B refresh cadence.

Every new display feature is opt-in. An existing installation upgraded from
settings schema version 1 must retain its current behavior and rendering until
the owner enables a new option.

## Goals

- Enrich visible aircraft with an optional origin/destination route from
  ADSBDB.
- Replace the terse aircraft type with an optional compact, more specific model
  from ADSBDB.
- Let the owner independently show a weather condition, temperature, humidity,
  time, and date in an adaptive radar footer.
- Support Celsius or Fahrenheit, radar-location or Zulu time, and 12- or
  24-hour time as independent preferences.
- Let the owner show or hide callsigns independently of route display.
- Let the owner scale all radar typography from 80% through 130%.
- Let the owner set optional minimum and maximum aircraft altitudes in feet.
- Isolate optional network services so their latency and failures cannot block
  the primary ADS-B feed.
- Preserve the existing radar pixels and network behavior under compatibility
  defaults.

## Non-goals

- A general plugin, widget, or provider framework.
- User-configurable footer item ordering.
- Persisting ADSBDB or weather caches across process restarts.
- Fetching or parsing airport METAR reports. The weather source remains
  Open-Meteo and only its terminology is rendered in an aviation-oriented
  compact form.
- Displaying cloud bases, visibility, pressure, wind, precipitation totals, or
  any value not requested in this feature set.
- Changing the touch gestures, radar ranges, distance-unit behavior, runway
  data, or core ADS-B provider.
- Adding browser JavaScript or a frontend build pipeline.

## Approved product decisions

- Optional work uses independent background services rather than the existing
  ADS-B polling thread.
- Callsign visibility is independently configurable and defaults on.
- When callsign and route are both enabled, both appear as compact tag lines.
- Footer condition, temperature, humidity, time, and date are independently
  selectable. The footer disappears when none are selected.
- Footer item order is automatic and fixed rather than user-configurable.
- Time zone and clock format are independent settings.
- Altitude bounds use feet regardless of the distance-unit setting. Blank
  inputs are unbounded. Aircraft with unknown altitude are excluded whenever
  either bound is active.
- Manufacturer names are removed from expanded models, but meaningful model
  suffixes are preserved. `737-800` must not be mechanically shortened to
  `737-8` because those names can identify different aircraft generations.
- Weather conditions use METAR-style compact tokens derived from Open-Meteo's
  WMO weather code. They are not represented as an actual METAR observation.
- The visual footer is a small adaptive information rail, not the ESP32
  reference's large opaque trapezoid.

## Settings model and migration

Settings schema version 2 adds the following conceptual fields to
`RadarSettings`:

```text
show_callsign: bool                         default true
show_route: bool                            default false
show_expanded_model: bool                   default false
radar_text_scale_percent: integer           default 100
minimum_altitude_feet: optional integer     default none
maximum_altitude_feet: optional integer     default none

footer.show_condition: bool                 default false
footer.show_temperature: bool               default false
footer.show_humidity: bool                  default false
footer.show_time: bool                       default false
footer.show_date: bool                       default false
footer.temperature_unit: celsius|fahrenheit default celsius
footer.time_zone: radar_local|zulu           default radar_local
footer.clock_format: twelve|twenty_four      default twenty_four
```

Nested settings may be represented by dedicated serializable types so parsing,
validation, form rendering, and renderer dependencies remain explicit.

Version-1 files migrate in memory during load. Their location, distance units,
runway visibility, and range index are preserved, and all new fields receive
the compatibility defaults above. Loading an old file does not perform a
startup write. The next successful settings mutation saves schema version 2
through the existing atomic persistence path. Fresh installations write the
version-2 defaults.

Validation rules are:

- text scale is one of `80`, `90`, `100`, `110`, `120`, or `130`;
- altitude bounds are between -2,000 and 100,000 feet, inclusive;
- when both altitude bounds exist, minimum is less than or equal to maximum;
- all existing location, range, and units validation remains authoritative;
- unknown fields and unsupported future schema versions remain errors.

## Runtime architecture

### Primary ADS-B worker

The current ADS-B worker retains its existing success cadence, failure
backoff, and responsibility for position freshness. Parsing additionally keeps
the normalized Mode S hex identifier and numeric altitude for each aircraft.
The existing display altitude remains available so compatibility defaults do
not silently change its formatting.

Altitude filtering occurs after each response is parsed and before the bounded
aircraft collection is published. With no bounds, current behavior is
unchanged. With either bound, aircraft without numeric altitude are excluded;
known altitudes must satisfy every configured inclusive bound. Ground aircraft
remain governed by the existing ground-traffic rule.

### ADSBDB enrichment worker

The enrichment worker runs only when `show_route` or `show_expanded_model` is
enabled. It reads the latest base-aircraft snapshot, selects the nearest
uncached eligible aircraft, and performs one lookup at a time. It never owns or
delays position publication.

The request strategy keeps the proven ESP32 cadence while minimizing the data
sent for the enabled feature:

- normalize hex identifiers and callsigns to bounded uppercase alphanumeric
  values before including them in a URL;
- use the aircraft endpoint with the hex identifier when only expanded model is
  enabled;
- use the callsign endpoint when only route is enabled;
- prefer the combined
  `https://api.adsbdb.com/v0/aircraft/{hex}?callsign={callsign}` request when
  both fields are enabled and both identifiers exist;
- fall back to the applicable single-field endpoint when the combined request
  cannot be formed;
- retry a missing route through the callsign endpoint when the combined
  response has usable aircraft data but no flight route;
- prefer IATA origin and destination codes, falling back to ICAO codes;
- require both origin and destination codes before publishing a route;
- apply a five-second request timeout and a strict response-size limit;
- space lookups by at least 750 milliseconds;
- wait at least 30 seconds after a network or protocol failure;
- cache successes for six hours and definite misses for ten minutes; and
- keep the in-memory cache bounded and evict the least recently useful entry.

The combined response may populate both route and model caches even when only
one field is currently displayed. Display settings remain authoritative: a
cached value is never rendered merely because it was returned.

Enrichment identity uses hex for aircraft details and normalized callsign for
routes so a callsign change on the same airframe does not retain the previous
flight's route. A setting that hides callsigns affects rendering only; the raw
callsign may still be used for route lookup when route display is enabled. A
model-only lookup does not send the callsign. The settings copy discloses these
provider boundaries.

### Environment and location-time worker

The environment worker calls Open-Meteo when any of these are true:

- condition, temperature, or humidity is enabled; or
- time or date is enabled with the `radar_local` time zone.

Zulu-only time and date require no environment request. When needed, the
worker requests current `temperature_2m`, `relative_humidity_2m`, and
`weather_code` with `timezone=auto`. The response supplies the radar
location's current UTC offset in addition to selected weather values.

The worker fetches immediately after enabling a dependent feature or changing
location, then no more than once every 15 minutes after success. Requests have
a six-second timeout and a strict response-size limit. Failed requests use a
bounded retry backoff and never affect ADS-B freshness.

The runtime snapshot gains an optional environment value containing
temperature in Celsius, relative humidity, WMO weather code, UTC offset,
successful-fetch time, and service status. Unit conversion and presentation
remain pure formatting operations so changing Celsius/Fahrenheit redraws
immediately without a network request.

### Shared model and redraw behavior

Base aircraft, enrichment, and environment values are written through focused
`RuntimeModel` methods. Every accepted visible change bumps the existing
generation counter. A result is rejected when it was requested for a location
or aircraft identity that is no longer current.

Position snapshots remain replaceable without copying network clients or
holding a model lock during I/O. Enrichment is joined to current aircraft by
stable identity at snapshot or render time, so a late lookup cannot restore a
departed aircraft.

The display render key gains a wall-clock minute component whenever time or
date is visible. This keeps the clock advancing even during an ADS-B outage,
while avoiding a full radar redraw every second. Wall-clock and monotonic time
remain injectable for deterministic tests: wall-clock time formats dates and
times, while monotonic time controls cache ages, backoff, and stale markers.

Changing location clears base aircraft and environment data immediately. The
aircraft enrichment cache may remain because its keys are location-independent.
Disabling an optional feature prevents new work immediately. An in-flight
request may finish, but its result stays hidden and must not schedule follow-up
work while the feature is disabled.

## Settings UX

The existing location search and manual-coordinate workflow remain unchanged.
The main `Radar display` area continues to show units, range, and runway
visibility, and adds a radar-text-size control with the six validated values.
The label identifies 100% as the current/default size.

New choices live in native expandable groups beneath the primary controls.
They require no JavaScript. A group renders open when it contains an active
setting or a validation error; otherwise it remains compact but discoverable.

### Aircraft labels

- `Show callsign`, on by default.
- `Show origin and destination`, off by default.
- `Show expanded aircraft model`, off by default.
- Supporting copy explains that route and expanded-model lookups use ADSBDB
  and send the callsign for routes and aircraft identifier for models. Enabling
  both may combine those values into one request.

### Footer

- Independent switches for weather condition, temperature, humidity, time,
  and date.
- Celsius/Fahrenheit appears as the temperature preference.
- Radar location/Zulu applies to both time and date.
- 12-hour/24-hour applies to time.
- Inactive dependent preferences remain stored when their item is hidden.
- The group summary describes the current selection, for example
  `Time, date, temperature`.
- Supporting copy explains that weather and radar-local time use the configured
  radar coordinates with Open-Meteo. Zulu-only time/date need no such request.

### Traffic filter

- Optional numeric `Minimum altitude` and `Maximum altitude` inputs.
- Both labels and supporting copy explicitly say feet.
- Blank means no limit.
- Native bounds help catch common mistakes; the server remains authoritative.
- A minimum-above-maximum submission returns a specific error and reopens this
  group.

Settings submission remains one atomic operation using the existing
POST-redirect-GET success flow and local security controls. Errors preserve the
saved configuration and present a concise message without disclosing internal
paths or provider payloads.

## Aircraft tag presentation

The renderer assembles tag lines in this order:

1. callsign, when enabled and non-empty;
2. route, when enabled and known;
3. detailed model when enabled and known, otherwise the existing short type;
4. altitude.

A fully enriched tag can therefore appear as:

```text
DAL123
JFK→LAX
737-800
12000 ft
```

If route is enabled while callsign is hidden, the route becomes the first line.
If enrichment has no route, no empty line or placeholder is inserted. If
enrichment has no detailed model, the original type designator remains.

Detailed model normalization removes known manufacturer prefixes such as
`Boeing`, `Airbus`, `Embraer`, `Bombardier`, and `De Havilland Canada`, trims
and collapses whitespace, and retains the remaining subtype. Examples include
`737-800`, `737 MAX 8`, and `A320-214`. The renderer measures the result and
uses a compact known prefix or ellipsis only when required to keep the tag
inside the display. It must not invent or silently collapse distinct model
variants.

The current far-first symbol and label ordering remains. Tag height and width
are calculated from the number of actual lines and configured text scale.
Labels continue to choose the side facing the radar center, clamp to the screen,
and additionally shift away from the footer bounds when a non-overlapping
position is available.

## Footer presentation

The footer is a measured, floating information rail inside the lower radar
area. It uses a rounded deep navy-black surface, a restrained radar-green edge,
and colors already present in the radar palette. Its bounds are derived from
the selected content, text metrics, padding, and the safe chord width of the
round display rather than a fixed full-width trapezoid.

Items use fixed semantic ordering:

1. condition;
2. temperature;
3. humidity;
4. time;
5. date.

Environmental items prefer the first row and temporal items prefer the second:

```text
SCT · 72°F · RH54%
14:35Z · 02 AUG
```

When all selected values fit comfortably on one row, the footer collapses to
one centered row. When only one semantic group is present, that group uses one
row. Condition text is compact before numeric fields are truncated. Selected
numeric items are never silently dropped. At most two rows are used.

Zulu time receives the `Z` suffix. Radar-local time has no suffix. Twelve-hour
time uses an unambiguous `AM` or `PM`; 24-hour time uses two-digit hours.
Dates use `DD MON` with an uppercase three-letter month.

The footer is drawn after the static grid but before aircraft symbols and tags,
making live traffic visually primary. Tag placement treats the footer as an
avoidance rectangle, although an aircraft symbol or vector may cross it rather
than being hidden.

With no footer items selected, no footer geometry, text, or network dependency
is introduced and the compatibility frame remains pixel-identical.

## METAR-style weather mapping

The weather label is a presentation mapping from Open-Meteo's WMO weather code,
not a claim that the application fetched an airport observation. It does not
append a cloud base because the source does not provide layer heights in this
request.

The formatter uses standard compact aviation tokens where the source supports
the distinction:

```text
0           CLR
1           FEW
2           SCT
3           OVC
45, 48      FG
51, 53, 55  -DZ, DZ, +DZ
56, 57      -FZDZ, +FZDZ
61, 63, 65  -RA, RA, +RA
66, 67      -FZRA, +FZRA
71, 73, 75  -SN, SN, +SN
77          SG
80, 81, 82  -SHRA, SHRA, +SHRA
85, 86      -SHSN, +SHSN
95          TS
96, 99      TSGR
other       WX
```

This mapping follows Open-Meteo's documented WMO-code severity groupings and
uses METAR-style contractions for compactness. It does not infer `BKN`, cloud
altitude, visibility, or precipitation details absent from the response.

## Radar text sizing

The selected percentage scales typography only:

- cardinal directions;
- range label;
- airport/runway labels;
- aircraft tag lines;
- `DATA STALE`;
- footer values and stale markers.

It does not scale grid geometry, runway strokes, aircraft symbols, vectors, rim
dots, or touch targets. The static-background cache key includes text scale
because several background labels depend on it. All fitting uses measured text
rather than fixed character counts. Existing cap heights at 100% remain the
source values, ensuring the default rendering does not drift.

## Failure and stale-data behavior

ADSBDB failures do not add radar warnings. The renderer falls back to the raw
callsign and short type already available from the primary feed. Definite
misses create no placeholder lines. Enrichment service failures are logged with
rate limiting and never modify the primary `DATA STALE` state.

Before the first successful environment response, selected weather fields
collapse to `WX --`. Zulu time and date remain available. Radar-local time and
date use placeholders until the correct location offset is known rather than
falling back to the Pi host's timezone.

The last successful environment data and offset remain usable during temporary
failures. Once their monotonic age exceeds 45 minutes, the environment row adds
`WX STALE` while retaining the selected last-known values. This service status
is separate from `DATA STALE`, which remains reserved for aircraft positions.

Malformed JSON, missing required fields, invalid numeric values, oversized
bodies, unexpected HTTP statuses, and transport failures do not replace a
previous valid value. Logs include service and failure class without emitting
full response bodies or sensitive local form data.

## Security, privacy, and provider boundaries

- Every external request uses the Rust client's normal certificate and
  hostname verification. The ESP32 reference's insecure TLS mode is not
  carried over.
- ADSBDB receives normalized callsigns for routes and aircraft hex identifiers
  for models only while the corresponding enrichment is enabled. When both are
  enabled, the values may share one combined request.
- Open-Meteo receives configured radar coordinates only while selected weather
  or radar-local time/date requires them.
- Zulu-only time/date and all compatibility defaults create no new provider
  calls.
- No API key, provider credential, user account, browser geolocation, or remote
  browser asset is introduced.
- Service base URLs and client traits are injectable through runtime
  configuration for deterministic local testing.
- Existing session, CSRF, host, origin/referer, body-size, worker-count,
  escaping, and no-store protections remain in force.

Provider references:

- ADSBDB API and rate-limit documentation: <https://www.adsbdb.com/>
- Open-Meteo Forecast API: <https://open-meteo.com/en/docs>
- Aviation Weather METAR terminology:
  <https://aviationweather.gov/gfa/help/?page=tutorial>

## Verification and acceptance

The compatibility gate is strict: loading version-1 settings with all new
fields at their defaults must produce the current radar pixels and must not
call ADSBDB or Open-Meteo.

Automated coverage includes:

- version-1 migration, version-2 persistence, defaults, unknown fields, and all
  new validation rules;
- settings form rendering, round trips, missing-checkbox behavior, errors,
  group-open state, semantic labels and fieldsets, hostile inputs, and privacy
  disclosures;
- altitude parsing and inclusive filtering at minimum, maximum, negative, and
  unknown-altitude boundaries;
- ADSBDB URL construction, identifier normalization, combined-response parsing,
  callsign fallback, IATA/ICAO preference, model normalization, nearest-first
  scheduling, positive and negative cache expiry, eviction, timeout, and
  backoff;
- Open-Meteo parsing, every supported WMO mapping, malformed data, unit
  conversion, 12/24-hour formatting, radar-local/Zulu selection, date rollover,
  initial placeholders, and 45-minute staleness;
- runtime concurrency proving blocked optional clients do not block primary
  ADS-B publication and late results cannot update a changed location or
  departed aircraft;
- render goldens covering route plus callsign, hidden callsign, compact model,
  one-row footer, two-row footer, stale environment data, and 80%/130% text;
- preservation of the existing default radar golden; and
- updated installer fixtures, settings-page golden, README feature/settings
  documentation, and provider/privacy copy.

Tests use fixture JSON and fake wall and monotonic clocks. The normal test suite
does not require live provider availability. Before completion, run the full
Rust test suite and the repository's standard formatting and lint checks,
regenerate intentional goldens, and inspect every changed radar and settings
golden at its original resolution.

## Open questions

None. The user approved the independent-service architecture, compatibility
defaults, settings information architecture, fixed footer ordering, altitude
semantics, compact aircraft terminology, METAR-style weather labels, adaptive
footer presentation, service degradation behavior, and verification scope.

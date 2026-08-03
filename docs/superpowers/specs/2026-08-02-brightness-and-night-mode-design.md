# Brightness and Scheduled Red Night Mode

**Status:** Approved design

**Date:** 2026-08-02

**Repositories:** `shayne/RPi-Plane-Radar`, `shayne/hyperpixel2r-kms`

## Goal

Add real HyperPixel backlight brightness control and an optional scheduled
night mode. The owner chooses a daytime brightness, a lower night brightness,
a radar-local start time, and whether night mode renders the entire physical
display in shades of red. Night mode ends automatically at sunrise for the
configured radar location.

The default behavior remains the current product behavior: 100% brightness,
full color, and no automatic night schedule.

## Product decisions

- Day and night brightness use native sliders from 5% through 100% in 5%
  increments. Zero is not accepted, so a saved setting cannot make the device
  appear dead.
- Night mode is disabled by default.
- The default night brightness is 30%.
- The default night start is 20:00.
- The configured start is local wall-clock time at the saved radar location,
  not the browser, Raspberry Pi, or UTC clock zone.
- Night mode ends at the next sunrise for that radar location. There is no
  configurable end time or sunset-derived start in this scope.
- Red-only rendering is independently optional and disabled by default.
- When enabled, red-only rendering applies to every physical Plane Radar
  state: radar, waiting for network, setup QR, and the on-device settings
  notice. It does not recolor the browser settings page.
- The existing script-free web contract remains in force.

## Scope

### Included

- a standard Linux PWM backlight device for the supported HyperPixel 2.1
  Round;
- unprivileged Plane Radar access to that brightness device;
- persistent day and night brightness settings;
- a fixed radar-local night start and location-derived sunrise end;
- a cached 16-day solar schedule from Open-Meteo;
- daylight-saving-safe local-time evaluation;
- a whole-frame red-only transform;
- settings status for active, upcoming, unavailable, and fallback schedules;
- host, release-contract, and physical-device verification in both
  repositories.

### Excluded

- displays other than the supported HyperPixel 2.1 Round;
- ambient-light sensors or automatic brightness based on room light;
- a sunset-derived start, configurable end time, or multiple daily schedules;
- 0% brightness or scheduled screen-off behavior;
- red styling for the web settings page;
- arbitrary RGB palette editors;
- JavaScript-based slider previews;
- controlling boot firmware, console, or unrelated desktop sessions.

## Cross-repository architecture

The kernel driver owns PWM, GPIO pinmux, panel power ordering, and the standard
backlight device. Plane Radar owns user percentages, solar data, schedule
policy, transitions, and framebuffer color transformation. The application
must never bit-bang GPIO19 or configure PWM directly.

```text
Open-Meteo daily sunrise + timezone
                 |
                 v
        SolarWorker -> atomic solar cache
                 |              |
                 +------> NightSchedule <------ settings + wall clock
                                  |
                         DisplayPolicy
                         /           \
                BacklightTarget     FrameColorMode
                      |                    |
             /sys/class/backlight     final RGBA frame
                      |                    |
                 PWM GPIO19             SDL/KMS
```

## HyperPixel driver design

### Standard PWM backlight

The custom device-tree overlay will follow the Raspberry Pi 6.18
`vc4-kms-dpi-hyperpixel2r` PWM path:

- a `pwm-backlight` node uses PWM channel 1 on GPIO19;
- the PWM period is 200,000 ns;
- GPIO19 uses Alt5;
- the PWM clock is assigned 1 MHz;
- brightness levels interpolate across 0 through 255;
- the custom panel node references the backlight through a `backlight`
  phandle.

The node name is stable so the class device is available as:

```text
/sys/class/backlight/planeradar-backlight
```

The driver's boot default is the application minimum, 5%. This prevents a
high-brightness flash before user space starts. Plane Radar applies the saved
day or night target as soon as it resolves startup policy; the steady-state
default remains 100% daytime brightness.

### DRM panel lifecycle

`hyperpixel2r_kms` will call `drm_panel_of_backlight()` during probe and let
the DRM panel helpers enable the backlight after panel enable and disable it
before panel disable. The driver will remove its direct backlight GPIO
descriptor, manual high/low writes, and backlight-specific GPIO quiesce hook.
Panel preparation, ST7701 commands, touch registration, and safe shared-bus
handling remain unchanged.

Probe deferral or failure to acquire the referenced backlight fails panel
probe safely. Module removal and error unwind leave the backlight disabled.

### Unprivileged access

Plane Radar continues to run as the dedicated `planeradar` user with the
existing supplementary `video` group. The driver package installs a narrowly
scoped udev permission rule for the `planeradar-backlight` brightness
attribute. It does not make unrelated backlights writable and does not grant
Plane Radar root, `CAP_DAC_OVERRIDE`, or GPIO access.

The driver verification contract checks both the class-device identity and
the exact permission rule. Installation and removal remain part of the
driver's staged lifecycle so an uninstall does not leave an orphaned rule.
Installation reloads the udev rules, triggers only the named backlight device,
and verifies a process in the `video` group can write `brightness` before the
candidate driver is accepted.

## Settings model and migration

Settings schema version 3 adds a nested brightness object:

```json
{
  "brightness": {
    "day_percent": 100,
    "night": {
      "enabled": false,
      "brightness_percent": 30,
      "start_hour": 20,
      "start_minute": 0,
      "red_mode": false
    }
  }
}
```

The model uses focused value types equivalent to:

```rust
pub struct BrightnessSettings {
    pub day_percent: u8,
    pub night: NightModeSettings,
}

pub struct NightModeSettings {
    pub enabled: bool,
    pub brightness_percent: u8,
    pub start_hour: u8,
    pub start_minute: u8,
    pub red_mode: bool,
}
```

Validation requires:

- day and night percentages from 5 through 100 inclusive;
- percentages divisible by 5;
- hours from 0 through 23; and
- minutes from 0 through 59.

Schemas 1 and 2 migrate in memory to schema 3 with the defaults above.
Loading an older schema does not rewrite the settings file during startup.
The next successful settings mutation persists schema 3. Unknown fields,
invalid values, and unsupported future schema versions remain errors.

## Settings UX

The responsive settings navigation gains a `Brightness` destination. The
section follows the approved single-content-flow layout and contains:

1. `Day brightness`, a native range input with 5% steps;
2. `Night mode`, a switch;
3. `Night brightness`, another native range input with 5% steps;
4. `Starts at`, a native `time` input with one-minute precision;
5. `Red-only display`, a switch; and
6. a concise schedule status.

The night controls stay editable when the schedule is disabled. This lets an
owner configure the complete mode before enabling it and avoids client-side
state scripting. Each slider has explicit 5%, 50%, and 100% references, a
server-rendered saved value, an associated label, and accessible minimum,
maximum, and step semantics.

Examples of status copy are:

- `Night mode starts at 8:00 PM · Sunrise 6:04 AM`
- `Night mode active · Ends at sunrise 6:04 AM`
- `Sunrise unavailable · Using 7:00 AM`
- `Waiting for sunrise data`
- `Brightness control unavailable · Upgrade the display driver`

The native time input persists a 24-hour hour/minute pair. Human-readable
status follows the owner's existing 12/24-hour clock preference. Applying the
form reevaluates display policy immediately. During active night mode,
changing the day slider updates the next daytime target without overriding
the active night brightness.

## Solar data

### Provider request

A dedicated `SolarClient` uses the existing bounded HTTPS client and
Open-Meteo forecast endpoint. When night mode is enabled and a location is
configured, it requests only the data needed for scheduling:

```text
latitude=<saved latitude>
longitude=<saved longitude>
daily=sunrise
timezone=auto
timeformat=unixtime
past_days=1
forecast_days=16
```

The response must contain a bounded IANA timezone identifier and matching
daily date and sunrise arrays. Each sunrise is either a finite integer Unix
timestamp or `null`; `null` is valid for a date on which the provider reports
no sunrise and drives the documented 07:00 fallback. Other value types,
mismatched arrays, duplicate dates, and out-of-range timestamps are schema
errors. Provider status, JSON, schema, timeout, TLS, and body-limit failures
use sanitized log categories and never log coordinates or response bodies.

This worker is independent of weather-footer selection. Night mode must fetch
solar data even when condition, temperature, humidity, radar-local time, and
date are all hidden. Conversely, solar-only use does not request current
weather variables.

### Timezone rules

The response's IANA timezone identifier is authoritative for the saved radar
coordinates. Plane Radar uses Jiff with the operating system's
`/usr/share/zoneinfo` database to translate the configured local start into
an instant for each cached date. This preserves daylight-saving and historical
timezone rules without bundling a second timezone database into the binary.

For a nonexistent spring-forward start, policy uses the first valid local
minute after the gap. For an ambiguous fall-back start, policy uses the first
occurrence.

### Atomic cache

Successful solar responses persist atomically to:

```text
/var/lib/planeradar/solar-schedule.json
```

The cache records:

- a cache schema version;
- the exact latitude and longitude used for the request;
- the IANA timezone identifier;
- fetched-at Unix time; and
- the validated sunrise instants.

The file is created beneath the existing protected state directory with the
same atomic write, flush, persist, and parent-directory sync discipline as
settings. A cache is usable only when its coordinates exactly match the
current saved coordinates. A label-only location change does not invalidate
it. Corrupt, unsupported, mismatched, or unsafe cache files are ignored and
replaced only after a successful response.

The worker loads a valid cache before its first network request. It refreshes
after a location change, when enabled without a usable cache, and once per
successful day. Failures retry after 30 seconds, 60 seconds, 5 minutes, and
then 15 minutes. All waits remain interruptible by settings changes and
shutdown.

## Night schedule policy

The schedule evaluator is a pure function of settings, the matching solar
schedule, and Unix time. It produces:

```rust
pub struct DisplayPolicy {
    pub period: DisplayPeriod,
    pub brightness_percent: u8,
    pub color_mode: FrameColorMode,
    pub next_transition: Option<Transition>,
    pub solar_status: SolarStatus,
}
```

Each night interval begins at the configured local time and ends at the first
sunrise after that start. At the exact start minute the period becomes night;
at the exact sunrise instant it becomes day. The evaluator considers the
previous local date so a restart after midnight correctly finds the prior
evening's interval.

Policy is reevaluated at least once per wall-clock minute and whenever
settings, location, or solar data changes. NTP adjustments and forward or
backward wall-clock jumps therefore converge on the correct current period;
policy does not derive civil time from monotonic elapsed time.

If a matching cache has no sunrise after a nightly start or has passed its
forecast coverage, that interval ends at 07:00 radar-local time and reports
fallback status. This prevents indefinite dim or red output during a long
outage or at locations with missing sunrise data.

A newly configured location without matching solar data stays at day
brightness and full color until the first valid response. A device without a
saved location also stays in day mode. Policy never borrows another
location's timezone or sunrise.

## Backlight control and transitions

Application hardware access is behind a small `Backlight` interface with a
sysfs implementation and a no-op implementation for unsupported hosts,
fixtures, and capture tests. Percentages map to the nearest 0-through-255
level using integer rounding:

```text
level = round(percent × max_brightness / 100)
```

The implementation reads `max_brightness` rather than assuming 255, validates
the standard class-device identity, and writes only when the effective level
changes.

Steady-state target changes ramp linearly over two seconds. Entering night
mode first ramps down while retaining full color, then applies red at the dim
target. Leaving night mode restores full color at the dim level, then ramps
up. Startup is special: with a cached active night, the first application
frame is red at the driver's 5% boot level before brightness rises to the
saved night target. This avoids an initial full-color flash.

Failure to discover, read, or write the backlight is non-fatal. Plane Radar
logs a rate-limited sanitized warning, publishes unavailable status to the web
settings model, and continues rendering. If current policy calls for red,
red-only output still applies even when hardware brightness control fails.

## Red-only framebuffer transform

Red-only mode is a final-frame operation after the existing setup or radar
renderer completes and before the frame is uploaded or retained as the debug
capture. It does not fork the theme constants or duplicate renderer logic.

For each nontransparent RGBA pixel, compute integer Rec. 709-style luma and
place it in the red channel:

```text
luma = (54 × red + 183 × green + 19 × blue + 128) / 256
output = (luma, 0, 0, alpha)
```

The weights sum to 256, so black remains black and white reaches full red.
Alpha is preserved. Existing differences in grid, aircraft, labels, runway,
status, footer, and QR brightness remain ordered while green and blue output
are eliminated.

The application render key includes the current frame color mode whenever
night mode is enabled. A color-mode transition therefore forces one complete
rerender even when traffic, footer, and other runtime data are unchanged.
Debug PNGs represent the actual transformed physical frame.

## Runtime data flow

The runtime snapshot gains only the state consumers need:

- the current validated solar schedule or absence;
- the latest sanitized solar failure time/category;
- backlight availability; and
- the resolved display policy or enough immutable inputs to derive it.

The solar worker owns provider cadence and cache persistence. The display loop
owns short brightness ramps because it already receives frequent monotonic
ticks and can avoid a second hardware-writing thread. The web handler reads
snapshot status but never writes the backlight or calls Open-Meteo.

Settings and location changes wake the solar worker and invalidate policy in
the same generation update used by the existing runtime model. Shutdown stops
the worker before shared runtime state is dropped. The DRM lifecycle, not the
application, turns the backlight fully off during panel disable or module
removal.

## Failure handling

- A provider outage continues using matching cached coverage.
- Missing future coverage uses the explicit 07:00 radar-local fallback.
- A new location with no cache uses day mode until a valid response arrives.
- Invalid or hostile provider data is rejected without replacing a good
  cache.
- A corrupt cache is ignored without preventing application startup.
- A missing IANA zoneinfo entry makes the solar response unusable and retains
  the prior valid cache when coordinates match.
- A missing or permission-denied backlight leaves rendering alive, reports a
  settings warning, and continues applying red mode.
- A settings validation failure preserves the last accepted settings and
  returns the existing generic form error behavior.
- A red transform error is impossible for a validated RGBA frame length; frame
  construction remains the authority for dimension validation.

## Security and privacy

- Coordinates go only to the already approved Open-Meteo forecast host over
  verified HTTPS when night mode or existing radar-local weather/time features
  require it.
- Solar logs contain provider and sanitized error category only.
- Provider response bodies, coordinates, IANA strings from invalid payloads,
  and cache contents are not logged.
- The solar cache is state-directory data and is not served by HTTP.
- Backlight access is limited to one named class device and the existing
  `video` group.
- Plane Radar remains unprivileged with `NoNewPrivileges=true` and receives no
  GPIO or broad filesystem capability.

## Verification

### HyperPixel driver repository

Host and artifact tests cover:

- the PWM node, channel, period, interpolation, clock, GPIO19 Alt5 pinmux, and
  panel phandle;
- removal of direct backlight GPIO ownership and GPIO quiesce calls;
- `drm_panel_of_backlight()` probe and deferral behavior;
- safe panel disable, error unwind, and module removal;
- exact udev rule identity and uninstall cleanup;
- source, overlay, applied-DTB, module, and release-manifest provenance; and
- compilation against the supported Raspberry Pi 6.18 kernel contract.

### Plane Radar repository

Unit and integration tests cover:

- schema-1 and schema-2 migration to schema 3 without startup persistence;
- defaults and strict validation for percentages, five-point steps, hours,
  minutes, nested unknown fields, and future schemas;
- exact web form controls, parsing, escaping, checkbox omission, accessible
  labels, schedule statuses, driver warnings, and no `<script>` output;
- bounded solar request construction and sanitized failures;
- provider parsing, timezone validation, coordinate matching, atomic cache
  replacement, corrupt caches, and preserved good caches;
- worker wakeups, retry cadence, daily refresh, location changes, settings
  disable, and prompt shutdown;
- night intervals before and after midnight, exact boundaries, DST gaps and
  overlaps, NTP jumps, expired coverage, missing sunrise, and 07:00 fallback;
- percentage mapping against multiple advertised maximum brightness values;
- idempotent writes, two-second ramps, startup active-night behavior, and
  backlight read/write failures;
- red transform vectors for black, white, each existing theme color, alpha,
  and invalid frame construction boundaries;
- fixtures for radar, waiting, setup, and on-device settings in red mode; and
- strict formatting, all-target/all-feature clippy, dependency policy,
  nextest, fixture inventory, and release portability.

### Physical acceptance

Before any public release, stage the brightness-capable driver through the
existing tryboot workflow on `user@radar.local`. Verify:

1. the expected `planeradar-backlight` device and unprivileged write access;
2. stable, visibly distinct 5%, 30%, and 100% levels without flicker;
3. touch, KMS rendering, service health, reboot, unload, rollback, and driver
   provenance;
4. an accelerated fixed-time night transition and sunrise transition;
5. red-only radar, waiting, setup QR, and on-device settings frames;
6. no high-brightness or full-color flash during application restart at
   night; and
7. restoration of 100% full-color daytime defaults when night mode is
   disabled.

Leave the local prerelease installed for owner assessment. Do not tag, push,
or cut a release until physical brightness, red appearance, and rollback have
been accepted.

## Coordination and release boundary

Implementation uses separate GitButler branches in both repositories. The
Plane Radar branch stacks on the completed optional-settings/navigation work
and must not absorb the independent route-confidence work or its uncommitted
spec edits. The driver branch starts from current `hyperpixel2r-kms` main.

The Plane Radar release manifest records the exact brightness-capable driver
source revision and artifact identity. Application installation must reject a
driver artifact that lacks the required PWM/backlight contract. Existing
staged activation, identity-bound reconnect, health checks, and rollback rules
remain authoritative.

No release publication is part of implementation. Release version selection,
tagging, pushing, and public publication require a later explicit release
request after physical acceptance.

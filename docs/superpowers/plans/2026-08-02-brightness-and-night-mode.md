# Brightness and Scheduled Red Night Mode Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add real 5–100% HyperPixel backlight control, an optional radar-local night schedule that ends at sunrise, and an optional all-screen red-only display mode while preserving today's 100% full-color behavior by default.

**Architecture:** Extend the HyperPixel KMS overlay with a standard `pwm-backlight` device and a narrowly scoped udev rule. In Plane Radar, migrate settings to schema 3, fetch and atomically cache sunrise/timezone data in an independent worker, resolve a pure `DisplayPolicy`, let the display loop own two-second sysfs brightness ramps, and transform the final RGBA frame immediately before upload and debug capture.

**Tech Stack:** Raspberry Pi Linux 6.18 DRM panel and PWM backlight APIs, device tree overlays, udev, POSIX shell lifecycle scripts, Rust 2024, serde/serde_json, ureq with rustls/WebPKI verification, Jiff 0.2 with system zoneinfo, tiny-skia/fontdue rendering, tiny_http server-rendered HTML/CSS, cargo-nextest, PNG golden tests, and GitButler virtual branches.

## Global Constraints

- Work in the existing GitButler workspace. Do not create Git worktrees and do not use raw Git commits.
- Keep Plane Radar work on `codex/brightness-night-mode`, stacked on `codex/optional-radar-features`. Do not absorb `codex/route-confidence` changes.
- Create `codex/brightness-night-mode-driver` from current `hyperpixel2r-kms` main. Do not mix unrelated driver changes into it.
- Today's behavior remains the compatibility default: 100% day brightness, night mode off, red-only off, and no solar request.
- Day and night percentages are integers from 5 through 100 inclusive and divisible by 5. Zero is not a valid persisted setting.
- Night starts at the configured location's local `HH:MM` and ends at the first subsequent sunrise; there is no configurable end time or sunset start.
- Missing future sunrise coverage falls back to 07:00 in the radar location's zone. A new or missing location without matching solar data remains in day/full-color mode.
- Red-only mode applies to every physical app state and debug PNG, but never to the browser settings page.
- The settings site remains dependency-free, server-rendered, accessible, responsive, and free of JavaScript or remote assets.
- Solar traffic uses verified HTTPS, bounded timeouts and bodies, and sanitized logs that contain neither coordinates nor response bodies.
- Plane Radar stays unprivileged. Do not add root execution, capabilities, direct GPIO access, or broad sysfs permissions.
- The display driver boots at 5%, owns GPIO19/PWM/panel ordering, and exposes only `/sys/class/backlight/planeradar-backlight` to the application.
- Brightness changes ramp for two seconds; entering night dims before red, leaving night restores full color before brightening, and active-night startup never flashes full color.
- Rust remains `#![forbid(unsafe_code)]`, compatible with Rust 1.97.1, and passes the existing dependency policy.
- No release tag, push, or public publication is authorized. Physical testing ends with a local prerelease installed on `user@radar.local` for owner acceptance.

---

## Scope and file structure

The work spans two repositories because hardware ownership belongs in the
driver while policy and UX belong in Plane Radar. Each task ends in a focused
GitButler commit and leaves the other active branches untouched.

### HyperPixel driver files

| Path | Responsibility after this plan |
|---|---|
| `overlays/hyperpixel2r-kms-overlay.dts` | Define the named PWM backlight, GPIO19 Alt5 pinctrl, 1 MHz PWM clock, 200,000 ns period, levels, boot default, and panel phandle |
| `kernel/hyperpixel2r_kms_main.c` | Acquire the phandle with `drm_panel_of_backlight()` and retain safe DRM lifecycle/error unwind without direct backlight GPIO writes |
| `kernel/hyperpixel2r_kms_gpio.[ch]` | Quiesce only the shared display command bus; no backlight callback |
| `packaging/70-planeradar-backlight.rules` | Grant the existing `video` group write access to only the named brightness attribute |
| `scripts/common.sh`, `scripts/build-driver.sh`, `scripts/check-artifacts.sh` | Validate and package the overlay, module, applied DTB, udev rule, digests, and `pwm-backlight-v1` capability |
| `scripts/stage-tryboot.sh`, `scripts/lifecycle-remote.sh`, `scripts/accepted-lifecycle.sh`, `scripts/uninstall.sh` | Transactionally stage, verify, accept, roll back, and remove the rule with the driver |
| `scripts/package-release.sh`, `release/driver-manifest.schema.json` | Publish schema-2 manifests declaring the required capability when a later release is authorized |
| `tests/gpio_test.c`, `tests/backlight-contract.sh`, `tests/build-contract.sh`, `tests/boot-scripts.sh`, `tests/boot-fixtures.sh`, `tests/release-contract.sh` | Prove compiled DT/module artifacts, behavioral GPIO helper handling, exact artifacts, permissions, lifecycle, rollback, and release metadata |
| `mise.toml` | Register the focused backlight contract in the complete verification task |

### Plane Radar new files

| Path | Responsibility |
|---|---|
| `src/solar.rs` | Open-Meteo request/response validation, timezone validation, schedule/cache types, and atomic cache persistence |
| `src/night_mode.rs` | Pure DST-aware `DisplayPolicy` evaluation and human schedule facts |
| `src/runtime/solar_worker.rs` | Independent cache-first solar refresh, retry, wake, and shutdown cadence |
| `src/backlight.rs` | Narrow backlight trait, sysfs/no-op implementations, percentage mapping, two-second ramp, and availability |
| `tests/solar.rs` | Request, response, cache, coordinate, zoneinfo, and sanitized-error contracts |
| `tests/night_mode.rs` | Day/night intervals, boundaries, DST, fallback, and time-jump policy tests |
| `tests/runtime_solar.rs` | Worker cache-first behavior, refresh/retry cadence, settings wakeups, and shutdown tests |
| `tests/backlight.rs` | Device identity, integer mapping, idempotence, ramps, ordering, and failure tests |
| `tests/fixtures/open_meteo/solar.json` | Deterministic 17-day sunrise/timezone response with one nullable sunrise |
| `tests/fixtures/settings/v2.json` | Exact schema-2 migration input |
| `tests/goldens/setup-red.png`, `tests/goldens/waiting-red.png`, `tests/goldens/settings-red.png`, `tests/goldens/radar-red.png` | Actual transformed physical-frame fixtures |

### Plane Radar existing files with focused changes

| Path | Responsibility after this plan |
|---|---|
| `Cargo.toml`, `Cargo.lock` | Add Jiff 0.2 while continuing to use OS `/usr/share/zoneinfo` |
| `src/model.rs` | Own schema-3 brightness types plus immutable solar and backlight policy inputs in `RuntimeSnapshot` |
| `src/settings.rs`, `src/install.rs` | Migrate v1/v2 in memory, validate v3 strictly, persist v3 on mutation, and seed v3 defaults |
| `src/runtime.rs`, `src/main.rs`, `src/lib.rs` | Start/stop/wake the solar worker, publish status, pass the backlight controller, and register modules |
| `src/app.rs`, `src/display.rs` | Reevaluate wall-clock policy, own monotonic brightness ramps, force color-mode rerenders, and transform before upload/capture |
| `src/render/mod.rs` | Apply the final deterministic red-only transform while preserving alpha |
| `src/web.rs` | Add Brightness navigation, native controls, strict parsing, and status from immutable runtime state |
| `driver.lock.toml`, `release/release-manifest.schema.json`, `crates/planeradarctl/src/*` | Bind the local app candidate to the exact brightness-driver revision and require `pwm-backlight-v1` without bypassing staged activation or rollback |
| `tests/settings.rs`, `tests/install.rs`, `tests/runtime.rs`, `tests/app.rs`, `tests/web.rs`, `tests/render_*.rs`, `tests/release_contract.rs` | Extend boundaries, integration behavior, fixtures, and release portability |
| `README.md`, `docs/install.md`, `docs/architecture.md`, `tests/docs_contract.rs` | Document UX, provider/privacy, sysfs ownership, fallback, operation, and physical acceptance; update an existing non-prose documentation contract only when one actually requires it |

### Cross-task interfaces

Use these names and shapes consistently:

```rust
pub const SETTINGS_SCHEMA_VERSION: u32 = 3;

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BrightnessSettings {
    pub day_percent: u8,
    pub night: NightModeSettings,
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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FrameColorMode { FullColor, RedOnly }

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DisplayPeriod { Day, Night }

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DisplayPolicy {
    pub period: DisplayPeriod,
    pub brightness_percent: u8,
    pub color_mode: FrameColorMode,
    pub next_transition: Option<Transition>,
    pub solar_status: SolarStatus,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SolarSchedule {
    pub schema_version: u32,
    pub latitude: f64,
    pub longitude: f64,
    pub time_zone: String,
    pub fetched_at_unix: u64,
    pub days: Vec<SolarDay>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SolarDay {
    pub date: String,
    pub sunrise_unix: Option<i64>,
}

pub trait Backlight: Send {
    fn availability(&self) -> BacklightAvailability;
    fn current_level(&mut self) -> Result<u32, BacklightError>;
    fn max_level(&self) -> u32;
    fn write_level(&mut self, level: u32) -> Result<(), BacklightError>;
}
```

`SolarSchedule` persists strings and Unix timestamps only; `src/solar.rs`
validates dates, timestamps, coordinate finiteness/ranges, array uniqueness,
bounded IANA identifiers, and zoneinfo availability before constructing it.
`src/night_mode.rs` is the only module that converts those values to Jiff civil
and zoned types.

---

### Task 1: Move GPIO19 ownership to a standard PWM backlight

**Repository:** `/Users/shayne/code/hyperpixel2r-kms`

**Files:**
- Create: `tests/backlight-contract.sh`
- Modify: `mise.toml`
- Modify: `overlays/hyperpixel2r-kms-overlay.dts`
- Modify: `kernel/hyperpixel2r_kms_main.c`
- Modify: `kernel/hyperpixel2r_kms_gpio.c`
- Modify: `kernel/hyperpixel2r_kms_gpio.h`
- Modify: `tests/gpio_test.c`
- Modify: `scripts/common.sh`
- Modify: `scripts/check-artifacts.sh`
- Modify: `tests/release-contract.sh`

**Interfaces:**
- Consumes: Raspberry Pi `pwm1`/GPIO19 pinctrl and DRM panel backlight helpers
- Produces: `/sys/class/backlight/planeradar-backlight` with max 255 and boot level 13
- Produces: compiled overlay/module verification and behavioral GPIO contract tests used by artifact verification

- [x] **Step 1: Create the driver GitButler branch**

Run:

```bash
cd /Users/shayne/code/hyperpixel2r-kms
but setup
but branch new codex/brightness-night-mode-driver --anchor main
but status
```

Expected: the new branch is applied from current driver `main`, with no
unrelated uncommitted changes assigned to it.

- [x] **Step 2: Write the failing compiled-artifact and GPIO-behavior contract**

Add `tests/backlight-contract.sh` assertions that compile the overlay and use
`fdtdump`/`fdtget` on the resulting DT artifact to require:

```text
compatible = "pwm-backlight"
pwms = <&pwm 1 200000 0>
brightness-levels = <0 255>
num-interpolated-steps = <255>
default-brightness-level = <13>
assigned-clock-rates = <1000000>
brcm,pins = <19>
brcm,function = <BCM2835_FSEL_ALT5>
backlight = <&planeradar_backlight>
```

The compiled DT checks must prove the named PWM backlight, GPIO19 Alt5 pinctrl,
the effective 1 MHz clock-framework assignment, panel `backlight` phandle, and
the absence of both `backlight-gpios` and the inert `clock-frequency` property.
Use the repository's existing local build contract to validate the in-progress
packaging rules. The target-bound immutable module/artifact proof belongs in
Task 12 after the target runs the kernel for which the bundle will be staged.
`tests/gpio_test.c` must behaviorally prove that quiesce deasserts CS, releases
SDA/SCL, and preserves the first error; it must not infer that behavior from C
source tokens. The contract's automated evidence is restricted to compiled
artifacts and behavior. Stable compiled symbol metadata may be inspected only
when the existing build emits it naturally. Register the new check as
`mise run test-backlight-contract` and make `verify` depend on it.

- [x] **Step 3: Run the focused tests and verify RED**

Run:

```bash
mise run test-backlight-contract
mise run test-gpio
mise run test-build-contract
```

Expected: the new compiled-overlay contract fails on the current GPIO-backed
artifact, while the existing GPIO test and local build contract establish the
safe helper and packaging baselines. The final physical-panel acceptance
remains required in Task 12.

- [x] **Step 4: Define the PWM backlight in the overlay**

Replace `backlight-gpios` with a panel phandle and add the stable root node:

```dts
planeradar_backlight: planeradar-backlight {
	compatible = "pwm-backlight";
	pwms = <&pwm 1 200000 0>;
	brightness-levels = <0 255>;
	num-interpolated-steps = <255>;
	default-brightness-level = <13>;
};
```

Add overlay fragments that enable `&pwm`, set
`assigned-clock-rates = <1000000>`, select a GPIO19 Alt5 pinctrl group, and
point `hyperpixel2r_panel.backlight` at the named node. Do not emit
`clock-frequency`; the Raspberry Pi PWM driver uses the clock framework rather
than that inert property. Extend `scripts/common.sh` and
`scripts/check-artifacts.sh` validation for the new fragments, fixups,
phandles, effective applied-DT clock, pinmux, period, interpolation, default,
and rejected inert property.

- [x] **Step 5: Adopt the DRM panel backlight lifecycle**

In `hyperpixel2r_kms_main.c`, remove `hp->backlight`, `hp->enabled`, direct
backlight enable/disable writes, and backlight-specific unwind. After
`drm_panel_init()` and before publishing the panel, call:

```c
ret = drm_panel_of_backlight(&hp->panel);
if (ret)
	return dev_err_probe(hp->dev, ret,
			     "failed to acquire panel backlight\n");
```

Keep `.enable`/`.disable` only if the supported DRM helper contract requires
callbacks; they must not touch GPIO or PWM. Preserve prepare/unprepare ST7701
commands, I2C adapter registration, touch child creation, lock ordering, and
error unwind. Remove `disable_backlight` from `hp2r_gpio_ops` and update
`tests/gpio_test.c` to prove quiesce still deasserts CS and releases SDA/SCL
while preserving the first error.

- [x] **Step 6: Run driver unit and contract verification**

Run:

```bash
mise run test-gpio
mise run test-backlight-contract
mise run test-build-contract
mise run verify
```

Expected: all tests pass; the compiled overlay proves the PWM backlight,
GPIO19 Alt5 pinctrl, assigned 1 MHz clock, panel phandle, and absence of both
`backlight-gpios` and `clock-frequency`; the local build contract validates the
in-progress packaging rules; and the GPIO test proves quiesce and first-error
behavior. Complete the target-bound immutable artifact proof and final
physical-panel acceptance in Task 12 before claiming the driver installed.

- [x] **Step 7: Commit only Task 1 files**

Stage the exact Task 1 paths, then commit only staged files:

```bash
but stage tests/backlight-contract.sh codex/brightness-night-mode-driver
but stage mise.toml codex/brightness-night-mode-driver
but stage overlays/hyperpixel2r-kms-overlay.dts codex/brightness-night-mode-driver
but stage kernel/hyperpixel2r_kms_main.c codex/brightness-night-mode-driver
but stage kernel/hyperpixel2r_kms_gpio.c codex/brightness-night-mode-driver
but stage kernel/hyperpixel2r_kms_gpio.h codex/brightness-night-mode-driver
but stage tests/gpio_test.c codex/brightness-night-mode-driver
but stage scripts/common.sh codex/brightness-night-mode-driver
but stage scripts/check-artifacts.sh codex/brightness-night-mode-driver
but stage tests/release-contract.sh codex/brightness-night-mode-driver
but commit codex/brightness-night-mode-driver --only -m "feat: expose PWM panel backlight"
but status
```

Expected: focused Task 1 commits only; no other workspace changes are included.

- [x] **Step 8: Record approved source evidence and defer the target-bound bundle**

Task 1 was independently approved at driver commit
`1141c119b91fd9e867cfc6bb59fa9bf1c17c47af`, tree
`b2afd1527dbe56cc11653921cebc2843405f1b97`, after these four focused
commits:

- `fa0b94d` — expose the standard PWM panel backlight
- `16f96a9` — match the target's void DRM lifecycle helpers
- `c901731` — enforce the effective PWM clock and privacy contracts
- `1141c11` — harden applied-artifact and privacy validation

The final `mise run verify` was GREEN in 1177.04 seconds. A native,
non-installing compile against the exact running `6.18.34+rpt-rpi-v8` headers
proved the unchanged module sources link with the expected AArch64 vermagic and
DRM symbols. That transient evidence is not the official immutable artifact
bundle.

The provenance-bound exporter cannot export the running 6.18.34 source because
the signed APT metadata now offers 6.18.39 and no trusted historical stanza was
recovered. Do not weaken that provenance requirement. The target already has
the 6.18.39 kernel and headers installed but no accepted custom module for that
release, so reboot was deliberately deferred. Task 12 performs a controlled
transition to the installed kernel and then runs the supported target-bound
export, build, and artifact-check sequence before driver staging.

---

### Task 2: Package permissions and capability as transactional driver state

**Repository:** `/Users/shayne/code/hyperpixel2r-kms`

**Files:**
- Create: `packaging/70-planeradar-backlight.rules`
- Modify: `scripts/common.sh`
- Modify: `scripts/build-driver.sh`
- Modify: `scripts/check-artifacts.sh`
- Modify: `scripts/stage-tryboot.sh`
- Modify: `scripts/lifecycle-remote.sh`
- Modify: `scripts/accepted-lifecycle.sh`
- Modify: `scripts/uninstall.sh`
- Modify: `scripts/package-release.sh`
- Modify: `release/driver-manifest.schema.json`
- Modify: `tests/build-contract.sh`
- Modify: `tests/boot-scripts.sh`
- Modify: `tests/boot-fixtures.sh`
- Modify: `tests/release-contract.sh`

**Interfaces:**
- Consumes: exact artifact bundle and existing tryboot/accepted rollback journals
- Produces: schema-2 exact artifacts containing the rule and `pwm-backlight-v1`
- Produces: root-owned mode-0644 udev rule with only `video` write access to the named device

- [ ] **Step 1: Write failing artifact and lifecycle fixtures**

Extend the shell suites to require manifest schema 2 fields:

```text
capability	pwm-backlight-v1
backlight_rule_file	70-planeradar-backlight.rules
backlight_rule_sha256	[0-9a-f]{64}
```

Add fixtures for missing/renamed/tampered rule, wrong capability, broad
`SUBSYSTEM=="backlight"` matching without a kernel name, interrupted staging,
candidate rollback, accepted replacement, and accepted uninstall. Assert the
final rule path is `/etc/udev/rules.d/70-planeradar-backlight.rules`, is
root-owned mode 0644, and is absent after uninstall when no proven prior rule
existed.

- [ ] **Step 2: Run the focused suites and verify RED**

Run:

```bash
mise run test-build-contract
mise run test-boot-scripts
mise run test-release-contract
```

Expected: failures identify the missing rule, manifest rows, journal fields,
and release capability.

- [ ] **Step 3: Add the narrowly scoped udev rule**

Create exactly:

```udev
SUBSYSTEM=="backlight", KERNEL=="planeradar-backlight", RUN+="/usr/bin/chgrp video /sys%p/brightness", RUN+="/usr/bin/chmod 0660 /sys%p/brightness"
```

Do not modify `actual_brightness`, `max_brightness`, any other backlight, or
any GPIO/PWM class. During physical preflight, verify the absolute helper paths
with `command -v chgrp chmod`; if Raspberry Pi OS resolves merged `/usr/bin`,
keep the committed rule unchanged.

- [ ] **Step 4: Extend exact artifacts and release manifest**

Advance only the internal artifact and public driver-manifest schemas needed
for this capability. Package the rule beside the module, overlay, and applied
DTB; validate its basename, regular-file status, digest, exact content, and
capability. Extend `release/driver-manifest.schema.json` with:

```json
"capabilities": {
  "type": "array",
  "const": ["pwm-backlight-v1"]
}
```

and make it required for schema 2. Emit `capabilities` from
`scripts/package-release.sh`; never infer support from a version string alone.

- [ ] **Step 5: Make the udev rule transactional**

Carry the rule filename/digest and prior-rule proof through candidate,
rollback, accepted, accepted-transition, and uninstall journals. Stage it
atomically with root ownership and mode 0644. After commit/accept, run:

```bash
sudo udevadm control --reload-rules
sudo udevadm trigger --action=add --subsystem-match=backlight --sysname-match=planeradar-backlight
```

Verify the class device resolves to that kernel name, `max_brightness` is a
positive integer, and a command running with the `video` group can round-trip
the current `brightness` value. Rollback and uninstall must restore an exact
proven prior rule or remove the candidate rule, reload udev, and retrigger only
the named device when present.

- [ ] **Step 6: Preserve the release-version boundary**

Do not choose or commit a new stable or prerelease driver version in this
implementation task. Local physical artifacts retain the repository's current
version field but are distinguished and accepted only by full source revision,
release-manifest digest, exact-artifact digest, and capability. A later
explicit release request selects the public version and refreshes versioned
metadata before anything is pushed.

- [ ] **Step 7: Verify the complete driver repository**

Run:

```bash
mise run verify
```

Then, against the supported Pi kernel inputs already used by this repository:

```bash
mise run build-driver
mise run check-artifacts
```

Expected: the source, module, overlay, applied DTB, udev rule, exact manifest,
and capability all verify with one source revision and no untracked payload.

- [ ] **Step 8: Commit only Task 2 files**

Stage every Task 2 path explicitly to the driver branch, then run:

```bash
but commit codex/brightness-night-mode-driver --only -m "feat: package backlight permissions"
but status
```

Expected: the driver branch contains two focused commits and remains unpushed.

---

### Task 3: Migrate Plane Radar settings to schema 3

**Repository:** `/Users/shayne/code/RPi-Plane-Radar`

**Files:**
- Modify: `src/model.rs`
- Modify: `src/settings.rs`
- Modify: `src/install.rs`
- Modify: `tests/settings.rs`
- Modify: `tests/install.rs`
- Create: `tests/fixtures/settings/v2.json`

**Interfaces:**
- Consumes: exact schema-1 and schema-2 JSON
- Produces: validated `BrightnessSettings` and `NightModeSettings` in schema 3
- Preserves: in-memory migration without startup rewrite

- [ ] **Step 1: Write failing defaults, migration, and validation tests**

Add assertions that `RadarSettings::default()` contains:

```rust
BrightnessSettings {
    day_percent: 100,
    night: NightModeSettings {
        enabled: false,
        brightness_percent: 30,
        start_hour: 20,
        start_minute: 0,
        red_mode: false,
    },
}
```

Load exact v1 and v2 fixtures through `SettingsStore::load()` and assert the
returned value is schema 3 while the fixture bytes and modification time stay
unchanged. Add table tests for 5/10/95/100 accepted; 0/1/6/101 rejected; hour
24 and minute 60 rejected; nested unknown fields rejected; future schema
rejected; the next save after migration writes schema 3.

- [ ] **Step 2: Run focused tests and verify RED**

Run:

```bash
mise exec -- cargo nextest run --test settings --test install
```

Expected: compilation fails because schema 3 and brightness types do not exist.

- [ ] **Step 3: Implement strict schema-3 types and migration**

Add the cross-task types to `src/model.rs`, their exact defaults, and a
`brightness` field on `RadarSettings`. In `src/settings.rs`, retain
`LegacyRadarSettingsV1`, add an exact `LegacyRadarSettingsV2` mirroring the
current schema-2 shape, and dispatch by the inspected integer schema version.
Both legacy conversions use `BrightnessSettings::default()`; only explicit
schema 3 is deserialized as the current strict type.

Validate percentages with:

```rust
fn valid_percent(value: u8) -> bool {
    (5..=100).contains(&value) && value % 5 == 0
}
```

Then validate `start_hour <= 23` and `start_minute <= 59` before accepting the
candidate.

- [ ] **Step 4: Update install defaults without changing ownership markers**

Change only the settings JSON seeded by `src/install.rs` to schema 3 and the
approved brightness defaults. Keep installer marker/schema versions unrelated
to settings unchanged. Assert fresh install output equals
`RadarSettings::default()` and upgrades do not overwrite a user's settings.

- [ ] **Step 5: Verify and commit the settings foundation**

Run:

```bash
mise exec -- cargo fmt --all --check
mise exec -- cargo nextest run --test settings --test install
```

Stage the exact Task 3 paths, then:

```bash
but stage src/model.rs br
but stage src/settings.rs br
but stage src/install.rs br
but stage tests/settings.rs br
but stage tests/install.rs br
but stage tests/fixtures/settings/v2.json br
but commit br --only -m "feat: add brightness settings schema"
but status
```

Expected: the commit lands only on `codex/brightness-night-mode`; route
confidence remains separate.

---

### Task 4: Implement bounded solar fetch and atomic cache

**Repository:** `/Users/shayne/code/RPi-Plane-Radar`

**Files:**
- Create: `src/solar.rs`
- Modify: `src/lib.rs`
- Modify: `Cargo.toml`
- Modify: `Cargo.lock`
- Create: `tests/solar.rs`
- Create: `tests/fixtures/open_meteo/solar.json`

**Interfaces:**
- Consumes: existing `HttpClient`, `Location`, `Clock`, and Open-Meteo forecast endpoint
- Produces: validated `SolarSchedule` and `/var/lib/planeradar/solar-schedule.json`
- Produces: sanitized `SolarErrorCategory` for runtime status/logging

- [ ] **Step 1: Add Jiff and failing request/response tests**

Add `jiff = "0.2"` with its standard timezone support and update the lockfile.
Write a recording HTTP client test requiring exactly:

```text
latitude=40.7769
longitude=-73.8740
daily=sunrise
timezone=auto
timeformat=unixtime
past_days=1
forecast_days=16
```

Assert verified HTTPS, the existing bounded body limit, a six-second timeout,
and no current-weather variables. Add response tests for 17 matching days,
integer or null sunrise, bounded IANA zone, duplicate/mismatched dates,
nonfinite/out-of-range coordinates, invalid timestamps, unsupported zoneinfo,
oversized body, TLS/status/JSON/schema categories, and no provider body in any
displayable error.

- [ ] **Step 2: Add failing cache tests**

Use a temporary state directory to prove exact-coordinate matching, label-only
location reuse, mismatch rejection, schema rejection, symlink/nonregular file
rejection, corrupt JSON ignore, successful atomic replacement, preservation of
a good cache after an invalid fetch, file sync, parent sync, and no write until
a successful response.

- [ ] **Step 3: Run the solar suite and verify RED**

Run:

```bash
mise exec -- cargo nextest run --test solar
```

Expected: compilation fails because the solar module and Jiff dependency are absent.

- [ ] **Step 4: Implement strict client parsing**

Implement `SolarClient<C: HttpClient>::fetch(&Location, u64)` using the
existing bounded HTTPS seam. Deserialize into private `#[serde(deny_unknown_fields)]`
wire types where appropriate, allow only integer-or-null sunrise values, and
require equal nonempty daily arrays with unique dates. Bound the timezone ID
before loading it with `jiff::tz::TimeZone::get`; do not echo invalid IDs in
errors. Convert the daily time array to canonical `YYYY-MM-DD` keys and keep
sunrise as checked `i64` Unix seconds.

- [ ] **Step 5: Implement cache load/save**

Expose:

```rust
pub fn load_cache(path: &Path, location: &Location) -> Option<SolarSchedule>;
pub fn save_cache(path: &Path, schedule: &SolarSchedule) -> Result<(), SolarError>;
```

Reuse the settings store's `NamedTempFile`-in-parent, write, flush, `sync_all`,
persist, and parent-directory `sync_all` discipline. Require schema 1 and exact
floating-point equality for latitude/longitude. Never use the location label
as an identity field.

- [ ] **Step 6: Verify and commit solar transport/cache**

Run:

```bash
mise exec -- cargo fmt --all --check
mise exec -- cargo nextest run --test solar
mise exec -- cargo deny check
```

Stage Task 4 paths to `br`, then:

```bash
but commit br --only -m "feat: cache radar-local sunrise data"
```

---

### Task 5: Resolve a DST-safe display policy

**Repository:** `/Users/shayne/code/RPi-Plane-Radar`

**Files:**
- Create: `src/night_mode.rs`
- Modify: `src/lib.rs`
- Modify: `src/model.rs`
- Create: `tests/night_mode.rs`

**Interfaces:**
- Consumes: schema-3 settings, matching optional `SolarSchedule`, Unix seconds
- Produces: pure `DisplayPolicy`, `Transition`, `SolarStatus`, and status facts
- Does not consume: monotonic elapsed time, browser timezone, or host timezone

- [ ] **Step 1: Write the policy truth table before implementation**

Add table-driven tests for night disabled, no location, new location without a
matching schedule, before start, exact start, after midnight, exact sunrise,
after sunrise, previous-local-date restart, null sunrise, expired forecast,
and schedule coordinate mismatch. Assert missing coverage ends at exactly
07:00 radar local and reports fallback rather than remaining night forever.

- [ ] **Step 2: Add DST and wall-clock jump tests**

Use `America/New_York` fixtures around both transitions. Assert a nonexistent
02:30 spring-forward start resolves to the first valid minute after the gap;
an ambiguous 01:30 fall-back start resolves to the first occurrence. Evaluate
the same policy independently before/after forward and backward NTP jumps and
assert the answer follows the supplied Unix time, not prior state.

- [ ] **Step 3: Run focused tests and verify RED**

Run:

```bash
mise exec -- cargo nextest run --test night_mode
```

Expected: compilation fails because `DisplayPolicy` and its evaluator do not exist.

- [ ] **Step 4: Implement the pure evaluator**

Expose:

```rust
pub fn display_policy(
    settings: &RadarSettings,
    schedule: Option<&SolarSchedule>,
    unix_seconds: u64,
) -> DisplayPolicy;
```

Resolve the response IANA zone with Jiff. Construct starts for the previous,
current, and next local dates; use compatible disambiguation that moves a gap
forward and chooses the earlier overlap. For each start, select the first
non-null sunrise instant strictly after it, otherwise create that date's next
07:00 local fallback. Treat the interval as `[start, sunrise)`.

Return day/`day_percent`/full color unless an enabled, configured, matching
schedule proves the instant is within a night interval. At night return
`night.brightness_percent` and `RedOnly` only when `night.red_mode` is true.

- [ ] **Step 5: Verify and commit policy**

Run:

```bash
mise exec -- cargo fmt --all --check
mise exec -- cargo nextest run --test night_mode
```

Stage Task 5 paths to `br`, then:

```bash
but commit br --only -m "feat: resolve radar-local night policy"
```

---

### Task 6: Run solar refresh independently and publish immutable status

**Repository:** `/Users/shayne/code/RPi-Plane-Radar`

**Files:**
- Create: `src/runtime/solar_worker.rs`
- Modify: `src/runtime.rs`
- Modify: `src/model.rs`
- Modify: `src/main.rs`
- Modify: `tests/runtime.rs`
- Create: `tests/runtime_solar.rs`

**Interfaces:**
- Consumes: `WorkerCommand`, current settings/location, solar cache/client, `Clock`, interruptible `Waiter`
- Produces: matching schedule, sanitized failure category/time, generation wakeups
- Preserves: ADS-B, ADSBDB, and weather worker isolation/cadence

- [ ] **Step 1: Write worker cadence and isolation tests**

Using the existing fake clock/waiter patterns, prove: night disabled causes no
solar request; enabled without location idles; enabled with location loads a
matching cache before its first request; success refreshes once per successful
local day; failures wait 30s, 60s, 5m, then 15m; waits are interrupted by
settings/location changes and stop; label-only changes retain cache; coordinate
changes publish no old schedule and fetch immediately; disabling clears the
need without disturbing weather; a blocked solar client does not delay ADS-B.
Add a coordinator bootstrap test proving a matching cache is in the initial
`RuntimeHandle::snapshot()` before `PlaneRadarApp` can render its first frame.

- [ ] **Step 2: Run worker tests and verify RED**

Run:

```bash
mise exec -- cargo nextest run --test runtime_solar --test runtime
```

Expected: missing runtime fields, worker, channel, and config paths fail compilation.

- [ ] **Step 3: Extend immutable runtime state**

Add to `RuntimeSnapshot`:

```rust
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BacklightAvailability { Unknown, Available, Unavailable }

pub solar_schedule: Option<Arc<SolarSchedule>>,
pub solar_last_error: Option<SolarFailure>,
pub backlight_availability: BacklightAvailability,
```

Add compare-and-record methods that publish only when the location coordinates
still match the completed request. A label-only change is not a mismatch.
Generation changes when solar or hardware status changes so the app and web
page observe it without polling hardware.

- [ ] **Step 4: Start, wake, and join the worker**

Add `solar_cache_path` and `solar_url` to `RuntimeConfig` defaults. Start a
dedicated worker thread and sender beside the existing ADS-B, enrichment, and
weather workers. Before returning the runtime handle, synchronously load only
the small matching local cache into the initial model and pass that schedule
to the worker; do not wait for network I/O. This makes cached active-night
policy available to the first physical frame. Include the worker in
`ChannelSettingsNotifier`, stop signaling, and join order. The worker refreshes
after bootstrap and persists only validated success. Logs contain a fixed
provider name and `SolarErrorCategory` only.

- [ ] **Step 5: Expose immutable policy inputs without a second policy owner**

Keep settings, matching schedule, solar status, and backlight availability in
the snapshot. The display loop recomputes the pure policy at least once per
`unix_seconds / 60` and after generation changes. The settings service computes
the same pure policy from one snapshot plus its current `Clock::unix_seconds()`
when rendering a page. Do not persist a second independently mutable policy in
the runtime model.

- [ ] **Step 6: Verify and commit runtime integration**

Run:

```bash
mise exec -- cargo fmt --all --check
mise exec -- cargo nextest run --test runtime_solar --test runtime
```

Stage Task 6 paths to `br`, then:

```bash
but commit br --only -m "feat: run solar schedule worker"
```

---

### Task 7: Control sysfs brightness with ordered two-second ramps

**Repository:** `/Users/shayne/code/RPi-Plane-Radar`

**Files:**
- Create: `src/backlight.rs`
- Modify: `src/lib.rs`
- Modify: `src/model.rs`
- Create: `tests/backlight.rs`

**Interfaces:**
- Consumes: named standard backlight device, monotonic ticks, `DisplayPolicy`
- Produces: effective hardware levels and `BacklightAvailability`
- Does not consume: GPIO/PWM APIs or root privileges

- [ ] **Step 1: Write sysfs discovery and mapping tests**

Build a temporary class-device tree and assert only a path whose basename is
`planeradar-backlight` is accepted. Require regular readable
`max_brightness`, regular readable/writable `brightness`, positive max, and
checked integer parsing. Table-test:

```rust
assert_eq!(percent_to_level(5, 255), 13);
assert_eq!(percent_to_level(30, 255), 77);
assert_eq!(percent_to_level(100, 255), 255);
assert_eq!(percent_to_level(50, 1000), 500);
```

The implementation formula is `(percent * max + 50) / 100` in a widened
integer type. Assert repeated effective levels do not write again.

- [ ] **Step 2: Write transition and failure tests**

With a recording fake, assert linear values at 0, 500, 1000, 1500, and 2000
ms; target changes restart from the current interpolated level; entering night
completes dimming before requesting red; leaving night requests full color at
the dim level before brightening; startup active night begins red without a
full-color frame; read/write/permission failure becomes unavailable, is
nonfatal, rate-limited, and does not disable red mode.

- [ ] **Step 3: Run the suite and verify RED**

Run:

```bash
mise exec -- cargo nextest run --test backlight
```

Expected: compilation fails because the trait, sysfs implementation, no-op,
and ramp controller do not exist.

- [ ] **Step 4: Implement the narrow hardware boundary**

Implement `SysfsBacklight::open(PathBuf)` and `NoopBacklight`. Cache
`max_level`, remember the last successful level, and open/write only the named
`brightness` attribute. Define a `BacklightController` whose update result
contains both the effective brightness and the color mode that is safe to
render for that tick. Use checked `u64` arithmetic and `Duration`; never sleep
inside the controller.

- [ ] **Step 5: Verify and commit the controller**

Run:

```bash
mise exec -- cargo fmt --all --check
mise exec -- cargo nextest run --test backlight
```

Stage Task 7 paths to `br`, then:

```bash
but commit br --only -m "feat: control display brightness"
```

---

### Task 8: Transform every physical frame and integrate display policy

**Repository:** `/Users/shayne/code/RPi-Plane-Radar`

**Files:**
- Modify: `src/render/mod.rs`
- Modify: `src/app.rs`
- Modify: `src/display.rs`
- Modify: `src/main.rs`
- Modify: `tests/app.rs`
- Modify: `tests/render_setup.rs`
- Modify: `tests/render_radar.rs`
- Create: `tests/goldens/setup-red.png`
- Create: `tests/goldens/waiting-red.png`
- Create: `tests/goldens/settings-red.png`
- Create: `tests/goldens/radar-red.png`

**Interfaces:**
- Consumes: `DisplayPolicy`, `BacklightController`, final validated `Frame`
- Produces: uploaded/debug-captured frame in the effective `FrameColorMode`
- Preserves: all existing renderer/theme/layout code paths

- [ ] **Step 1: Write exact red transform vectors**

Add a `Frame::apply_color_mode` test for transparent and opaque black, white,
pure RGB primaries, and every current theme color. Require alpha preservation,
zero green/blue, and:

```rust
let luma = (54_u16 * r as u16
    + 183_u16 * g as u16
    + 19_u16 * b as u16
    + 128) / 256;
```

Assert full color is byte-for-byte unchanged and a second red transform is not
applied by the application pipeline.

- [ ] **Step 2: Write app ordering and rerender tests**

Extend `AppRuntime` fakes and add a recording backlight. Assert color mode is
part of `RenderKey`; a wall-minute, settings, location, or solar generation
change reevaluates policy; color transition forces one rerender; brightness
ramp ticks do not rerender unchanged full-color frames; debug PNG bytes equal
the uploaded transformed frame. Cover radar, setup, waiting, and on-device
settings states.

- [ ] **Step 3: Run focused tests and verify RED**

Run:

```bash
mise exec -- cargo nextest run --test app --test render_setup --test render_radar
```

Expected: missing transform, policy key, injection seam, and goldens fail.

- [ ] **Step 4: Apply color after all rendering**

Make `Frame::apply_color_mode` mutate validated RGBA chunks in place. In
`PlaneRadarApp`, render the normal setup/radar/settings frame, then apply the
effective controller color mode before assigning `current_frame`, returning a
`DisplayUpdate`, or writing `debug.png`. Do not add red palette branches to any
renderer.

- [ ] **Step 5: Inject and tick the controller**

Have `main` attempt `SysfsBacklight::open` at the stable path and fall back to
`NoopBacklight` with a sanitized warning. Pass `Box<dyn Backlight>` into
`PlaneRadarApp`; tests use the fake. The existing display cadence calls the
controller with monotonic time. Add
`AppRuntime::record_backlight_availability(BacklightAvailability)` so the app
publishes status through the model without exposing a hardware handle to the
web server. Implement the required order:

```text
enter night: full color while ramping down -> red at night target
leave night: full color at night target -> ramp up
cached active-night startup: first frame red at driver boot level -> ramp to target
```

- [ ] **Step 6: Regenerate and inspect all red goldens**

Use the repository's existing golden update command, then inspect the four PNGs
to confirm labels, grid, aircraft, footer, QR, and status remain legible and
every non-alpha green/blue byte is zero. Run fixture inventory tests so no
unregistered image remains.

- [ ] **Step 7: Verify and commit display integration**

Run:

```bash
mise exec -- cargo fmt --all --check
mise exec -- cargo nextest run --test app --test render_setup --test render_radar
```

Stage Task 8 paths to `br`, then:

```bash
but commit br --only -m "feat: render scheduled red night mode"
```

---

### Task 9: Add the Brightness settings destination and live status

**Repository:** `/Users/shayne/code/RPi-Plane-Radar`

**Files:**
- Modify: `src/web.rs`
- Modify: `tests/web.rs`

**Interfaces:**
- Consumes: current settings plus immutable policy/solar/backlight snapshot
- Produces: strict schema-3 form mutation and accessible script-free controls
- Preserves: CSRF, session, host, origin, body, escaping, no-store, and request limits

- [ ] **Step 1: Write failing navigation/control tests**

Extend the test service so `SettingsService` returns a page snapshot containing
settings, display policy, solar status, and backlight availability. Assert the
sticky navigation order includes `Brightness` and links to
`#brightness-settings`. Require:

```html
<input type="range" name="brightness_day_percent" min="5" max="100" step="5" value="100">
<input type="range" name="brightness_night_percent" min="5" max="100" step="5" value="30">
<input type="time" name="brightness_night_start" step="60" value="20:00">
```

for day/night brightness and start time, plus checkbox sentinels for night
enabled and red-only. Assert explicit 5%, 50%, and 100% references, associated
labels/descriptions, saved values, no disabled night controls, and no
`<script` output at any viewport.

- [ ] **Step 2: Write strict form and status tests**

Add form cases for duplicate, missing, malformed, out-of-range, and non-step
percent/time fields; checkbox omission/sentinel semantics; section-scoped
errors; escaping; unrelated field preservation; immediate policy
reevaluation. Test exact status facts for disabled/upcoming/active/fallback/
waiting/driver-unavailable and human formatting under both existing 12- and
24-hour preferences.

- [ ] **Step 3: Run focused tests and verify RED**

Run:

```bash
mise exec -- cargo nextest run --test web
```

Expected: tests fail because Brightness navigation, fields, page status, and
parsers do not exist.

- [ ] **Step 4: Extend the settings service snapshot**

Replace `SettingsService::current()` with a focused immutable page value (or
add `page_state()`) that returns the current settings and runtime status in one
model-lock read. The web handler must not call Open-Meteo, read sysfs, or write
brightness. Retain `replace(candidate)` as the only mutation.

- [ ] **Step 5: Parse and validate schema-3 controls**

Add every new field and checkbox sentinel to `KNOWN_FIELDS`. Parse time as
exact `HH:MM` ASCII with hour/minute bounds and no timezone conversion. Parse
percentages as `u8`, then let `validate_settings` enforce range/step. Preserve
saved night values while disabled and preserve all unrelated settings.

- [ ] **Step 6: Render the responsive Brightness section**

Add `SettingsSection::Brightness`, nav summary, and one section in the approved
single-content flow. Use native controls, the existing design tokens, compact
range reference labels, and concise server-rendered status. Keep browser
styling full color. Use the owner's clock setting for text such as `8:00 PM`
or `20:00`, while the input value stays `20:00`.

- [ ] **Step 7: Verify desktop and mobile layouts**

Open `http://planeradar.local` only after deployment in Task 12; locally, run
the existing web fixture server and inspect wide, tablet, and narrow captures
with every section expanded. The sticky navigator must remain usable, content
must not split into uneven columns, and the Brightness section must not cause
horizontal overflow.

- [ ] **Step 8: Verify and commit the UX**

Run:

```bash
mise exec -- cargo fmt --all --check
mise exec -- cargo nextest run --test web
```

Stage only `src/web.rs` and `tests/web.rs` to `br`, then:

```bash
but commit br --only -m "feat: add brightness settings UX"
```

---

### Task 10: Require the brightness-capable driver in Plane Radar provenance

**Repository:** `/Users/shayne/code/RPi-Plane-Radar`

**Files:**
- Modify: `driver.lock.toml`
- Modify: `release/release-manifest.schema.json`
- Modify: `crates/planeradarctl/src/config.rs`
- Modify: `crates/planeradarctl/src/driver.rs`
- Modify: `crates/planeradarctl/src/release.rs`
- Modify: `crates/planeradarctl/src/system_install.rs`
- Modify: `crates/planeradarctl/src/operations.rs`
- Modify: `crates/planeradarctl/src/main.rs`
- Modify: `tests/release_contract.rs`
- Modify: `tests/ctl_end_to_end.rs`
- Modify: `tests/support/release_fixture.rs`
- Modify: `tests/fixtures/releases/*`

**Interfaces:**
- Consumes: driver schema-2 release and exact-artifact manifests
- Produces: explicit `pwm-backlight-v1` requirement through sync/stage/verify/rollback
- Preserves: exact source revision, digest, identity-bound reconnect, and rollback

- [ ] **Step 1: Locate exact current parser/fixture paths before editing**

Run:

```bash
rg -n "driver.lock|driver-manifest|manifest.txt|schema_version|source_revision|capabil" crates tests release scripts
```

Update the Files list in the execution ledger if the current refactor moved a
parser, but do not broaden the behavioral scope.

- [ ] **Step 2: Write failing capability rejection tests**

Add fixtures proving the app rejects a driver manifest with no capability,
an unknown capability, duplicate capability, tampered udev rule, missing rule
artifact, or inconsistent release/exact manifests. Prove a schema-2 manifest
with exactly `pwm-backlight-v1` passes and retains existing version, source
revision, asset digest, kernel release, overlay, module, and applied-DTB checks.

- [ ] **Step 3: Run release contract tests and verify RED**

Run:

```bash
mise exec -- cargo nextest run --test release_contract
```

Expected: the current parser accepts manifests without the capability/rule.

- [ ] **Step 4: Thread capability through the installer contract**

Add `required_capability = "pwm-backlight-v1"` to the lock schema and require
the downloaded or local driver manifest and exact artifact manifest to agree. Extend
the raw lifecycle argument/manifest allowlists for the udev rule without
weakening basename, ownership, mode, digest, or exact-row checks. Keep
candidate activation and rollback delegated to the driver scripts.

- [ ] **Step 5: Enforce capability while preserving the release boundary**

Add `required_capability = "pwm-backlight-v1"` to the checked-in lock, but do
not invent a GitHub version, source revision, or manifest digest for the local
driver candidate. The existing published driver identity now intentionally
fails capability verification, making a display-driver upgrade mandatory for
this app feature instead of silently accepting GPIO-only artifacts. Task 12
stages the exact local driver directly through its own verified tryboot
lifecycle and deploys the app as a source candidate, so it does not need to
pretend an unpublished driver asset is downloadable. A later explicit release
request first publishes the accepted driver and then refreshes the remaining
lock identity fields to that immutable asset.

- [ ] **Step 6: Verify and commit the contract**

Run:

```bash
mise exec -- cargo fmt --all --check
mise exec -- cargo nextest run --test release_contract
```

Stage Task 10 paths actually changed to `br`, then:

```bash
but commit br --only -m "feat: require PWM backlight driver capability"
```

---

### Task 11: Complete documentation and automated verification

**Repository:** `/Users/shayne/code/RPi-Plane-Radar`

**Files:**
- Modify: `README.md`
- Modify: `docs/install.md`
- Modify: `docs/architecture.md`
- Modify: `tests/docs_contract.rs` only if an existing non-prose contract must be mechanically updated
- Modify: any fixture inventory file named by failing existing tests

**Interfaces:**
- Consumes: completed driver/app behavior
- Produces: user/operator contract and green all-target verification

- [ ] **Step 1: Define the human documentation acceptance checklist**

Review the user/operator documentation for the 5–100% bounds, 30%/20:00
defaults, radar-local time, sunrise and 07:00 fallback, full-device red-only
scope, Open-Meteo coordinate privacy, named sysfs device, `video` permission,
two-second ramps, and the fact that no public release is created by local
physical staging. These are human documentation acceptance facts, not
assertions in `tests/docs_contract.rs`. Do not add exact prose-presence or
change-detector assertions; update `tests/docs_contract.rs` only for a real
existing non-prose contract that must change mechanically.

- [ ] **Step 2: Update user and architecture docs**

Document the Brightness page in owner language. In architecture docs, show
driver/app ownership, solar worker isolation, atomic cache, pure policy,
display-loop ramp, final-frame transform, sanitized failures, and release
capability. In install docs, add diagnostic read-only commands for the class
device and permission rule without recommending chmod workarounds.

- [ ] **Step 3: Run focused documentation tests**

Run:

```bash
mise run docs-check
```

- [ ] **Step 4: Run complete Plane Radar verification**

Run:

```bash
mise run fmt
mise run lint
mise run test
mise run deny
mise run verify
```

Expected: formatting, all-target/all-feature clippy, nextest, dependency
policy, goldens, fixture inventory, documentation, and release portability all
pass from the GitButler workspace.

- [ ] **Step 5: Commit documentation and any mechanical fixture inventory**

Stage only Task 11 paths to `br`, then:

```bash
but commit br --only -m "docs: explain brightness and night mode"
but status
```

Expected: both feature branches are internally clean; route-confidence remains
independent; nothing has been pushed or tagged.

---

### Task 12: Stage and physically accept the local prerelease

**Repositories:** `/Users/shayne/code/hyperpixel2r-kms`, `/Users/shayne/code/RPi-Plane-Radar`

**Files:**
- No source edits expected
- Local generated artifacts only: repository `dist/` output and temporary test captures

**Interfaces:**
- Consumes: exact local driver/app commits, the installed 6.18.39 target kernel,
  and provenance-bound local prerelease artifacts exported from that running kernel
- Produces: installed physical test state and an evidence log for owner acceptance
- Does not produce: Git tag, push, GitHub release, or public package

- [ ] **Step 1: Record immutable local identities and cleanliness**

Run:

```bash
but status
git -C /Users/shayne/code/hyperpixel2r-kms rev-parse \
  refs/heads/codex/brightness-night-mode-driver^{commit}
git -C /Users/shayne/code/RPi-Plane-Radar rev-parse \
  refs/heads/codex/brightness-night-mode^{commit}
```

Record the resolved GitButler commit IDs for the two feature branches and
confirm there are no uncommitted files assigned to them. Do not claim the
shared workspace `HEAD` alone identifies a virtual branch; record `but status`
branch commits as the authority.

- [ ] **Step 2: Verify source before the target kernel transition**

From the driver repository, run:

```bash
mise run verify
```

Expected: the final driver source, including Task 2, passes the complete suite.
Retain Task 1's native 6.18.34 exact-header result as compatibility evidence
only; do not treat it as an artifact that can be staged or accepted.

- [ ] **Step 3: Preflight and perform the controlled 6.18.39 transition**

The exporter deliberately derives its release from the target's live
`uname -r` and has no non-running-kernel option. Before rebooting, set the
known installed transition release and perform a read-only preflight:

```bash
transition_release=6.18.39+rpt-rpi-v8
ssh user@radar.local "
set -eu
test \"\$(uname -r)\" = 6.18.34+rpt-rpi-v8
dpkg-query -W -f='\${db:Status-Abbrev} \${Version}\\n' \
  linux-image-rpi-v8 linux-headers-rpi-v8
test -d /lib/modules/$transition_release/build
test -f /lib/modules/$transition_release/build/.config
test -f /lib/modules/$transition_release/build/Module.symvers
test ! -e /lib/modules/$transition_release/extra/hyperpixel2r_kms.ko
id planeradar
command -v chgrp chmod udevadm
systemctl is-active ssh planeradar
cat /etc/os-release
sudo test ! -e /var/lib/hyperpixel2r-kms/tryboot-state
sudo test ! -e /var/lib/hyperpixel2r-kms/rollback-state
sudo test -f /var/lib/hyperpixel2r-kms/accepted-state
sudo test ! -e /var/lib/hyperpixel2r-kms/accepted-transition
sudo test ! -e /var/lib/hyperpixel2r-kms/accepted-transition-prior-config.txt
"
prior_receipt="$(ssh user@radar.local \
  sudo cat /var/lib/hyperpixel2r-kms/accepted-state)"
receipt_value() {
  printf '%s\n' "$prior_receipt" |
    awk -F= -v wanted="$1" '$1 == wanted { print $2 }'
}
prior_driver_version="$(receipt_value driver_version)"
prior_source_revision="$(receipt_value source_revision)"
prior_kernel_release="$(receipt_value kernel_release)"
prior_overlay_file="$(receipt_value overlay_file)"
test "$prior_kernel_release" = 6.18.34+rpt-rpi-v8
```

Record the installed package versions, current accepted driver identity,
normal boot-config digest, accepted receipt, exact recovery commands that
select 6.18.34 for a normal boot, and exact forward commands that reselect the
installed 6.18.39 kernel. Confirm the existing normal boot selection is already
6.18.39, the Pi is on stable power, and SSH recovery does not depend on the
display or touch stack. This is a hard staging checkpoint: if either normal
kernel selection or independent SSH recovery cannot be proven, stop without
rebooting. Do not invent an exporter flag or weaken signed APT provenance.

Only after that checkpoint, request the already-configured normal reboot, wait
for identity-bound SSH reconnect, and require the exact live release:

```bash
set +e
ssh user@radar.local sudo reboot
reboot_status=$?
set -e
case "$reboot_status" in 0|255) ;; *) exit "$reboot_status" ;; esac
for attempt in {1..90}; do
  ssh -o BatchMode=yes -o ConnectTimeout=5 user@radar.local true \
    >/dev/null 2>&1 && break
  sleep 2
done
test "$(ssh user@radar.local uname -r)" = "$transition_release"
```

The physical panel may be unavailable between this normal kernel transition
and the verified tryboot candidate. If SSH does not return or the release does
not match, use the pre-recorded 6.18.34 recovery path and stop; do not continue
to artifact creation or staging.

- [ ] **Step 4: Export, build, check, and package the immutable driver bundle**

From a clean driver workspace whose tree matches the durable GitButler branch
commit, run the supported live-target sequence in this exact order:

```bash
driver_repo=/Users/shayne/code/hyperpixel2r-kms
driver_commit="$(git -C "$driver_repo" rev-parse refs/heads/codex/brightness-night-mode-driver^{commit})"
kernel_release="$(ssh user@radar.local uname -r)"
test "$kernel_release" = 6.18.39+rpt-rpi-v8
HP2R_TARGET=user@radar.local mise run export-target-kbuild
HP2R_TARGET=user@radar.local mise run build-driver -- \
  --kernel-release "$kernel_release" \
  --source-revision "$driver_commit"
HP2R_TARGET=user@radar.local mise run check-artifacts -- \
  --kernel-release "$kernel_release"
```

Expected: `export-target-kbuild` binds the signed 6.18.39 source metadata,
headers, kbuild tree, and base DTB to the live target; `build-driver` binds the
module and overlay to the durable branch commit; and `check-artifacts` proves
the exact module, applied DT, rule, capability, manifests, and digests. Any
failure stops the transition before tryboot staging.

Derive every lifecycle identity from that checked internal artifact manifest:

```bash
artifact_dir="$driver_repo/dist/artifacts/$kernel_release"
artifact_manifest="$artifact_dir/manifest.txt"
source "$driver_repo/scripts/common.sh"
candidate_driver_version="$(hp2r_manifest_value "$artifact_manifest" driver_version)"
candidate_revision="$(hp2r_manifest_value "$artifact_manifest" source_revision)"
candidate_release="$(hp2r_manifest_value "$artifact_manifest" kernel_release)"
candidate_manifest_sha="$(hp2r_sha256 "$artifact_manifest")"
candidate_module_file="$(hp2r_manifest_value "$artifact_manifest" module_file)"
candidate_module_sha="$(hp2r_manifest_value "$artifact_manifest" module_sha256)"
candidate_overlay_file="$(hp2r_manifest_value "$artifact_manifest" overlay_file)"
candidate_overlay_sha="$(hp2r_manifest_value "$artifact_manifest" overlay_sha256)"
test "$candidate_revision" = "$driver_commit"
test "$candidate_release" = "$kernel_release"
```

Export the exact driver branch commit into a temporary packaging clone so the
manifest never names the synthetic GitButler workspace commit:

```bash
driver_package_source="$(mktemp -d "${TMPDIR:-/tmp}/hp2r-brightness-package.XXXXXX")"
test ! -e "$driver_repo/dist/local-brightness-release"
git clone --no-local "$driver_repo" "$driver_package_source/repo"
git -C "$driver_package_source/repo" checkout --detach "$driver_commit"
(
  cd "$driver_package_source/repo"
  ./scripts/package-release.sh \
    --source-revision "$driver_commit" \
    --artifact-dir "$driver_repo/dist/artifacts" \
    --output "$driver_repo/dist/local-brightness-release"
)
shasum -a 256 "$driver_repo/dist/local-brightness-release/driver-manifest.json"
```

Verify the manifest through the driver release-contract command, confirm its
source commit equals the branch tip, require `pwm-backlight-v1`, and verify
every asset digest before transfer.

- [ ] **Step 5: Stage the driver through tryboot**

Because the host has an accepted receipt, first publish the exact prepared
replacement transition. `stage-tryboot` rejects an accepted replacement that
does not already have this identity-bound journal:

```bash
kernel_release="$(ssh user@radar.local uname -r)"
test "$kernel_release" = "$candidate_release"
HP2R_TARGET=user@radar.local \
  "$driver_repo/scripts/accepted-lifecycle.sh" \
    --action prepare-new \
    --driver-version "$candidate_driver_version" \
    --source-revision "$candidate_revision" \
    --kernel-release "$candidate_release" \
    --manifest-sha256 "$candidate_manifest_sha" \
    --module-file "$candidate_module_file" \
    --module-sha256 "$candidate_module_sha" \
    --overlay-file "$candidate_overlay_file" \
    --overlay-sha256 "$candidate_overlay_sha"
HP2R_TARGET=user@radar.local mise run stage-tryboot -- \
  --artifact-dir "$artifact_dir"
```

Follow the repository's printed
identity-bound reconnect command. Reboot only through the staged workflow.
After reconnect, verify the exact candidate before inspecting the class device:

```bash
HP2R_TARGET=user@radar.local mise run verify-boot -- \
  --expect-tryboot \
  --expect-driver-version "$candidate_driver_version" \
  --expect-overlay-file "$candidate_overlay_file" \
  --json
```

Do not continue until every automated health and provenance check passes.

- [ ] **Step 6: Verify sysfs identity, permission, and visible levels**

On the Pi, record:

```bash
readlink -f /sys/class/backlight/planeradar-backlight
cat /sys/class/backlight/planeradar-backlight/max_brightness
stat -c '%U %G %a %n' /sys/class/backlight/planeradar-backlight/brightness
```

As the `planeradar` service identity, round-trip the current value, then test
5%, 30%, and 100% mapped through the advertised max. Confirm each is visibly
distinct and stable with no flicker. Verify unrelated backlights and GPIO
remain unwritable.

Use the same rounded mapping as the app and restore the saved level after the
visual check:

```bash
ssh user@radar.local '
set -eu
path=/sys/class/backlight/planeradar-backlight
max=$(cat "$path/max_brightness")
saved=$(cat "$path/brightness")
for percent in 5 30 100; do
  level=$(( (percent * max + 50) / 100 ))
  sudo -u planeradar sh -c "printf %s $level > $path/brightness"
  sleep 3
done
sudo -u planeradar sh -c "printf %s $saved > $path/brightness"
'
```

Before accepting, exercise the candidate rollback once:

```bash
HP2R_TARGET=user@radar.local \
  "$driver_repo/scripts/accepted-lifecycle.sh" --action recover
```

Do not call plain `rollback-boot`: the accepted recovery action owns both the
tryboot rollback and accepted-transition cleanup. Apply the exact 6.18.34
normal-boot recovery commands recorded at the Step 3 checkpoint, then perform
the required normal reboot and verify the prior accepted identity:

```bash
set +e
ssh user@radar.local sudo reboot
reboot_status=$?
set -e
case "$reboot_status" in 0|255) ;; *) exit "$reboot_status" ;; esac
for attempt in {1..90}; do
  ssh -o BatchMode=yes -o ConnectTimeout=5 user@radar.local true \
    >/dev/null 2>&1 && break
  sleep 2
done
test "$(ssh user@radar.local uname -r)" = "$prior_kernel_release"
HP2R_TARGET=user@radar.local mise run verify-boot -- \
  --expect-normal \
  --expect-driver-version "$prior_driver_version" \
  --expect-overlay-file "$prior_overlay_file" \
  --json
ssh user@radar.local "
set -eu
sudo grep -Fxq 'source_revision=$prior_source_revision' \
  /var/lib/hyperpixel2r-kms/accepted-state
sudo test ! -e /var/lib/hyperpixel2r-kms/tryboot-state
sudo test ! -e /var/lib/hyperpixel2r-kms/rollback-state
sudo test ! -e /var/lib/hyperpixel2r-kms/accepted-transition
sudo test ! -e /var/lib/hyperpixel2r-kms/accepted-transition-prior-config.txt
sudo test ! -e \
  /usr/lib/hyperpixel2r-kms/$candidate_driver_version/$candidate_revision/$candidate_release
"
```

After the prior normal boot and cleanup proof pass, apply the exact 6.18.39
normal-boot selection already proven in Step 3, reboot normally, reconnect, and
require `uname -r` to equal `$candidate_release`. The display may again remain
unavailable until tryboot. Prepare a fresh accepted transition before the
second stage:

```bash
set +e
ssh user@radar.local sudo reboot
reboot_status=$?
set -e
case "$reboot_status" in 0|255) ;; *) exit "$reboot_status" ;; esac
for attempt in {1..90}; do
  ssh -o BatchMode=yes -o ConnectTimeout=5 user@radar.local true \
    >/dev/null 2>&1 && break
  sleep 2
done
test "$(ssh user@radar.local uname -r)" = "$candidate_release"
HP2R_TARGET=user@radar.local \
  "$driver_repo/scripts/accepted-lifecycle.sh" \
    --action prepare-new \
    --driver-version "$candidate_driver_version" \
    --source-revision "$candidate_revision" \
    --kernel-release "$candidate_release" \
    --manifest-sha256 "$candidate_manifest_sha" \
    --module-file "$candidate_module_file" \
    --module-sha256 "$candidate_module_sha" \
    --overlay-file "$candidate_overlay_file" \
    --overlay-sha256 "$candidate_overlay_sha"
HP2R_TARGET=user@radar.local mise run stage-tryboot -- \
  --artifact-dir "$artifact_dir"
HP2R_TARGET=user@radar.local mise run verify-boot -- \
  --expect-tryboot \
  --expect-driver-version "$candidate_driver_version" \
  --expect-overlay-file "$candidate_overlay_file" \
  --json
```

After each commanded reboot, follow the identity-bound reconnect output.
Repeat the named-device permission and visible-level check against the restaged
candidate before continuing.

- [ ] **Step 7: Accept the driver candidate and deploy Plane Radar locally**

Promote the restaged tryboot config, then advance the accepted transition to
`committed`. Neither command performs the required normal reboot:

```bash
HP2R_TARGET=user@radar.local mise run commit-boot
HP2R_TARGET=user@radar.local \
  "$driver_repo/scripts/accepted-lifecycle.sh" --action mark-committed
set +e
ssh user@radar.local sudo reboot
reboot_status=$?
set -e
case "$reboot_status" in 0|255) ;; *) exit "$reboot_status" ;; esac
for attempt in {1..90}; do
  ssh -o BatchMode=yes -o ConnectTimeout=5 user@radar.local true \
    >/dev/null 2>&1 && break
  sleep 2
done
test "$(ssh user@radar.local uname -r)" = "$candidate_release"
HP2R_TARGET=user@radar.local mise run verify-boot -- \
  --expect-normal \
  --expect-driver-version "$candidate_driver_version" \
  --expect-overlay-file "$candidate_overlay_file" \
  --json
HP2R_TARGET=user@radar.local \
  "$driver_repo/scripts/accepted-lifecycle.sh" --action mark-verified
HP2R_TARGET=user@radar.local \
  "$driver_repo/scripts/accepted-lifecycle.sh" --action finalize
ssh user@radar.local "
set -eu
sudo grep -Fxq 'driver_version=$candidate_driver_version' \
  /var/lib/hyperpixel2r-kms/accepted-state
sudo grep -Fxq 'source_revision=$candidate_revision' \
  /var/lib/hyperpixel2r-kms/accepted-state
sudo grep -Fxq 'kernel_release=$candidate_release' \
  /var/lib/hyperpixel2r-kms/accepted-state
sudo test ! -e /var/lib/hyperpixel2r-kms/tryboot-state
sudo test ! -e /var/lib/hyperpixel2r-kms/rollback-state
sudo test ! -e /var/lib/hyperpixel2r-kms/accepted-transition
sudo test ! -e /var/lib/hyperpixel2r-kms/accepted-transition-prior-config.txt
sudo test ! -e /boot/firmware/tryboot.txt
"
```

Only after the normal boot is verified and `finalize` rotates the exact accepted
receipt and retires the transition journal, export the exact app branch commit
into a temporary build clone so the embedded revision is the durable GitButler
branch commit rather than the synthetic workspace commit:

```bash
app_repo=/Users/shayne/code/RPi-Plane-Radar
app_commit="$(git -C "$app_repo" rev-parse refs/heads/codex/brightness-night-mode^{commit})"
app_build_source="$(mktemp -d "${TMPDIR:-/tmp}/planeradar-brightness-build.XXXXXX")"
git clone --no-local "$app_repo" "$app_build_source/repo"
git -C "$app_build_source/repo" checkout --detach "$app_commit"
(
  cd "$app_build_source/repo"
  mise run build-pi
  ./scripts/deploy-pi.sh user@radar.local
)
stage="$(cat "$app_build_source/repo/dist/last-stage-path")"
app_sha="$(awk '{print $1}' "$app_build_source/repo/dist/planeradar.sha256")"
ssh user@radar.local "
set -eu
candidate=/opt/planeradar/candidates/$app_commit
dropin=/etc/systemd/system/planeradar.service.d/90-brightness-candidate.conf
sudo test ! -e \"\$dropin\"
sudo install -d -o root -g root -m 0755 \"\$candidate\"
sudo install -d -o root -g root -m 0755 /etc/systemd/system/planeradar.service.d
sudo install -o root -g root -m 0755 '$stage/planeradar' \"\$candidate/planeradar\"
test \"\$(sudo sha256sum \"\$candidate/planeradar\" | awk '{print \$1}')\" = '$app_sha'
printf '%s\n' '$app_commit' | sudo tee \"\$candidate/REVISION\" >/dev/null
printf '%s\n' '$app_sha' | sudo tee \"\$candidate/SHA256\" >/dev/null
sudo chown root:root \"\$candidate/REVISION\" \"\$candidate/SHA256\"
sudo chmod 0644 \"\$candidate/REVISION\" \"\$candidate/SHA256\"
printf '%s\n' '[Service]' 'ExecStart=' \
  'ExecStart=/opt/planeradar/candidates/$app_commit/planeradar run' |
  sudo tee \"\$dropin\" >/dev/null
sudo chown root:root \"\$dropin\"
sudo chmod 0644 \"\$dropin\"
sudo systemctl daemon-reload
sudo systemctl restart planeradar.service
"
```

This is an explicitly local, reversible source-candidate deployment; it does
not alter the installer-owned stable binary or lifecycle receipt. The unique
systemd drop-in selects the root-owned, checksum-verified candidate while the
stable binary remains available for rollback. Verify `systemctl cat`, service
identity, embedded binary revision, `/healthz`, debug-frame capture, and
journal logs.

- [ ] **Step 8: Exercise accelerated policy transitions**

Use settings whose radar location matches the configured device. Set night
start a few minutes ahead and use a deterministic local solar fixture/test
endpoint or a temporary cached schedule through the existing test-only config
seam; do not alter production provider DNS. Verify exact start-minute entry,
exact sunrise exit, 5/30/100 ramps, disabled-mode return to 100%, 12/24 status
formatting, and the 07:00 fallback with missing future sunrise.

- [ ] **Step 9: Inspect every red physical state**

Enable red-only and physically inspect radar, waiting-for-network, setup QR,
and on-device settings states. Capture debug PNGs and verify all non-alpha
green/blue bytes are zero while QR scanning, aircraft distinction, grid,
labels, footer, and status remain legible. Restart the application during
active night and confirm no high-brightness or full-color flash.

- [ ] **Step 10: Verify lifecycle safety**

Confirm touch input and KMS output through service restart and full reboot.
Exercise module unload safety only through the driver's supported lifecycle.
Temporarily move the one candidate drop-in to
`90-brightness-candidate.conf.disabled`, reload systemd, and restart; verify the
stable installer-owned binary returns. Move the exact drop-in back, reload,
restart, and verify the feature revision returns. Confirm the accepted driver
receipt is unchanged throughout, then run both repositories' physical
smoke/doctor/status commands.

- [ ] **Step 11: Leave the prerelease installed and report evidence**

Leave the exact local brightness-driver candidate plus the exact Plane Radar
feature-branch binary installed for the owner to assess. Report exact branch
commit IDs, artifact SHA-256 values, Pi kernel,
driver identity, class-device max/current/mode, service/binary provenance,
transition observations, red-frame captures, reboot/rollback result, and any
nonfatal warnings. Explicitly state that no tag, push, or public release was
created.

---

## Final self-review checklist

- [ ] Every approved setting, default, validation rule, schedule boundary,
  fallback, transition ordering, failure mode, and privacy constraint maps to
  at least one implementation step and one test.
- [ ] The driver alone owns PWM/GPIO19/panel ordering; the application sees
  only the named standard backlight sysfs interface.
- [ ] Schema 1 and 2 migration is in-memory and schema 3 stays strict.
- [ ] Solar and weather remain independent, and disabled defaults cause no
  solar request.
- [ ] Jiff resolves the provider IANA timezone from system zoneinfo; browser,
  host, and monotonic clocks are not substituted for radar-local civil time.
- [ ] Red transformation occurs once, after complete rendering, before both
  upload and debug capture, across all physical states.
- [ ] Web handlers read immutable status and never access hardware or provider
  I/O; every new form field remains within existing security controls.
- [ ] Driver capability and rule provenance are exact and transactional; the
  unpublished candidate lock stays unpushed and is replaced with a real public
  driver asset only during a later explicitly authorized release.
- [ ] All code snippets and commands use concrete interfaces and paths; there
  are no implementation placeholders or unresolved design choices.
- [ ] Both repositories pass their complete verification commands before any
  physical staging or completion claim.
- [ ] Physical acceptance covers permission, 5/30/100 levels, touch, KMS,
  reboot, rollback, all red states, startup flash, provenance, and restoration
  of default full-color daytime behavior.
- [ ] The local prerelease remains installed for owner testing, while release
  selection, tags, pushes, and publication remain deferred pending explicit
  approval.

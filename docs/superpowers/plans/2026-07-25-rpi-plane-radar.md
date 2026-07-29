# RPi Plane Radar Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use
> `superpowers:subagent-driven-development` (recommended) or
> `superpowers:executing-plans` to implement this plan task-by-task. Steps use
> checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build, deploy, verify, and publish a faithful Rust-based Raspberry Pi
Plane Radar for a Pi Zero 2 W with a HyperPixel 2.1 Round touchscreen.

**Architecture:** One Rust 2024 crate owns pure domain, rendering, networking,
web, settings, installation, and runtime modules. The SDL main-thread boundary
uploads complete 480×480 frames and converts touch events; ADS-B and settings
HTTP each use one worker thread with immutable snapshots in a narrow shared
model. `mise` pins every development tool and drives native checks, an ARM64
Debian 13 container build on this Mac, checksummed deployment, and repeated
end-to-end checks on `pi@raspberrypi.local`.

**Tech Stack:** Rust 1.97.1, Rust 2024, mise 2026.7.7+, SDL2 2.32, tiny-skia,
fontdue, qrcode, ureq with rustls, tiny_http, serde, cargo-nextest, cargo-deny,
Docker/OrbStack, systemd, Raspberry Pi DRM/KMS, GitButler, and GitHub Actions.

## Global Constraints

- The approved design in
  `docs/superpowers/specs/2026-07-25-rpi-plane-radar-design.md` is authoritative.
- Every application crate contains `#![forbid(unsafe_code)]`; native safety
  boundaries remain inside audited dependencies such as SDL2.
- The logical frame is always RGBA 480×480. KMS and touch configuration own
  physical rotation.
- Never add Wi-Fi scanning, credentials, captive-portal behavior, or
  NetworkManager mutation to the app.
- Setup uses explicit place search or manual coordinates over LAN HTTP; browser
  geolocation is absent.
- Unit and golden tests never contact adsb.fi, Nominatim, or any live service.
- Never log coordinates, address-search text, CSRF tokens, complete settings,
  or HTTP bodies.
- Nominatim requests are explicit, cached for seven days, use an identifying
  User-Agent, and start at least 1.05 seconds apart.
- ADS-B requests verify TLS, poll no faster than every three seconds, and render
  no more than 64 non-ground aircraft.
- The Pi runtime contains the release binary, SDL2, and CA certificates; it
  contains no Rust compiler, Cargo, mise, container tooling, or build headers.
- `mise run build-pi` refuses an uncommitted workspace, builds in a pinned ARM64
  Debian 13 container, and emits an ELF/checksum/revision bundle under `dist/`.
- Every hardware checkpoint cross-builds on this Mac, deploys the checksummed
  artifact to `pi@raspberrypi.local`, and exercises the real screen or target
  service before dependent work continues.
- A failed hardware checkpoint is fixed by first adding an automated regression
  test in the responsible task.
- Run `but status` and `but diff` before each commit. Commit and push content
  only with GitButler.
- Do not create `github.com/shayne/RPi-Plane-Radar` until automated and physical
  acceptance passes on the exact installed revision.

## Planned File Map

| Path | Responsibility |
| --- | --- |
| `Cargo.toml`, `Cargo.lock`, `deny.toml` | Rust package, dependency lock, and dependency policy |
| `mise.toml`, `mise.lock` | Pinned tools and the only documented task entry points |
| `build.rs` | Embed the clean source revision in every binary |
| `src/cli.rs`, `src/main.rs`, `src/lib.rs` | Commands, process exit behavior, and module surface |
| `src/model.rs` | Immutable settings, traffic, airport, snapshot, and state types |
| `src/settings.rs` | Strict JSON validation and atomic persistence |
| `src/range.rs`, `src/geometry.rs` | Presets, units, local projection, clipping, and rim points |
| `src/http.rs`, `src/adsb.rs` | TLS HTTP abstraction and adsb.fi client/parser |
| `src/airports.rs`, `src/bin/build-airports.rs` | Embedded OurAirports loader and deterministic generator |
| `src/render/{mod,theme,text,radar,setup}.rs` | RGBA frame, font rasterization, radar, and QR screens |
| `src/display.rs`, `src/touch.rs` | SDL/KMS owner loop, normalized input, and gesture state machine |
| `src/geocode.rs`, `src/web.rs` | Rate-limited Nominatim lookup and secure LAN settings server |
| `src/network.rs`, `src/runtime.rs`, `src/app.rs` | Local URL discovery, shared snapshots, workers, state transitions, signals, and display handler |
| `src/install.rs`, `packaging/planeradar.service` | Idempotent target setup and hardened systemd unit |
| `src/assets/` | Embedded DejaVu font, font license, and compressed runway dataset |
| `scripts/{build-pi,deploy-pi,smoke-pi}.sh` | Container build, checksum transfer, and target verification |
| `packaging/Dockerfile.build`, `.dockerignore` | Reproducible ARM64 Debian 13 build environment |
| `tests/` | Integration, fixture, golden-image, and installation tests |
| `docs/` and `README.md` | Operator, architecture, provenance, and troubleshooting documentation |

---

### Task 1: Establish the Rust and mise Build Contract

**Files:**

- Create: `Cargo.toml`
- Create: `Cargo.lock`
- Create: `deny.toml`
- Create: `mise.toml`
- Create: `mise.lock`
- Create: `build.rs`
- Create: `src/lib.rs`
- Create: `src/main.rs`
- Create: `src/cli.rs`
- Create: `tests/cli.rs`
- Create: `packaging/Dockerfile.build`
- Create: `scripts/build-pi.sh`
- Create: `scripts/deploy-pi.sh`
- Create: `scripts/smoke-pi.sh`
- Create: `.dockerignore`
- Create: `.gitignore`
- Create: `.github/workflows/ci.yml`

**Interfaces:**

- Consumes: clean GitButler branch `rpi-port` based on upstream `69c10785`.
- Produces: `planeradar version`, which prints
  `planeradar <semver> (<40-character revision>|development)`.
- Produces mise tasks `fmt`, `lint`, `test`, `deny`, `verify`, `build-pi`,
  `deploy-pi`, and `smoke-pi`.
- Produces `dist/planeradar`, `dist/planeradar.sha256`,
  `dist/planeradar.revision`, and `dist/planeradar.readelf.txt`.

- [ ] **Step 1: Write the failing CLI revision test**

```rust
// tests/cli.rs
use std::process::Command;

#[test]
fn version_reports_name_and_revision() {
    let output = Command::new(env!("CARGO_BIN_EXE_planeradar"))
        .arg("version")
        .output()
        .expect("run planeradar");
    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).expect("utf8");
    assert!(stdout.starts_with("planeradar "));
    assert!(stdout.contains("development") || stdout.contains('('));
}
```

- [ ] **Step 2: Add the package manifest and prove the test fails**

Use Rust 2024 and `rust-version = "1.97.1"`. Add exact compatible minor lines:

```toml
[package]
name = "planeradar"
version = "0.1.0"
edition = "2024"
rust-version = "1.97.1"
build = "build.rs"
license = "MIT"

[dependencies]
clap = { version = "4.5", features = ["derive"] }
csv = "1.3"
flate2 = "1.1"
fontdue = "0.9"
log = "0.4"
env_logger = "0.11"
nix = { version = "0.30", features = ["net", "signal"] }
png = "0.18"
qrcode = "0.14"
rand = "0.9"
serde = { version = "1.0", features = ["derive"] }
serde_json = "1.0"
sha2 = "0.10"
signal-hook = "0.3"
subtle = "2.6"
tempfile = "3.20"
thiserror = "2.0"
tiny_http = "0.12"
tiny-skia = "0.11"
ureq = { version = "3.1", features = ["json"] }
url = "2.5"

[target.'cfg(target_os = "macos")'.dependencies]
sdl2 = { version = "0.38", features = ["bundled", "static-link"] }

[target.'cfg(target_os = "linux")'.dependencies]
sdl2 = "0.38"
```

Run:

```bash
mise exec rust@1.97.1 cmake@4.3.3 -- cargo test --test cli
```

Expected: failure because the `version` command does not exist.

- [ ] **Step 3: Implement the command and embedded revision**

```rust
// build.rs
fn main() {
    println!("cargo:rerun-if-env-changed=PLANERADAR_REVISION");
    let revision = std::env::var("PLANERADAR_REVISION")
        .unwrap_or_else(|_| "development".to_owned());
    println!("cargo:rustc-env=PLANERADAR_REVISION={revision}");
}
```

```rust
// src/cli.rs
use clap::{Parser, Subcommand};

#[derive(Debug, Parser)]
#[command(name = "planeradar")]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    Version,
}

pub fn version_line() -> String {
    format!(
        "planeradar {} ({})",
        env!("CARGO_PKG_VERSION"),
        env!("PLANERADAR_REVISION")
    )
}
```

Both `src/lib.rs` and `src/main.rs` begin with
`#![forbid(unsafe_code)]`. `main` parses `Cli`, prints `version_line()`, and
exits zero.

- [ ] **Step 4: Define the pinned mise tasks**

```toml
min_version = "2026.7.7"

[tools]
rust = "1.97.1"
cmake = "4.3.3"
"cargo:cargo-deny" = "0.20.2"
"cargo:cargo-nextest" = "0.9.140"

[tasks.fmt]
run = "cargo fmt --all --check"

[tasks.lint]
run = "cargo clippy --all-targets --all-features -- -D warnings"

[tasks.test]
run = "cargo nextest run --all-features"

[tasks.deny]
run = "cargo deny check"

[tasks.verify]
depends = ["fmt", "lint", "test", "deny"]

[tasks.build-pi]
run = "./scripts/build-pi.sh"

[tasks.deploy-pi]
depends = ["build-pi"]
run = "./scripts/deploy-pi.sh"

[tasks.smoke-pi]
run = "./scripts/smoke-pi.sh"
```

Run `mise install` followed by `mise lock`. Configure `deny.toml` to reject
unknown registries, duplicate wildcard dependencies, yanked crates, and unknown
licenses while allowing MIT, Apache-2.0, BSD-2-Clause, BSD-3-Clause, ISC,
MPL-2.0, Unicode-3.0, Zlib, and OFL-1.1.

- [ ] **Step 5: Implement the ARM64 container build**

`packaging/Dockerfile.build` starts from `rust:1.97.1-trixie`, installs
`libsdl2-dev`, `pkg-config`, `binutils`, `file`, and `ca-certificates`, builds
`planeradar --release --locked`, runs `file` and `readelf -d`, and exports the
binary plus reports from a scratch artifact stage.
`.dockerignore` excludes `.git`, `target`, `dist`, editor state, and OS metadata
but includes `Cargo.lock`, all Rust sources/assets, and packaging files.

`scripts/build-pi.sh` must:

```bash
#!/usr/bin/env bash
set -euo pipefail

test -z "$(git status --porcelain)" || {
  echo "build-pi requires a clean workspace" >&2
  exit 1
}
docker info >/dev/null 2>&1 || {
  command -v orbctl >/dev/null && orbctl start
}
for attempt in {1..30}; do
  docker info >/dev/null 2>&1 && break
  sleep 1
done
docker info >/dev/null
source_ref="${PLANERADAR_SOURCE_REF:-rpi-port}"
revision="$(git rev-parse --verify "${source_ref}^{commit}")"
rm -rf dist
mkdir -p dist
docker buildx build --platform linux/arm64 \
  --build-arg "PLANERADAR_REVISION=${revision}" \
  --file packaging/Dockerfile.build \
  --target artifact \
  --output type=local,dest=dist .
printf '%s\n' "$revision" > dist/planeradar.revision
(cd dist && shasum -a 256 planeradar > planeradar.sha256)
file dist/planeradar | grep -q 'ARM aarch64'
```

The implementation may use a temporary output directory and atomic rename, but
the final paths and checks are exact.

- [ ] **Step 6: Implement checksummed staging and smoke scripts**

`deploy-pi.sh` transfers the four `dist` files to a newly created
`/tmp/planeradar-stage.<suffix>` directory, verifies the checksum remotely, sets
mode `0755`, and writes the stage path to `dist/last-stage-path`.
`smoke-pi.sh` reads that path and asserts remote `planeradar version` contains
the exact `dist/planeradar.revision`.

- [ ] **Step 7: Add CI using the same mise tasks**

The workflow uses `runs-on: ubuntu-24.04-arm`, `actions/checkout@v6`, and
`jdx/mise-action@v4`; it runs `mise run verify` and then:

```bash
PLANERADAR_SOURCE_REF=HEAD mise run build-pi
file dist/planeradar
```

It triggers for pushes and pull requests and uploads no release artifact.

- [ ] **Step 8: Pass native verification and the first ARM64 smoke**

```bash
mise run verify
but status
but diff
but commit rpi-port -m "build: establish Rust and mise toolchain"
mise run build-pi
mise run deploy-pi
mise run smoke-pi
```

Expected: native checks pass; `file` reports ARM aarch64; the Pi executes the
binary and prints the committed `rpi-port` revision.

---

### Task 2: Prove SDL/KMS and Touch on the HyperPixel

**Files:**

- Create: `src/display.rs`
- Create: `src/install.rs`
- Create: `tests/boot_config.rs`
- Create: `tests/display.rs`
- Modify: `src/cli.rs`
- Modify: `src/main.rs`
- Modify: `src/lib.rs`

**Interfaces:**

- Consumes: `cli::Command`, the Task 1 ARM64 artifact/deploy contract.
- Produces:
  `ensure_overlay(input: &str, declaration: &str) -> (String, bool)`.
- Produces:
  `edit_boot_config(path: &Path, declaration: &str) -> Result<bool, InstallError>`.
- Produces:
  `DisplayConfig { width: u32, height: u32, video_driver: String, fullscreen: bool }`
  with production default `480, 480, "kmsdrm", true`.
- Produces normalized
  `InputEvent::{Pressed, Moved, Released, Quit}` with logical 480×480
  coordinates.
- Produces:
  `run_display<H: DisplayHandler>(config: DisplayConfig, handler: &mut H) -> Result<(), DisplayError>`.
- Produces `planeradar probe` and
  `planeradar configure-display --boot-config <path> --declaration <text>`.

- [ ] **Step 1: Write failing boot-config tests**

```rust
// tests/boot_config.rs
use planeradar::install::ensure_overlay;

#[test]
fn adds_one_overlay_under_all() {
    let source = "[all]\ndtoverlay=vc4-kms-v3d\n";
    let (updated, changed) = ensure_overlay(
        source,
        "dtoverlay=vc4-kms-dpi-hyperpixel2r",
    );
    assert!(changed);
    assert_eq!(
        updated.matches("dtoverlay=vc4-kms-dpi-hyperpixel2r").count(),
        1
    );
}

#[test]
fn second_edit_is_identical() {
    let source = "[all]\ndtoverlay=vc4-kms-dpi-hyperpixel2r\n";
    assert_eq!(
        ensure_overlay(source, "dtoverlay=vc4-kms-dpi-hyperpixel2r"),
        (source.to_owned(), false)
    );
}
```

Add named tests for a commented declaration, duplicate active declarations,
CRLF input, missing final newline, and no `[all]` section.

- [ ] **Step 2: Write failing input-normalization tests**

```rust
// tests/display.rs
use planeradar::display::{normalize_finger, InputEvent};

#[test]
fn normalized_finger_coordinates_fill_logical_frame() {
    assert_eq!(
        normalize_finger(7, 0.25, 0.75, true),
        InputEvent::Pressed {
            pointer_id: 7,
            x: 120.0,
            y: 360.0,
        }
    );
}
```

Cover mouse-left mapping, ignored mouse buttons, clamping of edge values, quit,
and finger-up/motion.

- [ ] **Step 3: Run focused tests to verify they fail**

```bash
mise exec -- cargo test --test boot_config --test display
```

Expected: unresolved `install` and `display` modules.

- [ ] **Step 4: Implement safe boot editing**

`ensure_overlay` preserves unrelated text and newline style, removes duplicate
active declarations, and inserts exactly one declaration under the last
`[all]`. `edit_boot_config` creates
`config.txt.planeradar-backup` with `create_new(true)` before the first change,
writes and fsyncs a sibling temporary file, preserves file mode, renames it,
and fsyncs the parent directory.

- [ ] **Step 5: Implement the SDL owner loop and probe**

```rust
pub trait DisplayHandler {
    fn step(
        &mut self,
        events: &[InputEvent],
        now: std::time::Instant,
    ) -> DisplayUpdate;
}

pub struct DisplayUpdate {
    pub frame: Option<Vec<u8>>,
    pub exit: bool,
}
```

`run_display` owns SDL context, window canvas, event pump, texture creator, and
one persistent `RGBA32` streaming texture in one function so no SDL lifetime
escapes. It selects `kmsdrm`, requests fullscreen 480×480, hides the cursor,
normalizes finger/mouse events, uploads only complete 480×480×4 frames, and caps
the loop at 30 Hz.

The probe handler draws opaque colored edge markers, `TOP`, a center cross, and
live touch dots without needing the radar renderer. It exits on quit or after
30 seconds and prints only SDL driver, logical mode, and normalized event type
and coordinates.

- [ ] **Step 6: Pass tests and commit**

```bash
mise run verify
but status
but diff
but commit rpi-port -m "feat: add HyperPixel SDL probe"
```

- [ ] **Step 7: Cross-build and enable the overlay on the Pi**

```bash
mise run build-pi
mise run deploy-pi
stage="$(cat dist/last-stage-path)"
ssh -t pi@raspberrypi.local \
  "sudo '$stage/planeradar' configure-display \
   --boot-config /boot/firmware/config.txt"
```

Show the exact config diff before accepting the write. Preserve the existing
`vc4-kms-v3d` line and do not alter I2C, hostname, or networking. Reboot only
when the command reports `changed`.

- [ ] **Step 8: Run the physical probe and calibrate**

```bash
ssh pi@raspberrypi.local \
  'cat /sys/class/drm/*/status; \
   for modes in /sys/class/drm/*/modes; do echo "$modes"; cat "$modes"; done'
ssh -t pi@raspberrypi.local \
  "sudo env SDL_VIDEODRIVER=kmsdrm '$stage/planeradar' probe"
```

Confirm a connected 480×480 connector, full circular coverage, physical `TOP`,
and matching touch coordinates. If required, test one overlay rotation or
touch-axis parameter at a time:
`rotate=90|180|270`, `touchscreen-swapped-x-y`,
`touchscreen-inverted-x`, and `touchscreen-inverted-y`. Commit the smallest
working declaration as the installer default.

- [ ] **Step 9: Lock any required calibration into source**

If the bare declaration is not correct, add a failing test that expects the
observed calibrated `DEFAULT_HYPERPIXEL_DECLARATION`, update the constant, run
`mise run verify`, and commit:

```bash
but status
but diff
but commit rpi-port -m "fix: calibrate HyperPixel orientation"
mise run build-pi
mise run deploy-pi
```

Run the probe once more from the new artifact. If the bare declaration was
correct, record a proved no-op and make no calibration commit.

**Hardware checkpoint 1:** stop unless the screen works at 480×480. If touch is
missing, record the kernel/input evidence; the web path may temporarily carry
setup, but orientation must be correct before gesture acceptance.

---

### Task 3: Define Immutable Models and Atomic Settings

**Files:**

- Create: `src/model.rs`
- Create: `src/settings.rs`
- Create: `tests/settings.rs`
- Modify: `src/lib.rs`

**Interfaces:**

- Consumes: no hardware or network interfaces.
- Produces `Units`, `Location`, `RadarSettings`, `Aircraft`, `Airport`,
  `Runway`, `RadarSnapshot`, and `AppState`.
- Produces:
  `validate_settings(value: serde_json::Value) -> Result<RadarSettings, SettingsError>`.
- Produces:
  `SettingsStore::new(path: PathBuf)`,
  `load(&self) -> Result<RadarSettings, SettingsError>`, and
  `save(&self, settings: &RadarSettings) -> Result<(), SettingsError>`.

- [ ] **Step 1: Write failing default and round-trip tests**

```rust
// tests/settings.rs
use planeradar::model::{Location, RadarSettings, Units};
use planeradar::settings::SettingsStore;

#[test]
fn defaults_require_location_setup() {
    let settings = RadarSettings::default();
    assert_eq!(settings.schema_version, 1);
    assert_eq!(settings.location, None);
    assert_eq!(settings.units, Units::Kilometres);
    assert!(settings.show_runways);
    assert_eq!(settings.range_index, 1);
}

#[test]
fn settings_round_trip_atomically() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = SettingsStore::new(dir.path().join("settings.json"));
    let expected = RadarSettings {
        schema_version: 1,
        location: Some(Location {
            latitude: 40.7128,
            longitude: -74.0060,
            label: "New York, NY".to_owned(),
        }),
        units: Units::Kilometres,
        show_runways: true,
        range_index: 1,
    };
    store.save(&expected).expect("save");
    assert_eq!(store.load().expect("load"), expected);
}
```

- [ ] **Step 2: Add failing strict-validation tests**

Use a table of JSON values to reject latitude outside ±90, longitude outside
±180, non-numbers, non-finite programmatic values, schema versions other than
1, units other than `km|mi`, range indices outside 0–3, unknown top-level keys,
malformed JSON, and missing required fields. Simulate rename failure by making
the destination a directory and assert the previously valid file remains.

- [ ] **Step 3: Run focused tests to verify they fail**

```bash
mise exec -- cargo test --test settings
```

- [ ] **Step 4: Implement the exact model**

```rust
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RadarSettings {
    pub schema_version: u32,
    pub location: Option<Location>,
    pub units: Units,
    pub show_runways: bool,
    pub range_index: u8,
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
```

Define the remaining cross-task types exactly:

```rust
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
```

Use finite-coordinate validation after Serde deserialization. Keep runtime
timestamps out of `RadarSettings`.

- [ ] **Step 5: Implement durable atomic persistence**

Missing files return defaults. Invalid existing files return `SettingsError`
without replacement. Save deterministic pretty JSON to a `NamedTempFile` in
the target directory, flush, `sync_all`, persist over the target, then open and
`sync_all` the parent directory. Create a missing parent with mode `0750`.

- [ ] **Step 6: Pass tests and commit**

```bash
mise run verify
but status
but diff
but commit rpi-port -m "feat: add validated atomic settings"
```

---

### Task 4: Port Range Semantics and Correct Geographic Projection

**Files:**

- Create: `src/range.rs`
- Create: `src/geometry.rs`
- Create: `tests/range.rs`
- Create: `tests/geometry.rs`
- Modify: `src/lib.rs`

**Interfaces:**

- Consumes: `model::{Location, Units}`.
- Produces `RANGE_RING3_KM: [f64; 4] = [5.0, 10.0, 15.0, 25.0]`.
- Produces `RangePreset { ring3_km: f64, outer_km: f64 }`.
- Produces:
  `range_preset(index: u8) -> Result<RangePreset, RangeError>`,
  `next_range_index(index: u8) -> u8`, and
  `format_range_label(preset: RangePreset, units: Units) -> String`.
- Produces:
  `offset_km(origin: &Location, latitude: f64, longitude: f64) -> OffsetKm`,
  `project_to_radar(...) -> ProjectedPoint`, and
  `rim_point(dx_km: f64, dy_km: f64, radius_px: f64) -> (i32, i32)`.

- [ ] **Step 1: Write failing range tests**

```rust
#[test]
fn cycles_all_upstream_ranges() {
    assert_eq!(
        [0, 1, 2, 3].map(next_range_index),
        [1, 2, 3, 0]
    );
    assert_eq!(range_preset(1).expect("range").ring3_km, 10.0);
    assert!((range_preset(1).expect("range").outer_km - 13.333_333).abs() < 1e-6);
}
```

Assert `10 km`, mile conversion using `0.6213711922`, and rejection of index 4.

- [ ] **Step 2: Write failing projection tests**

At New York and at 70° latitude, assert north decreases screen Y, east
increases X, equal physical distances have equal pixel radii within 0.5 pixels,
and east-west scale uses cosine of mean latitude. Assert N/E/S/W rim points are
`(240,2)`, `(478,240)`, `(240,478)`, and `(2,240)` for radius 238.

- [ ] **Step 3: Run tests to verify they fail**

```bash
mise exec -- cargo test --test range --test geometry
```

- [ ] **Step 4: Implement range and local projection**

```rust
pub const EARTH_RADIUS_KM: f64 = 6_371.008_8;

pub fn offset_km(origin: &Location, latitude: f64, longitude: f64) -> OffsetKm {
    let lat0 = origin.latitude.to_radians();
    let lat1 = latitude.to_radians();
    let dlon = (longitude - origin.longitude).to_radians();
    OffsetKm {
        east: EARTH_RADIUS_KM * dlon * ((lat0 + lat1) / 2.0).cos(),
        north: EARTH_RADIUS_KM * (lat1 - lat0),
    }
}
```

`project_to_radar` uses center `(240.0, 240.0)`, rounds only the returned pixel
position, and sets `inside_ring` using the aircraft-safe radius supplied by the
caller.

- [ ] **Step 5: Pass tests and commit**

```bash
mise run verify
but status
but diff
but commit rpi-port -m "feat: port range and radar projection"
```

---

### Task 5: Implement the TLS HTTP Boundary and ADS-B Client

**Files:**

- Create: `src/http.rs`
- Create: `src/adsb.rs`
- Create: `tests/adsb.rs`
- Create: `tests/fixtures/adsb/aircraft.json`
- Create: `tests/fixtures/adsb/empty.json`
- Create: `tests/fixtures/adsb/malformed.json`
- Create: `tests/live_adsb.rs`
- Modify: `src/lib.rs`

**Interfaces:**

- Consumes: `model::{Aircraft, Location}`.
- Produces `HttpRequest`, `HttpResponse`, `HttpError`, `HttpClient`, and
  `UreqHttpClient`.
- Produces:
  `parse_aircraft(value: &serde_json::Value, max: usize, show_ground: bool) -> Result<Vec<Aircraft>, AdsbError>`.
- Produces:
  `AdsbClient<C: HttpClient>::fetch(&self, location: &Location, radius_km: f64) -> Result<Vec<Aircraft>, AdsbError>`.

- [ ] **Step 1: Create realistic ADS-B fixtures**

The main fixture includes full records, padded `flight`, hex-only identity,
numeric and `"ground"` altitudes, absent coordinates, every heading/speed
fallback, malformed optional fields, and 66 valid aircraft to exercise the cap.

- [ ] **Step 2: Write failing parser tests**

```rust
#[test]
fn parser_preserves_upstream_field_preference() {
    let value: serde_json::Value =
        serde_json::from_str(include_str!("fixtures/adsb/aircraft.json"))
            .expect("fixture");
    let aircraft = parse_aircraft(&value, 64, false).expect("parse");
    let first = &aircraft[0];
    assert_eq!(first.callsign, "UAL123");
    assert_eq!(first.nose_degrees, 91.0);
    assert_eq!(first.track_degrees, 93.0);
    assert_eq!(first.ground_speed_knots, 420.0);
    assert!(aircraft.iter().all(|item| item.altitude != "GND"));
    assert!(aircraft.len() <= 64);
}
```

Add named tests for heading order
`true_heading→mag_heading→track→dir→0`, track order
`track→true_heading→mag_heading→dir→0`, speed order `gs→tas→ias→0`,
callsign `trim(flight)→hex`, altitude `alt_baro→alt_geom`, missing `ac`, and
malformed top-level schema.

- [ ] **Step 3: Write failing request tests with a fake HTTP client**

Assert the URL is:

```text
https://opendata.adsb.fi/api/v3/lat/40.712800/lon/-74.006000/dist/7.2
```

for 13.3333 km, the request uses `verify_tls=true`, connect timeout 3.05
seconds, read timeout 10 seconds, and maps timeout, status, body, and JSON
errors without retaining the previous snapshot inside the client.

- [ ] **Step 4: Run focused tests to verify they fail**

```bash
mise exec -- cargo test --test adsb
```

- [ ] **Step 5: Implement the shared HTTP abstraction and client**

```rust
pub trait HttpClient: Send + Sync + 'static {
    fn execute(&self, request: HttpRequest) -> Result<HttpResponse, HttpError>;
}

pub struct HttpRequest {
    pub url: String,
    pub query: Vec<(String, String)>,
    pub headers: Vec<(String, String)>,
    pub connect_timeout: Duration,
    pub read_timeout: Duration,
    pub verify_tls: bool,
}

pub struct HttpResponse {
    pub status: u16,
    pub body: Vec<u8>,
}
```

`UreqHttpClient` rejects `verify_tls=false` in production code and uses rustls
certificate verification. `AdsbClient` formats coordinates to six decimals and
nautical miles to one decimal. It never logs URLs because they contain location.

- [ ] **Step 6: Pass tests and perform one explicit integration request**

```bash
mise run verify
PLANERADAR_TEST_LAT=40.7128 \
PLANERADAR_TEST_LON=-74.0060 \
mise exec -- cargo test --test live_adsb -- --ignored --nocapture
```

`tests/live_adsb.rs` contains one `#[ignore]` test that reads both environment
variables, calls `AdsbClient`, and prints only aircraft count and request
duration. It asserts no TLS warning/error and never prints coordinates or URL.

- [ ] **Step 7: Commit**

```bash
but status
but diff
but commit rpi-port -m "feat: add verified ADS-B client"
```

---

### Task 6: Port the OurAirports Dataset Reproducibly

**Files:**

- Create: `src/airports.rs`
- Create: `src/bin/build-airports.rs`
- Create: `src/assets/large_airports.json.gz`
- Create: `tests/airports.rs`
- Create: `tests/fixtures/ourairports/airports.csv`
- Create: `tests/fixtures/ourairports/runways.csv`
- Delete: `include/data/large_airports.h`
- Delete: `src/data/large_airports_data.cpp`
- Modify: `src/lib.rs`
- Modify: `mise.toml`

**Interfaces:**

- Consumes: `geometry::offset_km`,
  `model::{Airport, Location, Runway}`.
- Produces:
  `build_dataset(airports: impl Read, runways: impl Read) -> Result<AirportDataset, AirportError>`.
- Produces:
  `write_dataset(dataset: &AirportDataset, writer: impl Write) -> Result<(), AirportError>`.
- Produces:
  `load_embedded() -> Result<Vec<Airport>, AirportError>`.
- Produces:
  `airports_within(airports: &[Airport], origin: &Location, radius_km: f64, max: usize) -> Vec<&Airport>`.
- Produces mise task `refresh-airports`.

- [ ] **Step 1: Create CSV fixtures and failing generator tests**

Fixtures include one large airport with two valid runways, a small airport, a
closed runway, a row missing one endpoint, malformed coordinates, and
non-ASCII names.

```rust
#[test]
fn generator_filters_and_orders_deterministically() {
    let dataset = build_dataset(
        include_bytes!("fixtures/ourairports/airports.csv").as_slice(),
        include_bytes!("fixtures/ourairports/runways.csv").as_slice(),
    )
    .expect("dataset");
    assert_eq!(dataset.schema_version, 1);
    assert_eq!(dataset.airports.len(), 1);
    assert_eq!(dataset.airports[0].ident, "KJFK");
    assert_eq!(dataset.airports[0].runways.len(), 2);
}
```

Write the same dataset twice and assert byte-identical gzip output with mtime
zero, sorted airports by identifier, and sorted runways by endpoint tuple.

- [ ] **Step 2: Run focused tests to verify they fail**

```bash
mise exec -- cargo test --test airports
```

- [ ] **Step 3: Implement the versioned compact schema**

Use Serde JSON records shaped as:

```json
{
  "schema_version": 1,
  "source": "OurAirports",
  "airports": [{
    "ident": "KJFK",
    "latitude": 40.639447,
    "longitude": -73.779317,
    "runways": [{
      "le": [40.648659, -73.791870],
      "he": [40.622246, -73.770584]
    }]
  }]
}
```

Select only `large_airport`, reject closed runways and incomplete endpoints,
round to seven decimal places, and use `flate2::GzBuilder::mtime(0)`.

- [ ] **Step 4: Implement resource loading and radius filtering**

`load_embedded` reads `include_bytes!("assets/large_airports.json.gz")`,
requires schema 1/source `OurAirports`, and validates every coordinate.
`airports_within` returns at most `max` ordered by distance then identifier.

- [ ] **Step 5: Generate and validate production data**

The `refresh-airports` task downloads only:

```text
https://ourairports.com/data/airports.csv
https://ourairports.com/data/runways.csv
```

into a temporary directory, runs `build-airports`, and atomically replaces the
embedded gzip. Run it once, compare the new airport/runway counts with the old
C++ generated constants, then delete the two old generated files.

- [ ] **Step 6: Pass tests and commit**

```bash
mise run refresh-airports
mise run verify
but status
but diff
but commit rpi-port -m "feat: port OurAirports runway data"
```

---

### Task 7: Render the Faithful 480×480 Radar

**Files:**

- Create: `src/render/mod.rs`
- Create: `src/render/theme.rs`
- Create: `src/render/text.rs`
- Create: `src/render/radar.rs`
- Create: `src/assets/DejaVuSans-Bold.ttf`
- Create: `src/assets/DejaVu-FONT-LICENSE.txt`
- Create: `tests/render_radar.rs`
- Create: `tests/support/mod.rs`
- Create: `tests/goldens/radar-empty.png`
- Create: `tests/goldens/radar-traffic.png`
- Create: `tests/goldens/radar-stale.png`
- Modify: `src/lib.rs`
- Modify: `src/cli.rs`
- Modify: `src/main.rs`

**Interfaces:**

- Consumes: airport loader, projection, range, and all Task 3 radar models.
- Produces:
  `FontAsset::from_static(bytes: &'static [u8]) -> Result<FontAsset, RenderError>`
  and `FontAsset::embedded() -> Result<FontAsset, RenderError>`.
- Produces:
  `Frame::new(width: u32, height: u32, rgba: Vec<u8>) -> Result<Frame, RenderError>`,
  `dimensions(&self) -> (u32, u32)`, `pixels(&self) -> &[u8]`,
  `save_png(&self, path: &Path) -> Result<(), RenderError>`.
- Produces test-only `FrameAssertions` helpers `pixel`,
  `dark_square_count`, `region_is_white`, and `assert_matches_golden` from
  `tests/support/mod.rs`.
- Produces:
  `RadarRenderer::new(font: FontAsset)`,
  `render(&mut self, snapshot: &RadarSnapshot, settings: &RadarSettings, airports: &[Airport], now: Duration) -> Result<Frame, RenderError>`.
- Produces `planeradar demo radar --seconds <u64>`.

- [ ] **Step 1: Write failing theme and geometry-level render tests**

```rust
#[test]
fn empty_radar_has_exact_size_and_palette() {
    let mut renderer = test_renderer();
    let frame = renderer
        .render(&empty_snapshot(), &configured_settings(), &[], Duration::ZERO)
        .expect("render");
    assert_eq!(frame.dimensions(), (480, 480));
    assert_eq!(frame.pixel(0, 0), [4, 10, 28, 255]);
    assert_ne!(frame.pixel(240, 240), [4, 10, 28, 255]);
}
```

Add named tests for four rings centered at `(240,240)` with radius 214,
N/S/E/W bounds, east aircraft placement, outer traffic at the 238-pixel rim,
runways disabled, vector clipping, and `DATA STALE` appearing only when
`now - fetched_at >= 30s`.

- [ ] **Step 2: Write failing golden comparison tests**

Decode committed PNGs to RGBA and compare every byte against deterministic
empty, traffic, and stale fixtures. On mismatch, write
`target/golden-failures/<name>.actual.png` without modifying expected files.

- [ ] **Step 3: Run focused tests to verify they fail**

```bash
mise exec -- cargo test --test render_radar
```

- [ ] **Step 4: Implement frame, palette, and font rasterization**

```rust
pub const SIZE: u32 = 480;
pub const CENTER: (f32, f32) = (240.0, 240.0);
pub const GRID_OUTER_RADIUS: f32 = 214.0;
pub const BACKGROUND: [u8; 4] = [4, 10, 28, 255];
pub const GRID: [u8; 4] = [16, 100, 32, 255];
pub const AIRCRAFT: [u8; 4] = [255, 0, 0, 255];
pub const TRACK: [u8; 4] = [255, 0, 255, 255];
pub const TAG_TYPE: [u8; 4] = [255, 200, 0, 255];
pub const TAG_ALTITUDE: [u8; 4] = [90, 200, 255, 255];
pub const RUNWAY: [u8; 4] = [56, 150, 170, 255];
pub const RUNWAY_LABEL: [u8; 4] = [110, 210, 230, 255];
```

Rasterize the embedded DejaVu font with `fontdue` onto `tiny_skia::Pixmap`.
Keep the DejaVu license beside the font and mention it in public notices.

- [ ] **Step 5: Implement cached background and dynamic traffic**

Cache static backgrounds by a `BackgroundKey` containing
`latitude.to_bits()`, `longitude.to_bits()`, `range_index`, `units`, and
`show_runways`; do not attempt to hash raw `f64` values.
Double every upstream layout dimension: strokes, center dot, triangle, speed
vector, rim dot, text heights, and gaps. Draw tags in
callsign/type/altitude order, vectors on the fixed upstream 60-second reference
scale, runway labels above lines, and traffic outside the aircraft-safe ring as
red rim dots.

- [ ] **Step 6: Generate and inspect the three goldens**

Add `planeradar render-fixtures --output tests/goldens`, invoke it once, and
open all three PNGs at original size. Confirm palette, cardinal labels, range
label, runways, headings, vectors, tags, rim dots, and stale notice. Golden
tests never update files automatically.

- [ ] **Step 7: Pass checks and commit**

```bash
mise exec -- cargo run -- render-fixtures --output tests/goldens
mise run verify
but status
but diff
but commit rpi-port -m "feat: render the 480 pixel radar"
```

---

### Task 8: Render the QR Setup and Settings Screen

**Files:**

- Create: `src/render/setup.rs`
- Create: `tests/render_setup.rs`
- Create: `tests/goldens/setup-required.png`
- Create: `tests/goldens/settings.png`
- Modify: `src/render/mod.rs`
- Modify: `src/cli.rs`
- Modify: `src/main.rs`

**Interfaces:**

- Consumes: `render::{FontAsset, Frame, RenderError}`.
- Produces:
  `SetupRenderer::new(font: FontAsset) -> SetupRenderer`.
- Produces:
  `SetupRenderer::render(local_url: &str, ip_url: Option<&str>, configured: bool, message: &str) -> Result<Frame, RenderError>`.
- Produces `planeradar demo setup --seconds <u64>`.

- [ ] **Step 1: Write failing QR and bounds tests**

```rust
#[test]
fn setup_frame_encodes_stable_local_url() {
    let frame = test_setup_renderer()
        .render(
            "http://planeradar.local",
            Some("http://10.0.4.74"),
            false,
            "Open this page to set the radar location",
        )
        .expect("render");
    assert_eq!(frame.dimensions(), (480, 480));
    assert!(frame.dark_square_count(QR_BOUNDS) > 100);
    assert!(frame.region_is_white(QR_QUIET_ZONE));
}
```

Assert QR modules use an integer pixel size, the encoded payload is always
`http://planeradar.local`, both URL text bounds remain inside the circular safe
area, missing IP renders `WAITING FOR NETWORK`, configured text says tap to
return, and required setup never says tap to dismiss.

- [ ] **Step 2: Write failing setup golden tests**

Compare deterministic required/configured frames byte-for-byte and write actual
failures under `target/golden-failures`.

- [ ] **Step 3: Run focused tests to verify they fail**

```bash
mise exec -- cargo test --test render_setup
```

- [ ] **Step 4: Implement QR and setup rendering**

Use `qrcode::QrCode` with medium error correction and a four-module quiet zone.
Choose the largest integer module size that leaves URL and instruction text
within the circle. Render `http://planeradar.local` and optional numeric URL
outside the code; escape/control-filter the message before rasterizing.

- [ ] **Step 5: Generate, inspect, pass, and commit**

```bash
mise exec -- cargo run -- render-fixtures --output tests/goldens
mise run verify
but status
but diff
but commit rpi-port -m "feat: render QR setup screen"
```

Open both PNGs at original size and confirm square modules, quiet zone, readable
URLs, and circular-edge clearance before committing.

---

### Task 9: Hardware Checkpoint 2 — Radar and QR on the Physical Panel

**Files:**

- No source files expected.
- If target behavior differs, modify the responsible Task 2, 7, or 8 file only
  after adding a failing regression test there.

**Interfaces:**

- Consumes: `planeradar demo radar`, `planeradar demo setup`, and the
  checksummed Task 1 deployment bundle.
- Produces: verified physical palette/layout/orientation and a phone-scannable
  QR result.

- [ ] **Step 1: Verify a clean source revision and build**

```bash
but status
but diff
mise run verify
mise run build-pi
mise run deploy-pi
mise run smoke-pi
```

Expected: clean workspace, green checks, ARM64 artifact, remote version equal to
`dist/planeradar.revision`.

- [ ] **Step 2: Show the deterministic radar fixture on the Pi**

```bash
stage="$(cat dist/last-stage-path)"
ssh -t pi@raspberrypi.local \
  "sudo env SDL_VIDEODRIVER=kmsdrm \
   '$stage/planeradar' demo radar --seconds 45"
```

Confirm dark-blue full circle, four green rings, crosshairs, N/S/E/W, range
label, teal runway, red aircraft, magenta vector, colored tag lines, and rim
dot. Compare orientation with the inspected PNG.

- [ ] **Step 3: Show and scan the setup fixture**

```bash
ssh -t pi@raspberrypi.local \
  "sudo env SDL_VIDEODRIVER=kmsdrm \
   '$stage/planeradar' demo setup \
   --ip-url http://10.0.4.74 --seconds 60"
```

Scan the code with a phone and confirm it resolves to
`http://planeradar.local`. Confirm both URLs and instructions are legible at
normal viewing distance.

- [ ] **Step 4: Record the checkpoint**

Record connector, mode, physical rotation, QR scan result, artifact revision,
and checksum in the execution log. Source changes require a regression test,
new GitButler commit, rebuild, redeploy, and repetition of all four steps.

**Hardware checkpoint 2:** do not start web/runtime integration until both
rendered screens pass on the physical panel.

---

### Task 10: Implement Tap and Three-Second Hold Recognition

**Files:**

- Create: `src/touch.rs`
- Create: `tests/touch.rs`
- Modify: `src/lib.rs`

**Interfaces:**

- Consumes: `display::InputEvent`.
- Produces `Gesture::{Tap, LongPress}`.
- Produces:
  `GestureRecognizer::handle(event: &InputEvent, now: Duration) -> Vec<Gesture>`
  and `GestureRecognizer::tick(now: Duration) -> Vec<Gesture>`.

- [ ] **Step 1: Write the failing gesture matrix**

```rust
#[test]
fn long_press_fires_once_and_release_is_consumed() {
    let mut recognizer = GestureRecognizer::default();
    assert!(recognizer
        .handle(&press(1, 240.0, 240.0), Duration::ZERO)
        .is_empty());
    assert_eq!(
        recognizer.tick(Duration::from_secs(3)),
        vec![Gesture::LongPress]
    );
    assert!(recognizer
        .handle(&release(1, 240.0, 240.0), Duration::from_millis(3100))
        .is_empty());
}
```

Add named tests for a 100 ms tap, movement of exactly 18 pixels, movement over
18 pixels, duplicate press, second pointer, release without press, repeated
ticks after long press, and 250 ms post-release debounce.

- [ ] **Step 2: Run focused tests to verify they fail**

```bash
mise exec -- cargo test --test touch
```

- [ ] **Step 3: Implement the clock-independent state machine**

Store one active pointer, initial position/time, cancellation flag,
long-press-fired flag, and last release time. `tick` is the only path that emits
`LongPress`, exactly at or after three continuous seconds. Release emits `Tap`
only when movement stayed at or below 18 pixels, long press never fired, and
debounce elapsed.

- [ ] **Step 4: Pass tests and commit**

```bash
mise run verify
but status
but diff
but commit rpi-port -m "feat: add touch gesture recognition"
```

---

### Task 11: Add Cached, Rate-Limited Nominatim Search

**Files:**

- Create: `src/time.rs`
- Create: `src/geocode.rs`
- Create: `tests/geocode.rs`
- Create: `tests/fixtures/nominatim/results.json`
- Modify: `src/lib.rs`

**Interfaces:**

- Consumes: `http::{HttpClient, HttpRequest}` and `model::Location`.
- Produces `Clock`, `Sleeper`, `SystemClock`, and `ThreadSleeper`.
- Produces:
  `GeocodeResult { display_name: String, location: Location }`.
- Produces:
  `GeocodeService::search(&mut self, query: &str) -> Result<Vec<GeocodeResult>, GeocodeError>`.
- Produces:
  `Geocoder<C, K, S>::search(&mut self, query: &str) -> Result<Vec<GeocodeResult>, GeocodeError>`.
- Cache schema 1 stores normalized-query keys, result records, and Unix expiry
  seconds.

- [ ] **Step 1: Write failing parse and privacy tests**

```rust
#[test]
fn parses_valid_results_and_caps_at_five() {
    let mut geocoder = fixture_geocoder();
    let results = geocoder.search("New York").expect("search");
    assert!(!results.is_empty());
    assert!(results.len() <= 5);
    assert!(results[0].location.latitude.is_finite());
}
```

Test malformed/out-of-range coordinates, missing display name, blank query,
control characters, HTML-like names remaining plain data, and captured logs
containing neither the raw nor normalized query.

- [ ] **Step 2: Write failing cache and rate-limit tests**

With fake HTTP, clock, and sleeper implementations, assert cache hits make zero
requests; distinct misses start at least 1.05 seconds apart; requests contain
`q`, `format=jsonv2`, `limit=5`, and `addressdetails=0`; the User-Agent is
`RPi-Plane-Radar/0.1 (+https://github.com/shayne/RPi-Plane-Radar)`; cache
success lasts seven days; and timeout, status, malformed JSON, and empty results
do not mutate settings.

- [ ] **Step 3: Run focused tests to verify they fail**

```bash
mise exec -- cargo test --test geocode
```

- [ ] **Step 4: Implement clock, normalization, cache, and lookup**

```rust
pub trait Clock: Send + Sync {
    fn monotonic(&self) -> Duration;
    fn unix_seconds(&self) -> u64;
}

pub trait Sleeper: Send + Sync {
    fn sleep(&self, duration: Duration);
}

pub trait GeocodeService: Send {
    fn search(&mut self, query: &str)
        -> Result<Vec<GeocodeResult>, GeocodeError>;
}
```

Normalize cache keys with trimmed/collapsed whitespace and Unicode lowercase.
Keep original query only in request memory. Use
`https://nominatim.openstreetmap.org/search` by default, allow a configured
provider base, reuse Task 3 durable atomic-write behavior, and never log query
or result text.

- [ ] **Step 5: Pass tests and commit**

```bash
mise run verify
but status
but diff
but commit rpi-port -m "feat: add rate-limited geocoding"
```

---

### Task 12: Build the Secure LAN Settings Server

**Files:**

- Create: `src/web.rs`
- Create: `tests/web.rs`
- Modify: `src/lib.rs`

**Interfaces:**

- Consumes: geocoder, `RadarSettings`, settings validation, and Task 3 storage.
- Produces:
  `SettingsService::current() -> RadarSettings` and
  `SettingsService::replace(RadarSettings) -> Result<(), WebError>`.
- Produces:
  `HealthSource::health() -> HealthSnapshot`.
- Produces:
  `HealthSnapshot { configured: bool, state: AppState, data_stale: bool, revision: &'static str }`.
- Produces:
  `SettingsServer::bind(address: SocketAddr, settings: Arc<dyn SettingsService>, geocoder: Arc<Mutex<Box<dyn GeocodeService>>>, health: Arc<dyn HealthSource>, allowed_hosts: Arc<dyn Fn() -> HashSet<String> + Send + Sync>) -> Result<SettingsServer, WebError>`.
- Produces:
  `SettingsServer::run(&self, stop: &AtomicBool) -> Result<(), WebError>`.
- Routes: `GET /`, `GET /healthz`, `POST /search`, `POST /settings`.

- [ ] **Step 1: Write failing page and health tests**

Start on `127.0.0.1:0` in a test thread. Assert `/` is UTF-8 HTML containing
`http://planeradar.local`, search, manual latitude/longitude, units, runways,
range, OpenStreetMap attribution, and the disclosure that submitted search text
is sent to OpenStreetMap. Assert it contains no Wi-Fi fields and no geolocation
JavaScript. Assert `/healthz` returns `configured`, `state`, `data_stale`, and
`revision` without coordinates or search history.

- [ ] **Step 2: Write failing mutation-security tests**

```rust
#[test]
fn settings_post_requires_matching_csrf_and_origin() {
    let server = test_server();
    let response = server.post_form(
        "/settings",
        &[("latitude", "40.7"), ("longitude", "-74.0")],
        None,
        Some("http://evil.example"),
    );
    assert_eq!(response.status, 403);
}
```

Add named tests for random session cookie/token, wrong token, accepted
`.local`/current-IP Origin, absent Origin with same-host Referer, invalid
coordinates retaining old settings, selected search result, manual
coordinates, escaped place name, bodies over 16 KiB returning 413, wrong
content type returning 415, and every unlisted route returning 404.

- [ ] **Step 3: Run focused tests to verify they fail**

```bash
mise exec -- cargo test --test web
```

- [ ] **Step 4: Implement controller and server**

Use `tiny_http`, `url::form_urlencoded`, `rand` for 256-bit tokens, and
`subtle::ConstantTimeEq`. Keep a bounded in-memory session map with one-hour
expiry. Validate a complete candidate `RadarSettings`, perform one
`SettingsService::replace`, then redirect with status 303. Search returns
escaped selectable results and never persists until selection.

- [ ] **Step 5: Pass tests and commit**

```bash
mise run verify
but status
but diff
but commit rpi-port -m "feat: add LAN settings server"
```

---

### Task 13: Integrate Shared Runtime State and Headless Workers

**Files:**

- Create: `src/network.rs`
- Create: `src/runtime.rs`
- Create: `tests/network.rs`
- Create: `tests/runtime.rs`
- Modify: `src/model.rs`
- Modify: `src/web.rs`
- Modify: `src/cli.rs`
- Modify: `src/main.rs`
- Modify: `src/lib.rs`

**Interfaces:**

- Consumes: ADS-B client, geocoder, settings store, web service traits, clock,
  range, and network interfaces from `nix::ifaddrs`.
- Produces:
  `RuntimeModel::snapshot() -> RuntimeSnapshot`,
  `replace_settings(&self, settings: RadarSettings) -> u64`,
  `record_aircraft(&self, aircraft: Vec<Aircraft>, fetched_at: Duration) -> u64`,
  `record_adsb_error(&self, at: Duration) -> u64`, and
  `set_urls(&self, local_url: String, ip_url: Option<String>) -> u64`.
- Produces:
  `discover_ip_url(route_table: &str, interfaces: impl Iterator<Item = InterfaceAddress>) -> Option<String>`.
- Produces:
  `AdsbWorker::run(commands: Receiver<WorkerCommand>, stop: Arc<AtomicBool>)`.
- Produces:
  `RuntimeCoordinator::start(config: RuntimeConfig) -> Result<RuntimeHandle, RuntimeError>`.
- Produces `planeradar run --headless` and production defaults:
  settings `/var/lib/planeradar/settings.json`, cache
  `/var/lib/planeradar/geocode-cache.json`, HTTP `0.0.0.0:80`, local URL
  `http://planeradar.local`, and Nominatim
  `https://nominatim.openstreetmap.org/search`.
- CLI flags have matching environment overrides
  `PLANERADAR_SETTINGS`, `PLANERADAR_GEOCODE_CACHE`, `PLANERADAR_HTTP`,
  `PLANERADAR_LOCAL_URL`, and `PLANERADAR_NOMINATIM_URL`.

- [ ] **Step 1: Write failing snapshot and SettingsService tests**

```rust
#[test]
fn settings_replace_is_persisted_then_published() {
    let fixture = runtime_fixture();
    let generation = fixture.model.snapshot().generation;
    fixture
        .settings_service
        .replace(configured_settings())
        .expect("replace");
    let snapshot = fixture.model.snapshot();
    assert_eq!(snapshot.settings, configured_settings());
    assert_eq!(snapshot.generation, generation + 1);
    assert_eq!(
        fixture.store.load().expect("load"),
        configured_settings()
    );
}
```

Assert a failed save does not publish, snapshots are immutable clones, and
health exposes state/revision but no location.

- [ ] **Step 2: Write failing polling/backoff tests**

Use fake HTTP, clock, and a controllable command receiver. Assert successful
fetch starts are at least three seconds apart; consecutive failures use
`3,6,12,24,30,30` seconds; success resets to three; location changes wake
immediately and discard old-center results; no location makes no request; stop
terminates promptly; transient failures retain last good aircraft; and stale
becomes true at exactly 30 seconds.

- [ ] **Step 3: Run focused tests to verify they fail**

```bash
mise exec -- cargo test --test network --test runtime
```

- [ ] **Step 4: Implement the narrow shared model**

```rust
#[derive(Clone)]
pub struct RuntimeSnapshot {
    pub settings: RadarSettings,
    pub aircraft: Arc<[Aircraft]>,
    pub fetched_at: Option<Duration>,
    pub last_error_at: Option<Duration>,
    pub local_url: String,
    pub ip_url: Option<String>,
    pub generation: u64,
}

pub struct RuntimeConfig {
    pub settings_path: PathBuf,
    pub geocode_cache_path: PathBuf,
    pub http_address: SocketAddr,
    pub local_url: String,
    pub nominatim_url: String,
}

pub enum WorkerCommand {
    SettingsChanged(RadarSettings),
    Stop,
}

pub struct RuntimeHandle {
    pub model: RuntimeModel,
    pub commands: Sender<WorkerCommand>,
    pub stop: Arc<AtomicBool>,
}
```

Wrap it in `Arc<RwLock<RuntimeSnapshot>>`. Hold locks only to clone or replace
fields. Network, disk, rendering, logging, and sleeps occur after releasing the
lock. `RuntimeHandle::shutdown(self) -> Result<(), RuntimeError>` signals and
joins both workers.

- [ ] **Step 5: Implement worker lifecycle and headless command**

Start the web worker before choosing runtime state. Keep the ADS-B worker alive
without location so a later web update wakes it through `WorkerCommand`.
Parse `/proc/net/route` to find the default-route interface, match that name to
a non-loopback IPv4 address returned by safe `nix::ifaddrs`, and expose
`http://<IP>`. Unit-test missing route, multiple addresses, loopback exclusion,
and fallback to the first non-loopback IPv4 address. SIGINT/SIGTERM set one
stop flag, wake workers, join them, and exit zero.

- [ ] **Step 6: Pass tests and commit**

```bash
mise run verify
but status
but diff
but commit rpi-port -m "feat: integrate headless radar services"
```

---

### Task 14: Hardware Checkpoint 3 — Web Setup and Live ADS-B

**Files:**

- No expected source changes; defects return to Tasks 3, 5, 11, 12, or 13 with
  a regression test.

**Interfaces:**

- Consumes: `planeradar run --headless`, HTTP `/`, `/healthz`, live Nominatim
  search, and live adsb.fi polling.
- Produces: verified phone configuration, persisted location, live aircraft,
  and privacy-safe logs on the real Pi.

- [ ] **Step 1: Build and deploy the exact committed artifact**

```bash
but status
mise run verify
mise run build-pi
mise run deploy-pi
mise run smoke-pi
stage="$(cat dist/last-stage-path)"
```

- [ ] **Step 2: Start the headless runtime on port 80**

```bash
ssh pi@raspberrypi.local \
  "sudo systemd-run --unit=planeradar-checkpoint3 --collect \
   --uid=pi \
   --property=AmbientCapabilities=CAP_NET_BIND_SERVICE \
   --setenv=RUST_LOG=info \
   '$stage/planeradar' run --headless \
   --settings '$stage/settings.json' \
   --geocode-cache '$stage/geocode-cache.json' \
   --http 0.0.0.0:80"
```

Confirm `curl http://planeradar.local/healthz` reports unconfigured without
coordinates.

- [ ] **Step 3: Configure from a phone**

Open `http://planeradar.local`, submit one explicit place search, verify
OpenStreetMap attribution/privacy text, select a result, save units/runway/range,
and confirm health becomes configured. Restart the foreground process and
confirm settings reload.

- [ ] **Step 4: Verify live data and log privacy**

Wait for at least one successful ADS-B poll or a valid empty response. Confirm
the reported aircraft count updates, transient failures preserve the last count,
and `journalctl -u planeradar-checkpoint3` contains no query, result label,
coordinates, token, or response body. Stop the transient unit with:

```bash
ssh pi@raspberrypi.local \
  'sudo systemctl stop planeradar-checkpoint3.service'
```

- [ ] **Step 5: Record the checkpoint**

Record revision/checksum, HTTP status, configuration persistence, ADS-B result,
and privacy scan. Any defect gets a failing test, focused fix, GitButler commit,
complete rebuild, and full checkpoint repetition.

**Hardware checkpoint 3:** do not integrate the full display loop until web
setup, persistence, and live ADS-B work on the target.

---

### Task 15: Integrate the Full Display Application and Runtime States

**Files:**

- Create: `src/app.rs`
- Create: `tests/app.rs`
- Modify: `src/display.rs`
- Modify: `src/runtime.rs`
- Modify: `src/cli.rs`
- Modify: `src/main.rs`
- Modify: `src/lib.rs`

**Interfaces:**

- Consumes: `run_display`, `GestureRecognizer`, `RadarRenderer`,
  `SetupRenderer`, and `RuntimeCoordinator`.
- Produces:
  `PlaneRadarApp::new(runtime: RuntimeHandle, radar: RadarRenderer, setup: SetupRenderer)`.
- Produces:
  `state(&self) -> AppState`, `settings(&self) -> &RadarSettings`, and
  `handle_gesture(&mut self, gesture: Gesture) -> Result<(), AppError>`.
- Produces:
  `DisplayHandler for PlaneRadarApp`.
- Produces runtime states
  `SETUP_REQUIRED`, `WAITING_FOR_NETWORK`, `RADAR`, and `SETTINGS`.
- `SIGUSR1` saves the current logical frame to
  `/var/lib/planeradar/debug.png`; `--debug-frame <path>` overrides it for
  checkpoint runs.

- [ ] **Step 1: Write failing state-transition tests**

```rust
#[test]
fn tap_cycles_range_and_long_press_opens_settings() {
    let mut app = app_fixture(configured_snapshot());
    assert_eq!(app.state(), AppState::Radar);
    app.handle_gesture(Gesture::Tap).expect("tap");
    assert_eq!(app.settings().range_index, 2);
    app.handle_gesture(Gesture::LongPress).expect("hold");
    assert_eq!(app.state(), AppState::Settings);
}
```

Add named tests:

- missing location → `SetupRequired`;
- configured without IP/data → `WaitingForNetwork`;
- first valid ADS-B response → `Radar`;
- tap in configured settings → `Radar`;
- tap in setup-required state remains setup-required;
- long-press release cannot cycle range;
- settings change invalidates background and poll center;
- transient ADS-B error retains radar;
- loss of the current IP after a successful fetch retains radar;
- 30 seconds stale draws notice;
- fresh data clears notice; and
- display initialization error returns non-zero.

- [ ] **Step 2: Write failing signal/debug tests**

Assert SIGTERM requests coordinated shutdown, SIGUSR1 writes exactly one
480×480 PNG through `Frame::save_png`, and a failed debug write logs one error
without stopping the radar.

- [ ] **Step 3: Run focused tests to verify they fail**

```bash
mise exec -- cargo test --test app
```

- [ ] **Step 4: Implement state selection and gesture actions**

Select state from immutable snapshots:

```rust
match (settings.location.is_some(), settings_open, has_success) {
    (false, _, _) => AppState::SetupRequired,
    (true, true, _) => AppState::Settings,
    (true, false, true) => AppState::Radar,
    (true, false, false) => AppState::WaitingForNetwork,
}
```

An explicit long press overrides configured radar with `Settings`; saving
location returns to radar/waiting automatically. Tap persists the next range in
radar, closes configured settings, and does nothing in required setup.
`WaitingForNetwork` uses `SetupRenderer` with the stable local URL, any current
numeric URL, and `WAITING FOR NETWORK`; it never invokes network configuration.

- [ ] **Step 5: Implement event/render lifecycle**

Each `DisplayHandler::step` feeds normalized events into `GestureRecognizer`,
observes runtime generation, state, stale boundary, and signal flags, and
returns a new complete frame only when content changes. Missing touch logs one
warning but does not stop display/web. Shutdown stops workers before SDL drops.

- [ ] **Step 6: Pass tests and commit**

```bash
mise run verify
but status
but diff
but commit rpi-port -m "feat: integrate Plane Radar display app"
```

---

### Task 16: Hardware Checkpoint 4 — Touch, States, and Recovery

**Files:**

- No expected source changes; defects return to Tasks 2, 8, 10, 13, or 15 with
  a regression test.

**Interfaces:**

- Consumes: default `planeradar run`, physical touch, runtime workers, and
  SIGUSR1.
- Produces: verified state/gesture semantics, stale recovery, and matching
  physical/debug orientation.

- [ ] **Step 1: Build and run the exact artifact**

```bash
but status
mise run verify
mise run build-pi
mise run deploy-pi
stage="$(cat dist/last-stage-path)"
ssh pi@raspberrypi.local \
  "sudo systemd-run --unit=planeradar-checkpoint4 --collect \
   --uid=pi \
   --property=AmbientCapabilities=CAP_NET_BIND_SERVICE \
   --setenv=SDL_VIDEODRIVER=kmsdrm \
   --setenv=RUST_LOG=info \
   '$stage/planeradar' run \
   --settings '$stage/settings.json' \
   --geocode-cache '$stage/geocode-cache.json' \
   --debug-frame '$stage/debug.png' \
   --http 0.0.0.0:80"
```

- [ ] **Step 2: Verify gestures exactly**

On the screen: tap once and confirm one range advance; hold continuously for
three seconds and confirm QR appears before release; release and confirm it
stays; tap once and confirm radar returns without another range change. Restart
and confirm the selected range persists.

- [ ] **Step 3: Verify setup-required blocking**

Move the staged settings to a recoverable sibling name, start again, and confirm
the QR cannot be dismissed by tap. Configure through the web page and confirm
the display changes to waiting/radar without restarting.

- [ ] **Step 4: Verify stale and network recovery**

Before disabling networking, schedule restoration:

```bash
ssh pi@raspberrypi.local \
  'sudo systemd-run --unit=planeradar-network-restore \
     --on-active=45s /usr/bin/nmcli networking on && \
   sudo nmcli networking off'
```

Confirm last aircraft remain, `DATA STALE` appears at 30 seconds, SSH/network
returns automatically, and fresh data clears the notice.

- [ ] **Step 5: Capture the real logical frame**

Send SIGUSR1 with
`sudo systemctl kill -s SIGUSR1 planeradar-checkpoint4`, copy the debug PNG,
and inspect at original size. Confirm the capture orientation and layout match
the physical panel and approved golden:

```bash
scp "pi@raspberrypi.local:$stage/debug.png" /tmp/planeradar-debug.png
ssh pi@raspberrypi.local \
  'sudo systemctl stop planeradar-checkpoint4.service'
```

- [ ] **Step 6: Record the checkpoint**

Record all state transitions, gesture outcomes, recovery timing, screenshot,
revision, and checksum. Apply the regression-test/fix/rebuild loop for any
failure.

**Hardware checkpoint 4:** do not install the boot service until all runtime
states and gestures pass on the real panel.

---

### Task 17: Install Idempotently and Run as a Hardened Service

**Files:**

- Create: `packaging/planeradar.service`
- Create: `tests/install.rs`
- Modify: `src/install.rs`
- Modify: `src/cli.rs`
- Modify: `src/main.rs`
- Modify: `packaging/Dockerfile.build`
- Modify: `scripts/deploy-pi.sh`

**Interfaces:**

- Consumes: current ARM64 binary, checksum, revision, calibrated overlay, and
  systemd unit embedded with `include_str!`.
- Produces:
  `InstallOptions`, `InstallResult { files_changed, boot_config_changed, reboot_required }`,
  `CommandRunner`, and `Installer::install()`.
- Produces:
  `planeradar install --artifact <path> --checksum-file <path> --revision-file <path>`.
- Installs binary `/opt/planeradar/bin/planeradar`, revision/checksum files,
  settings directory `/var/lib/planeradar`, and
  `/etc/systemd/system/planeradar.service`.

- [ ] **Step 1: Write failing installation/idempotence tests**

Use a temporary fake root and recording `CommandRunner`. Assert supported Pi OS
checking, ARM64 artifact/checksum/revision verification, exact apt packages
`libsdl2-2.0-0 ca-certificates avahi-daemon`, service user creation,
supplementary groups `video render input`, directory mode `0750`, one overlay,
one backup, service copy/enable, and reboot only when boot config changes.
Run twice and assert the second result has all change/reboot flags false.

Use these exact public shapes:

```rust
pub struct InstallOptions {
    pub root: PathBuf,
    pub boot_config: PathBuf,
    pub artifact: PathBuf,
    pub checksum_file: PathBuf,
    pub revision_file: PathBuf,
    pub reboot: bool,
}

pub trait CommandRunner: Send + Sync {
    fn run(&self, program: &str, args: &[&str]) -> Result<(), InstallError>;
}

pub struct InstallResult {
    pub files_changed: bool,
    pub boot_config_changed: bool,
    pub reboot_required: bool,
}
```

- [ ] **Step 2: Write failing service-unit tests**

Assert these exact directives:

```ini
[Unit]
After=network-online.target
Wants=network-online.target

[Service]
User=planeradar
SupplementaryGroups=video render input
WorkingDirectory=/opt/planeradar
ExecStart=/opt/planeradar/bin/planeradar run
Restart=on-failure
RestartSec=3
AmbientCapabilities=CAP_NET_BIND_SERVICE
CapabilityBoundingSet=CAP_NET_BIND_SERVICE
NoNewPrivileges=true
ProtectSystem=strict
ProtectHome=true
PrivateTmp=true
PrivateDevices=false
DevicePolicy=closed
ReadWritePaths=/var/lib/planeradar
StateDirectory=planeradar
UMask=0027
```

Also require:

```ini
Environment=SDL_VIDEODRIVER=kmsdrm
RestrictAddressFamilies=AF_UNIX AF_INET AF_INET6 AF_NETLINK
DeviceAllow=/dev/dri/card* rw
DeviceAllow=/dev/dri/renderD* rw
DeviceAllow=/dev/input/event* r
```

- [ ] **Step 3: Run focused tests to verify they fail**

```bash
mise exec -- cargo test --test install
```

- [ ] **Step 4: Implement verified installation**

Verify SHA-256 before any write and require the revision file to equal
`env!("PLANERADAR_REVISION")`. Use injectable commands for apt, user/group,
ownership, daemon reload, enable, and reboot. Copy the artifact atomically with
mode `0755`; write revision/checksum with `0644`; create state directory `0750`.
Preserve the Task 2 boot backup and calibrated declaration.

- [ ] **Step 5: Pass tests and commit**

```bash
mise run verify
but status
but diff
but commit rpi-port -m "feat: install Plane Radar system service"
```

- [ ] **Step 6: Cross-build, install, and verify systemd**

```bash
mise run build-pi
mise run deploy-pi
stage="$(cat dist/last-stage-path)"
ssh -t pi@raspberrypi.local \
  "sudo '$stage/planeradar' install \
   --artifact '$stage/planeradar' \
   --checksum-file '$stage/planeradar.sha256' \
   --revision-file '$stage/planeradar.revision'"
```

Reboot only if reported. Then:

```bash
ssh pi@raspberrypi.local \
  'systemctl is-enabled planeradar; \
   systemctl is-active planeradar; \
   systemctl show planeradar \
     -p User -p SupplementaryGroups -p AmbientCapabilities -p MainPID; \
   sudo journalctl -u planeradar -b --no-pager -n 100'
```

- [ ] **Step 7: Verify idempotence and cold boot**

Run the installer again and assert no reboot/change. Reboot once. Confirm KMS
480×480, service active, HTTP health, persisted settings, radar display, touch,
and live polling without login. Restart the service and confirm recovery within
the configured three-second delay.

**Hardware checkpoint 5:** systemd, permissions, cold boot, and installer
idempotence must pass before product cleanup and publication.

---

### Task 18: Complete Product Documentation, Cleanup, and Final Acceptance

**Files:**

- Rewrite: `README.md`
- Create: `docs/architecture.md`
- Create: `docs/troubleshooting.md`
- Create: `docs/images/radar.png`
- Create: `docs/images/setup.png`
- Delete: `data/ui_font.vlw`
- Delete: `include/config.h`
- Delete: `include/hardware/display.h`
- Delete: `include/hardware/display_font.h`
- Delete: `include/hardware/lgfx_config.hpp`
- Delete: `include/services/adsb_client.h`
- Delete: `include/services/radar_location.h`
- Delete: `include/services/wifi_setup.h`
- Delete: `include/ui/radar_display.h`
- Delete: `include/ui/radar_range.h`
- Delete: `include/ui/radar_theme.h`
- Delete: `include/ui/runway_overlay.h`
- Delete: `include/ui/status_screens.h`
- Delete: `partitions/plane_radar.csv`
- Delete: `platformio.ini`
- Delete: `scripts/merge-firmware.sh`
- Delete: `scripts/merge_firmware.py`
- Delete: `src/hardware/display.cpp`
- Delete: `src/hardware/display_font.cpp`
- Delete: `src/main.cpp`
- Delete: `src/services/adsb_client.cpp`
- Delete: `src/services/radar_location.cpp`
- Delete: `src/services/wifi_setup.cpp`
- Delete: `src/ui/radar_display.cpp`
- Delete: `src/ui/radar_range.cpp`
- Delete: `src/ui/runway_overlay.cpp`
- Delete: `src/ui/status_screens.cpp`

**Interfaces:**

- Consumes: all accepted runtime/install behavior and deterministic goldens.
- Produces public operator docs, provenance, screenshots, CI badge, architecture
  and troubleshooting references, plus the final exact accepted binary.

- [ ] **Step 1: Add deterministic public screenshots**

Copy approved non-location-specific radar/setup goldens to `docs/images`.
README opens with the radar image and links the setup image.

- [ ] **Step 2: Rewrite README with operator workflows**

Document supported hardware/OS, `mise install`, native checks, ARM64 build,
deployment, installation, first-run QR, place/manual location, gestures,
updates, logs, debug capture, uninstall, and service paths. State that the app
never manages Wi-Fi. Credit and link `MatixYo/ESP32-Plane-Radar`, explain the
independent derivative repository, retain MIT, and include DejaVu attribution.

- [ ] **Step 3: Write architecture and troubleshooting docs**

Architecture covers thread ownership, immutable snapshots, SDL boundary,
renderer cache, settings schema, Nominatim/ADS-B policy, artifact provenance,
and five Pi checkpoints. Troubleshooting covers Docker/OrbStack preflight,
blank/wrongly rotated display, touch axes, `.local`, port 80, stale data,
geocoder failure, invalid settings recovery, service permissions, and journal
privacy.

- [ ] **Step 4: Remove the superseded ESP32 build surface**

Delete only the files listed in this task. Preserve upstream history, LICENSE,
Rust sources, assets/licenses, tests, mise/container files, installer, specs,
plan, and new docs.

- [ ] **Step 5: Scan for stale or incomplete content**

```bash
rg -n 'PlatformIO|ESP32-C3|WiFiManager|web flasher|firmware\\.bin|LovyanGFX' \
  README.md docs src scripts packaging Cargo.toml mise.toml .github tests
rg -n 'TO[D]O|TB[D]|FIX[M]E|coming[[:space:]]soon' \
  README.md docs src scripts packaging Cargo.toml mise.toml .github tests
```

Expected: the first command returns only deliberate provenance discussion; the
second returns nothing.

- [ ] **Step 6: Run full checks and commit**

```bash
mise run verify
but status
but diff
but commit rpi-port -m "docs: complete Raspberry Pi product surface"
```

Build only after the commit so `build-pi` sees a clean workspace and embeds the
new revision:

```bash
mise run build-pi
mise run deploy-pi
```

- [ ] **Step 7: Install and verify the exact final revision**

Run the Task 17 installer from the new stage. Assert:

```bash
git rev-parse rpi-port
cat dist/planeradar.revision
ssh pi@raspberrypi.local \
  'cat /opt/planeradar/REVISION; \
   sha256sum /opt/planeradar/bin/planeradar; \
   systemctl is-active planeradar'
```

All three revisions match and the installed checksum matches
`dist/planeradar.sha256`.

- [ ] **Step 8: Repeat final physical acceptance**

Verify setup QR/URLs, phone search/manual entry, persistence, live radar,
runways, tap, three-second hold/release, debug PNG, service restart, scheduled
network interruption/recovery, and cold boot. Inspect journal for panics,
location/query/token leakage, or repeated permission failures.

No publication occurs until every check passes on this exact revision.

---

### Task 19: Publish the Independent Public Repository

**Files:**

- No product-file changes expected.
- Remotes become
  `origin=github.com/shayne/RPi-Plane-Radar` and
  `upstream=github.com/MatixYo/ESP32-Plane-Radar`.

**Interfaces:**

- Consumes: clean accepted `rpi-port` SHA, matching installed revision/checksum,
  green local verification, and user authorization already recorded.
- Produces public independent MIT repository with default branch `main`, green
  CI, preserved upstream history/remote, and exact Pi revision.

- [ ] **Step 1: Reconfirm publication preconditions**

```bash
but status
but diff
mise run verify
git rev-parse rpi-port
cat dist/planeradar.revision
ssh pi@raspberrypi.local \
  'cat /opt/planeradar/REVISION; systemctl is-active planeradar'
```

Expected: clean workspace, all revisions equal, service active.

- [ ] **Step 2: Create the public repository without initialization**

```bash
gh repo view shayne/RPi-Plane-Radar
```

Expected before creation: not found. Then:

```bash
gh api --method POST /user/repos \
  -f name=RPi-Plane-Radar \
  -F private=false \
  -f description='A Raspberry Pi Zero 2 W ADS-B radar for the HyperPixel 2.1 Round'
```

- [ ] **Step 3: Configure origin/upstream and GitButler push remote**

```bash
git remote rename origin upstream
git remote add origin git@github.com:shayne/RPi-Plane-Radar.git
but config push-remote origin
git remote -v
but push rpi-port
```

Remote-add/rename is the only Git write here because GitButler has no equivalent;
the content push remains GitButler-owned.

- [ ] **Step 4: Establish the accepted SHA as public `main`**

Read the pushed SHA with `git ls-remote origin refs/heads/rpi-port`. Create
`refs/heads/main` at that exact SHA:

```bash
gh api --method POST \
  repos/shayne/RPi-Plane-Radar/git/refs \
  -f ref=refs/heads/main \
  -f sha="$(git rev-parse rpi-port)"
gh api --method PATCH repos/shayne/RPi-Plane-Radar \
  -f default_branch=main
git fetch origin main
but config target origin/main --push-remote origin
```

After `origin/main` equality is proven, remove the temporary public `rpi-port`
ref through the GitHub API.

- [ ] **Step 5: Verify metadata, history, artifact revision, and CI**

```bash
gh repo view shayne/RPi-Plane-Radar \
  --json nameWithOwner,isPrivate,defaultBranchRef,licenseInfo,url
git ls-remote origin refs/heads/main
git ls-remote upstream refs/heads/main
gh run list --repo shayne/RPi-Plane-Radar --branch main --limit 1
gh run watch --repo shayne/RPi-Plane-Radar --exit-status
curl --fail --silent http://planeradar.local/healthz
```

Assert public visibility, `main`, MIT, expected upstream, green CI, README
screenshots, health without coordinates, and `origin/main` equal to the
installed revision.

- [ ] **Step 6: Final handoff**

Report the public URL, published/installed SHA, binary SHA-256, CI URL, service
state, cold-boot result, display/touch result, and any explicitly accepted
hardware limitation.

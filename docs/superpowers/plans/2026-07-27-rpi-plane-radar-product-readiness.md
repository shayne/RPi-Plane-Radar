# RPi Plane Radar Product Readiness Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Turn the accepted Raspberry Pi application and HyperPixel prototype into two public, versioned repositories that a Mac user can install, upgrade, diagnose, roll back, and remove end to end.

**Architecture:** `shayne/hyperpixel2r-kms` owns the generic GPL kernel driver, exact-kernel artifacts, and safe tryboot transaction. `shayne/RPi-Plane-Radar` pins a driver release and adds a Rust macOS control tool, `planeradarctl`, which verifies releases and orchestrates the target over OpenSSH. GitHub Actions produces attested release assets; stable publication remains gated by clean-image and physical-hardware acceptance.

**Tech Stack:** Rust 1.97.1, C/Linux DRM and DKMS, device tree, Bash, mise, Docker Buildx/OrbStack, OpenSSH, Raspberry Pi OS Lite Trixie ARM64, GitHub Actions/Releases/attestations, GitButler.

## Global Constraints

- Supported target is exactly Raspberry Pi Zero 2 W, HyperPixel 2.1 Round, and 64-bit Raspberry Pi OS **Lite** Trixie.
- Wi-Fi and initial SSH provisioning are prerequisites, not installer features.
- The public installation host is macOS with Git, mise, OpenSSH, and Docker Desktop, OrbStack, or equivalent Buildx support.
- The public happy path is `mise install` followed by `mise run install -- user@host`; no manual Pi-side command is allowed.
- Default desired hostname is `planeradar`; SSH username and current host are inputs, with `pi@raspberrypi.local` only a suggestion.
- Maintainer-specific values live only in ignored `.env`; `.env` is optional and `.env.example` is public.
- CLI arguments override `.env`, which overrides generic defaults.
- Plane Radar is MIT; the extracted driver is GPL-2.0-only and preserves upstream notices.
- Plane Radar pins an immutable driver release by repository, semantic version, full commit, and manifest digest; no submodule and no vendored mutable copy.
- CI-built artifacts are preferred; an exact target-kernel Docker cross-build is the fallback.
- Driver activation uses one-shot tryboot and is committed only after automated verification.
- Application updates are explicit and atomic; arbitrary OS/kernel upgrades are not claimed to be unattended-safe.
- All Codex-assisted commits end exactly once with `Co-authored-by: Codex <noreply@openai.com>`.
- Use GitButler for every commit, history edit, and push. Read-only Git commands remain allowed.
- Apply `$plainspoken-voice` only while writing the public README and human-facing documentation, never to this plan or agent instructions.
- Run app checks with `mise run verify`; run driver checks with the driver repository's `mise run verify`.
- Do not publish a stable release until the rewritten source revision passes physical and clean-room acceptance.

## Repository and file map

### `RPi-Plane-Radar`

| Path | Responsibility |
|---|---|
| `Cargo.toml` | Existing application package plus workspace membership for `planeradarctl` |
| `crates/planeradarctl/Cargo.toml` | macOS control-tool dependencies, isolated from SDL |
| `crates/planeradarctl/src/cli.rs` | Public command and argument definitions |
| `crates/planeradarctl/src/config.rs` | CLI, `.env`, and generic-default precedence |
| `crates/planeradarctl/src/target.rs` | SSH target parsing, host identity, and probe schema |
| `crates/planeradarctl/src/transport.rs` | OpenSSH/scp execution and testable command runner |
| `crates/planeradarctl/src/state.rs` | Durable transaction schema and atomic state store |
| `crates/planeradarctl/src/release.rs` | Plane Radar and driver manifest parsing and integrity checks |
| `crates/planeradarctl/src/preflight.rs` | Mac and target requirement evaluation |
| `crates/planeradarctl/src/driver.rs` | Locked driver resolution and driver-tool invocation |
| `crates/planeradarctl/src/install.rs` | Resumable install state machine |
| `crates/planeradarctl/src/operations.rs` | Status, doctor, screenshot, upgrade, rollback, uninstall |
| `crates/planeradarctl/src/main.rs` | CLI dispatch and exit reporting |
| `driver.lock.toml` | Exact external driver release dependency |
| `release/release-manifest.schema.json` | Plane Radar release-manifest contract |
| `scripts/package-release.sh` | Deterministic app/control-tool release bundle generation |
| `scripts/install.sh` | Thin release bootstrap that verifies and launches `planeradarctl` |
| `.github/workflows/ci.yml` | App and control-tool verification |
| `.github/workflows/release.yml` | Draft release assets, SBOMs, and attestations |
| `tests/ctl_*.rs` | Cross-process control-tool contract tests where unit tests are insufficient |
| `tests/release_contract.rs` | Plane Radar release assets and workflow contract |
| `.env.example`, `.gitignore`, `mise.toml` | Public defaults, ignored maintainer overrides, task surface |
| `README.md`, `docs/*.md` | Public installation, operations, architecture, recovery, and disclosure |
| `docs/images/planeradar-radar.png` | Accepted 480×480 frame captured from the physical Pi path |

### `hyperpixel2r-kms`

| Path | Responsibility |
|---|---|
| `kernel/hyperpixel2r_kms_main.c` | Generic DRM panel and shared touch adapter |
| `kernel/hyperpixel2r_kms_gpio.[ch]` | GPIO transport and cleanup |
| `kernel/hyperpixel2r_kms_protocol.[ch]` | Fixed HyperPixel command stream |
| `kernel/Kbuild`, `kernel/Makefile`, `kernel/dkms.conf` | Kernel and DKMS builds |
| `overlays/hyperpixel2r-kms-overlay.dts` | Panel, DPI, and touch device tree |
| `tests/protocol_test.c`, `tests/gpio_test.c` | Header-free host unit tests |
| `scripts/export-target-kbuild.sh` | Exact target kernel build-context export |
| `scripts/build-driver.sh` | Docker cross-build and artifact manifest |
| `scripts/check-artifacts.sh` | Module, overlay, DTB, checksum, and provenance validation |
| `scripts/stage-tryboot.sh` | Versioned target staging and one-shot boot |
| `scripts/verify-boot.sh` | DRM, mode, module, touch, SDL, and GLES verification |
| `scripts/commit-boot.sh`, `scripts/rollback-boot.sh` | Accept or restore owned boot state |
| `scripts/uninstall.sh` | Remove only driver-owned state |
| `release/driver-manifest.schema.json` | Driver release contract |
| `tests/build-contract.sh`, `tests/boot-scripts.sh`, `tests/release-contract.sh` | Shell interface and packaging contracts |
| `.github/workflows/ci.yml`, `.github/workflows/release.yml` | Driver verification and attested assets |
| `mise.toml`, `.env.example`, `.gitignore` | Reproducible driver development environment |
| `README.md`, `docs/compatibility.md`, `docs/provenance.md` | Public driver use, support matrix, and source notices |

---

## Phase 1: Close application correctness and configuration gaps

### Task 1: Make maintainer configuration local and complete runtime dependencies

**Files:**
- Create: `.env.example`
- Modify: `.gitignore`
- Modify: `mise.toml`
- Modify: `src/install.rs`
- Modify: `tests/install.rs`
- Modify: `tests/deploy_scripts.rs`

**Interfaces:**
- Consumes: existing `Installer`, `CommandRunner`, and mise task names.
- Produces: optional `.env` loading; `PLANERADAR_PI_TARGET`, `PLANERADAR_HOSTNAME`, and `PLANERADAR_DOCKER_CONTEXT`; complete `RUNTIME_PACKAGES`.

- [ ] **Step 1: Add failing tests for public defaults and package completeness**

Add assertions that every tracked shell script lacks a maintainer-specific SSH
target and that installation invokes APT with the exact runtime package set:

```rust
const EXPECTED_PACKAGES: &[&str] = &[
    "libsdl2-2.0-0",
    "libegl1",
    "libgles2",
    "libgl1-mesa-dri",
    "ca-certificates",
    "avahi-daemon",
];

#[test]
fn installer_declares_every_graphics_runtime_package() {
    let fixture = Fixture::new("[all]\n");
    let runner = RecordingRunner::for_root(&fixture.root);
    Installer::new(&runner)
        .install(&fixture.options(false))
        .expect("install");
    let install = runner
        .commands()
        .into_iter()
        .find(|(program, args)| program == "apt-get" && args.first().is_some_and(|v| v == "install"))
        .expect("apt install");
    for package in EXPECTED_PACKAGES {
        assert!(install.1.iter().any(|value| value == package), "missing {package}");
    }
}
```

Extend `tests/deploy_scripts.rs` to scan `scripts/*.sh` and reject any
hard-coded user-at-host token except the documented public suggestion
`pi@raspberrypi.local`.

- [ ] **Step 2: Run the focused tests and observe failure**

Run:

```bash
mise exec -- cargo test --test install installer_declares_every_graphics_runtime_package
mise exec -- cargo test --test deploy_scripts
```

Expected: package test reports missing EGL/GLES/Mesa packages; script test
reports current hard-coded targets.

- [ ] **Step 3: Add the optional environment contract**

Add:

```dotenv
# .env.example
PLANERADAR_PI_TARGET=pi@raspberrypi.local
PLANERADAR_HOSTNAME=planeradar
PLANERADAR_DOCKER_CONTEXT=
```

Add `.env` to `.gitignore`. Use mise's optional dotenv file form:

```toml
[env]
_.file = { path = ".env", redact = true }
```

If the installed mise version does not treat a missing `path` as optional,
leave dotenv parsing to `planeradarctl` and do not add `env._.file`. In either
case, `mise env` must succeed both without `.env` and with a copied
`.env.example`; that behavior is the contract.

Replace shell defaults with:

```bash
target="${1:-${PLANERADAR_PI_TARGET:-pi@raspberrypi.local}}"
```

This is a generic convenience default, not the maintainer's live target.

- [ ] **Step 4: Declare the runtime packages once**

In `src/install.rs`, define:

```rust
const RUNTIME_PACKAGES: &[&str] = &[
    "libsdl2-2.0-0",
    "libegl1",
    "libgles2",
    "libgl1-mesa-dri",
    "ca-certificates",
    "avahi-daemon",
];
```

Build the `apt-get install --no-install-recommends` arguments from this
constant so tests and production share the same order.

- [ ] **Step 5: Run focused and full verification**

Run:

```bash
mise exec -- cargo test --test install
mise exec -- cargo test --test deploy_scripts
mise run verify
```

Expected: all pass with no tracked maintainer target.

- [ ] **Step 6: Commit**

```bash
but commit product-readiness -m $'build: localize maintainer configuration\n\nCo-authored-by: Codex <noreply@openai.com>'
```

### Task 2: Fix deferred installer and HTTP boundary errors

**Files:**
- Modify: `src/install.rs`
- Modify: `src/http.rs`
- Test: `tests/install.rs`

**Interfaces:**
- Consumes: `InstallError`, `HttpError`, `UreqHttpClient`.
- Produces: `lock_path(&Path) -> Result<PathBuf, InstallError>` and `HttpError::InvalidTimeout`.

- [ ] **Step 1: Add failing regression tests**

In `src/install.rs` unit tests:

```rust
#[test]
fn lock_path_rejects_a_path_without_a_file_name() {
    assert_eq!(
        lock_path(Path::new("/")),
        Err(InstallError::MissingParent(PathBuf::from("/")))
    );
}
```

In `src/http.rs`:

```rust
#[test]
fn timeout_sum_overflow_is_rejected() {
    let mut request = request();
    request.connect_timeout = Duration::MAX;
    request.read_timeout = Duration::from_nanos(1);
    assert_eq!(build_agent(&request), Err(HttpError::InvalidTimeout));
}
```

- [ ] **Step 2: Run the tests to prove both failures**

Run:

```bash
mise exec -- cargo test lock_path_rejects_a_path_without_a_file_name
mise exec -- cargo test timeout_sum_overflow_is_rejected
```

Expected: the lock test panics and the HTTP test cannot compare the current
infallible return.

- [ ] **Step 3: Make lock creation fallible**

Change:

```rust
fn lock_path(path: &Path) -> Result<PathBuf, InstallError> {
    let name = path
        .file_name()
        .ok_or_else(|| InstallError::MissingParent(path.to_owned()))?;
    let mut lock_name = name.to_os_string();
    lock_name.push(".planeradar-lock");
    Ok(path.with_file_name(lock_name))
}
```

Propagate `lock_path(path)?` from `open_lock_file`.

- [ ] **Step 4: Reject unrepresentable global timeouts**

Add:

```rust
#[error("HTTP timeout values exceed the supported duration")]
InvalidTimeout,
```

Make `build_agent` return `Result<ureq::Agent, HttpError>` and use:

```rust
let global_timeout = request
    .connect_timeout
    .checked_add(request.read_timeout)
    .ok_or(HttpError::InvalidTimeout)?;
```

Propagate the result from `execute`.

- [ ] **Step 5: Run all verification**

Run:

```bash
mise run verify
```

Expected: all tests, Clippy, formatting, and dependency policy pass.

- [ ] **Step 6: Commit**

```bash
but commit product-readiness -m $'fix: reject invalid installer and timeout paths\n\nCo-authored-by: Codex <noreply@openai.com>'
```

### Task 3: Derive the local URL from the target hostname

**Files:**
- Modify: `src/cli.rs`
- Modify: `src/main.rs`
- Modify: `src/network.rs`
- Modify: `tests/cli.rs`
- Modify: `tests/network.rs`
- Modify: `tests/web.rs`
- Modify: `docs/architecture.md`

**Interfaces:**
- Consumes: `Command::Run`, `RuntimeConfig.local_url`.
- Produces: `local_url(hostname: &str) -> Result<String, HostnameError>` and optional `--local-url` override.

- [ ] **Step 1: Add hostname validation tests**

Add:

```rust
#[test]
fn builds_local_url_from_a_valid_hostname() {
    assert_eq!(local_url("planeradar").unwrap(), "http://planeradar.local");
    assert_eq!(local_url("hangar-2").unwrap(), "http://hangar-2.local");
}

#[test]
fn rejects_hostname_text_that_could_change_the_authority() {
    for value in ["", ".local", "radar.local", "radar/evil", "radar:80", "-radar"] {
        assert!(local_url(value).is_err(), "{value}");
    }
}
```

Update the CLI test to assert `local_url: None` when neither flag nor
environment override is supplied.

- [ ] **Step 2: Run focused tests and observe failure**

Run:

```bash
mise exec -- cargo test --test network
mise exec -- cargo test --test cli
```

Expected: `local_url` is missing and the CLI still injects
`http://planeradar.local`.

- [ ] **Step 3: Implement hostname-derived URL selection**

Change the CLI field to:

```rust
#[arg(long, env = "PLANERADAR_LOCAL_URL")]
local_url: Option<String>,
```

Implement strict ASCII hostname-label validation in `network.rs`. In `main.rs`,
resolve:

```rust
let local_url = match local_url {
    Some(url) => url,
    None => {
        let hostname = std::fs::read_to_string("/etc/hostname")?;
        planeradar::network::local_url(hostname.trim())?
    }
};
```

- [ ] **Step 4: Update web authority tests**

Construct the web fixture with `http://hangar-2.local` and assert that
`Host: hangar-2.local` succeeds while `Host: planeradar.local` is rejected.

- [ ] **Step 5: Run verification**

Run:

```bash
mise run verify
```

Expected: all tests pass and architecture text says the URL follows the
installed hostname.

- [ ] **Step 6: Commit**

```bash
but commit product-readiness -m $'fix: derive setup URL from hostname\n\nCo-authored-by: Codex <noreply@openai.com>'
```

## Phase 2: Extract and release the HyperPixel driver

### Task 4: Create the generic driver repository and import the accepted source

**Files:**
- Create repository: `/Users/shayne/code/hyperpixel2r-kms`
- Create: driver paths listed in the repository map
- Source from: `RPi-Plane-Radar/kernel/*`

**Interfaces:**
- Consumes: accepted `planeradar_hyperpixel2r` module and host tests.
- Produces: `hyperpixel2r_kms.ko`, `hyperpixel2r-kms-overlay.dts`, DKMS package `hyperpixel2r-kms/0.1.0`.

- [ ] **Step 1: Create an empty public repository and initialize GitButler**

Create `shayne/hyperpixel2r-kms` as an empty public repository, without a
generated README, license, or `.gitignore`, and clone it to
`/Users/shayne/code/hyperpixel2r-kms`. Run `but setup --init`; its generated
empty root commit is the target base. Configure the eventual `origin/main`
push remote and create branch `driver-v0.1`. Add the complete GPL-2.0-only
`LICENSE` in the import change rather than allowing GitHub to create a separate
content commit.

Expected: the repository is public, the root commit is GitButler's empty
bootstrap, and the first non-empty commit is the clean Codex-authored import.

- [ ] **Step 2: Add host tests before the implementation**

Move the existing tests to `tests/` and update includes to:

```c
#include "../kernel/hyperpixel2r_kms_gpio.h"
#include "../kernel/hyperpixel2r_kms_protocol.h"
```

Create mise tasks `test-protocol` and `test-gpio` that compile with:

```bash
cc -std=c11 -Wall -Wextra -Werror -pedantic \
  kernel/hyperpixel2r_kms_protocol.c tests/protocol_test.c \
  -o target/protocol-test
```

and the corresponding GPIO sources.

- [ ] **Step 3: Run tests and observe missing generic sources**

Run:

```bash
mise run test-protocol
mise run test-gpio
```

Expected: compilation fails because the generic files do not exist.

- [ ] **Step 4: Import and rename the accepted implementation**

Use `apply_patch` moves and make these exact identity changes:

```text
planeradar_hyperpixel2r_*      -> hyperpixel2r_kms_*
planeradar-hyperpixel2r        -> hyperpixel2r-kms
struct planeradar_hyperpixel2r -> struct hyperpixel2r_kms
planeradar,hyperpixel2r        -> shayne,hyperpixel2r-kms
```

Retain every `SPDX-License-Identifier: GPL-2.0-only` line, the Amarula/Jagan
Teki notice, and the exact Raspberry Pi kernel provenance commit.

Set DKMS:

```bash
PACKAGE_NAME="hyperpixel2r-kms"
PACKAGE_VERSION="0.1.0"
BUILT_MODULE_NAME[0]="hyperpixel2r_kms"
DEST_MODULE_LOCATION[0]="/extra"
AUTOINSTALL="yes"
```

- [ ] **Step 5: Prove generic naming and tests**

Run:

```bash
mise run test-protocol
mise run test-gpio
! rg -n 'planeradar|Plane Radar' kernel overlays tests
```

Expected: both tests pass and the identity scan returns no matches.

- [ ] **Step 6: Commit**

```bash
but commit driver-v0.1 -m $'feat: import HyperPixel KMS driver\n\nCo-authored-by: Codex <noreply@openai.com>'
```

### Task 5: Move exact-kernel build and artifact validation into the driver repo

**Files:**
- Create: `scripts/export-target-kbuild.sh`
- Create: `scripts/build-driver.sh`
- Create: `scripts/check-artifacts.sh`
- Create: `scripts/prepare-kbuild-host-tools.sh`
- Create: `scripts/common.sh`
- Create: `packaging/Dockerfile.kernel`
- Create: `tests/build-contract.sh`
- Modify: `mise.toml`

**Interfaces:**
- Consumes: `PLANERADAR_PI_TARGET` only as a compatibility input during migration.
- Produces: explicit `HP2R_TARGET`; artifact directory with `manifest.txt`, module, overlay, applied DTB, hashes, modinfo, and readelf output.

- [ ] **Step 1: Add a failing public-interface contract test**

Create `tests/build-contract.sh`. It reads all driver scripts, fails on any
hard-coded user-at-host token or `PLANERADAR_`, requires `HP2R_TARGET`, and asserts
`build-driver.sh --help` documents `--target`, `--kernel-release`,
`--source-revision`, and `--output`. Add a mise task named
`test-build-contract`.

- [ ] **Step 2: Run the test and observe missing scripts**

Run `mise run test-build-contract`.

Expected: failure naming the absent driver commands.

- [ ] **Step 3: Port the proven scripts with explicit inputs**

Move the corresponding app scripts, rename every artifact to the generic
identity, and replace implicit repository paths with:

```bash
repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
target="${HP2R_TARGET:?set HP2R_TARGET or pass --target}"
```

Preserve manifest validation, base-DTB binding, host-tool preparation,
vermagic checks, path traversal rejection, and atomic target staging.

- [ ] **Step 4: Add deterministic artifact schema assertions**

Require exact manifest keys:

```text
schema_version
driver_version
source_revision
source_tree
kernel_release
architecture
base_dtb_sha256
module_file
module_sha256
module_vermagic
overlay_file
overlay_sha256
applied_dtb_file
applied_dtb_sha256
```

Reject duplicate, missing, unknown, absolute-path, and trailing-data entries.

- [ ] **Step 5: Run host and container verification**

Run:

```bash
mise run verify
HP2R_TARGET="$PLANERADAR_PI_TARGET" mise run export-target-kbuild
HP2R_TARGET="$PLANERADAR_PI_TARGET" mise run build-driver
mise run check-artifacts
```

Expected: the exact live kernel artifacts pass with generic names.

- [ ] **Step 6: Commit**

```bash
but commit driver-v0.1 -m $'build: package exact-kernel driver artifacts\n\nCo-authored-by: Codex <noreply@openai.com>'
```

### Task 6: Move tryboot, verification, rollback, and uninstall into the driver repo

**Files:**
- Create: `scripts/stage-tryboot.sh`
- Create: `scripts/verify-boot.sh`
- Create: `scripts/commit-boot.sh`
- Create: `scripts/rollback-boot.sh`
- Create: `scripts/uninstall.sh`
- Create: `tests/boot-scripts.sh`
- Create: `docs/operations.md`
- Modify: `mise.toml`

**Interfaces:**
- Consumes: Task 5 artifact manifest and explicit `HP2R_TARGET`.
- Produces: commands `stage-tryboot`, `verify-boot`, `commit-boot`, `rollback-boot`, and `uninstall`.

- [ ] **Step 1: Port the existing hostile-fixture tests first**

Port the app's `tests/hyperpixel_boot_scripts.rs` fixtures to
`tests/boot-scripts.sh` and update expected names to `hyperpixel2r-kms`. Keep
tests for symlinks, garbage, unsafe kernel release, changed boot source,
partial DKMS state, rollback, and idempotence. Add the
`test-boot-scripts` mise task.

- [ ] **Step 2: Run tests and observe missing generic scripts**

Run `mise run test-boot-scripts`.

Expected: all command fixtures fail because scripts are absent.

- [ ] **Step 3: Port the safe boot transaction**

Move the five proven scripts. Preserve:

- exact boot-source SHA comparison;
- atomic file replacement and mode checks;
- versioned artifacts under `/usr/lib/hyperpixel2r-kms/`;
- DKMS source under `/usr/src/hyperpixel2r-kms-<version>`;
- one-shot `sudo reboot '0 tryboot'`;
- expected module/overlay identity;
- restoration of preexisting `tryboot.txt`;
- refusal to edit unrelated overlay declarations.

- [ ] **Step 4: Define machine-readable verification output**

Make `verify-boot.sh --json` emit:

```json
{
  "schema_version": 1,
  "driver_version": "0.1.0",
  "kernel_release": "6.18.34+rpt-rpi-v8",
  "module": "hyperpixel2r_kms",
  "drm_mode": "480x480",
  "touch": true,
  "sdl_driver": "KMSDRM",
  "renderer": "opengles2",
  "accepted": true
}
```

Values come from live probes; the example fixes field names and types.

- [ ] **Step 5: Run fixture and live trial verification**

Run:

```bash
mise run verify
HP2R_TARGET="$PLANERADAR_PI_TARGET" mise run stage-tryboot
```

Reboot through the script, then:

```bash
HP2R_TARGET="$PLANERADAR_PI_TARGET" mise run verify-boot -- --json
HP2R_TARGET="$PLANERADAR_PI_TARGET" mise run rollback-boot
```

Expected: trial verifies, rollback restores the currently accepted Plane Radar
driver without losing SSH.

- [ ] **Step 6: Commit**

```bash
but commit driver-v0.1 -m $'feat: manage safe HyperPixel boot lifecycle\n\nCo-authored-by: Codex <noreply@openai.com>'
```

### Task 7: Add driver CI, release manifests, documentation, and `v0.1.0-rc.1`

**Files:**
- Create: `release/driver-manifest.schema.json`
- Create: `scripts/package-release.sh`
- Create: `tests/release-contract.sh`
- Create: `.github/workflows/ci.yml`
- Create: `.github/workflows/release.yml`
- Create: `README.md`
- Create: `docs/compatibility.md`
- Create: `docs/provenance.md`
- Modify: `mise.toml`

**Interfaces:**
- Consumes: driver source and scripts from Tasks 4–6.
- Produces: immutable-shape release assets and public release `v0.1.0-rc.1`.

- [ ] **Step 1: Write the shell release contract test**

Create `tests/release-contract.sh` and a `test-release-contract` mise task.
Test that packaging emits:

```text
hyperpixel2r-kms-source.tar.zst
driver-manifest.json
SHA256SUMS
SBOM.spdx.json
```

and, when `dist/artifacts/<kernel>` exists, one exact-kernel archive. Parse
`driver-manifest.json` and assert source commit, semantic version, artifact
digest, architecture, and kernel compatibility fields.

- [ ] **Step 2: Run tests and observe missing packaging**

Run `mise run test-release-contract`.

Expected: release contract test fails before packaging exists.

- [ ] **Step 3: Implement deterministic packaging and workflows**

Use sorted tar input, numeric owner/group zero, and `SOURCE_DATE_EPOCH` from the
source commit. Release workflow permissions must include:

```yaml
permissions:
  contents: write
  id-token: write
  attestations: write
```

Build assets, generate SPDX SBOM, attest each executable/archive and the
manifest, create a draft prerelease, then upload all assets.

- [ ] **Step 4: Write public driver documentation**

Document supported hardware/OS, exact-kernel behavior, generic standalone use,
tryboot recovery, DKMS limits, GPL/upstream provenance, and the fact that this
repo is independent of Plane Radar.

- [ ] **Step 5: Verify and publish the release candidate**

Run:

```bash
mise run verify
but commit driver-v0.1 -m $'ci: publish verified HyperPixel driver releases\n\nCo-authored-by: Codex <noreply@openai.com>'
```

Copy the branch ID from `but status`, rename it to `main` with `but reword`,
configure `origin` as the push remote, run `but push main --dry-run`, and then
run `but push main`. Create tag/release candidate `v0.1.0-rc.1` through the
release workflow. Verify:

```bash
gh release download v0.1.0-rc.1 -R shayne/hyperpixel2r-kms -D dist/rc1
gh attestation verify dist/rc1/hyperpixel2r-kms-source.tar.zst \
  -R shayne/hyperpixel2r-kms
```

Expected: workflow green, assets present, checksums pass, attestation resolves
to the driver repository and tagged commit.

- [ ] **Step 6: Commit any release-only fixes and republish as `rc.2`**

Do not mutate `rc.1`. If verification finds a defect, commit:

```bash
but commit main -m $'fix: complete driver release contract\n\nCo-authored-by: Codex <noreply@openai.com>'
```

Push `main`, publish `v0.1.0-rc.2`, and repeat the artifact verification from
Step 5.

## Phase 3: Build the Mac control plane

### Task 8: Add the driver lock and `planeradarctl` workspace skeleton

**Files:**
- Modify: `Cargo.toml`
- Modify: `Cargo.lock`
- Create: `driver.lock.toml`
- Create: `crates/planeradarctl/Cargo.toml`
- Create: `crates/planeradarctl/src/lib.rs`
- Create: `crates/planeradarctl/src/cli.rs`
- Create: `crates/planeradarctl/src/config.rs`
- Create: `crates/planeradarctl/src/main.rs`
- Create: `crates/planeradarctl/tests/cli.rs`
- Modify: `mise.toml`

**Interfaces:**
- Consumes: published driver RC repository, commit, and manifest digest.
- Produces: `planeradarctl` command parser and `InstallConfig::resolve`.

- [ ] **Step 1: Add failing CLI precedence tests**

Test:

```rust
#[test]
fn cli_target_wins_over_environment_and_default() {
    let cli = Cli::try_parse_from([
        "planeradarctl", "install", "alice@radar.local", "--hostname", "hangar",
    ]).unwrap();
    let env = Environment {
        target: Some("pi@raspberrypi.local".into()),
        hostname: Some("planeradar".into()),
        docker_context: Some("orbstack".into()),
    };
    let config = InstallConfig::resolve(cli, env).unwrap();
    assert_eq!(config.target.to_string(), "alice@radar.local");
    assert_eq!(config.hostname, "hangar");
}
```

Also test an absent `.env` and zero CLI arguments produce promptable `None`
rather than a panic.

- [ ] **Step 2: Add the workspace member and observe test failure**

Add:

```toml
[workspace]
members = [".", "crates/planeradarctl"]
resolver = "3"
```

Run `mise exec -- cargo test -p planeradarctl`.

Expected: missing CLI/config modules.

- [ ] **Step 3: Implement the command surface**

Define Clap subcommands `install`, `upgrade`, `status`, `doctor`, `screenshot`,
`rollback`, and `uninstall`. Every mutating command accepts:

```text
[target]
--hostname <hostname>
--version <version>
--release-dir <directory>
--docker-context <context>
--non-interactive
```

Read `.env` with a small strict dotenv parser that accepts only the three
documented keys and ignores a missing file. Reject simultaneous `--version`
and `--release-dir`. The local directory is a maintainer acceptance input and
must pass the same manifest, checksum, repository, commit, and attestation
identity checks as downloaded assets. Do not accept passwords or tokens.

- [ ] **Step 4: Pin the driver RC**

Write `driver.lock.toml` with the exact published RC version, full commit, and
downloaded manifest SHA-256. Add a parser test that rejects shortened commits,
non-HTTPS repository URLs, invalid semver, and malformed digests.

- [ ] **Step 5: Run verification**

Run:

```bash
mise exec -- cargo test -p planeradarctl
mise run verify
```

Expected: app and control workspace pass without compiling SDL for
`cargo build -p planeradarctl`.

- [ ] **Step 6: Commit**

```bash
but commit product-readiness -m $'feat: add Plane Radar control CLI\n\nCo-authored-by: Codex <noreply@openai.com>'
```

### Task 9: Implement durable state and target identity

**Files:**
- Create: `crates/planeradarctl/src/state.rs`
- Create: `crates/planeradarctl/src/target.rs`
- Create: `crates/planeradarctl/tests/state.rs`

**Interfaces:**
- Produces:

```rust
pub enum InstallPhase {
    Discovered,
    PreflightPassed,
    ApplicationAcquired,
    DriverReady,
    TrybootStaged,
    TrybootVerified,
    DriverAccepted,
    ApplicationInstalled,
    HostnameChanged,
    FinalRebooted,
    FinalVerified,
    Complete,
}

pub struct TargetIdentity {
    pub host_key_sha256: String,
    pub model: String,
    pub serial: String,
}

pub struct InstallState {
    pub schema_version: u32,
    pub target: TargetIdentity,
    pub phase: InstallPhase,
    pub application: Option<ArtifactIdentity>,
    pub driver: Option<ArtifactIdentity>,
}
```

- [ ] **Step 1: Add atomic-state tests**

Test round-trip, unknown schema rejection, symlink refusal, mode `0600`,
same-directory atomic replacement, truncated JSON rejection, and identity
mismatch rejection.

- [ ] **Step 2: Run tests and observe missing types**

Run `mise exec -- cargo test -p planeradarctl --test state`.

Expected: compile failure for `StateStore`, `InstallState`, and
`TargetIdentity`.

- [ ] **Step 3: Implement the state store**

Provide:

```rust
pub trait StateStore {
    fn load(&self) -> Result<Option<InstallState>, StateError>;
    fn save(&self, state: &InstallState) -> Result<(), StateError>;
}
```

Use an XDG-style macOS state root resolved from the user's home and a target
key derived from the SHA-256 host-key fingerprint. Use `tempfile::NamedTempFile`
in the same directory, `sync_all`, persist/rename, directory `sync_all`, and
mode `0600`.

Define the matching target-side record stored at
`/var/lib/planeradar/installer/state.json`. It contains the schema, target
hardware identity, installed app/driver identities, owned-file manifest, and
last verified phase, but no Mac path, credential, location setting, or secret.
The internal target installer writes it root-owned, mode `0600`, with the same
atomic-file rules.

- [ ] **Step 4: Implement target parsing and identity comparison**

`SshTarget` accepts exactly `user@hostname` or `user@IPv4`; reject whitespace,
shell metacharacters, empty components, root, URL syntax, and option-like
hosts. Keep SSH arguments separate; never build a shell command by joining
untrusted strings.

- [ ] **Step 5: Run control-tool tests**

Run `mise exec -- cargo test -p planeradarctl`.

Expected: all state and parser tests pass.

- [ ] **Step 6: Commit**

```bash
but commit product-readiness -m $'feat: persist installer target state\n\nCo-authored-by: Codex <noreply@openai.com>'
```

### Task 10: Implement OpenSSH transport and reboot reconnection

**Files:**
- Create: `crates/planeradarctl/src/transport.rs`
- Create: `crates/planeradarctl/tests/transport.rs`

**Interfaces:**
- Consumes: `SshTarget`, `TargetIdentity`.
- Produces:

```rust
pub trait Transport {
    fn probe(&self, target: &SshTarget) -> Result<TargetProbe, TransportError>;
    fn run(&self, target: &SshTarget, request: RemoteCommand) -> Result<Output, TransportError>;
    fn copy_to(&self, target: &SshTarget, local: &Path, remote: &Path) -> Result<(), TransportError>;
    fn copy_from(&self, target: &SshTarget, remote: &Path, local: &Path) -> Result<(), TransportError>;
    fn wait_for_reboot(&self, identity: &TargetIdentity, addresses: &[SshTarget], policy: ReconnectPolicy)
        -> Result<SshTarget, TransportError>;
}
```

- [ ] **Step 1: Add command-construction tests**

Assert that OpenSSH receives argument vectors, not an interpolated local shell:

```rust
assert_eq!(
    invocation.args,
    ["-o", "BatchMode=yes", "--", "alice@radar.local", "uname", "-r"]
);
```

Add a PTY case with `-tt` for sudo and reject remote arguments containing NUL
or newline.

- [ ] **Step 2: Add deterministic reboot tests**

Use a fake runner sequence: reachable old boot, disconnect, two connection
refusals, reachable new hostname with matching identity. Add mismatched host
key and timeout cases.

- [ ] **Step 3: Implement OpenSSH and scp adapters**

Use `std::process::Command`, `BatchMode=yes` for noninteractive probes, strict
host-key checking, bounded connect timeout, and `-tt` only for interactive
sudo. Capture redacted stdout/stderr; never log target settings or location
data beyond host identity.

- [ ] **Step 4: Implement identity-based reconnection**

Poll original hostname, recorded IPs, and desired `.local` name with bounded
backoff. Accept a connection only after host key, model, and serial match the
recorded target.

- [ ] **Step 5: Run tests**

Run `mise exec -- cargo test -p planeradarctl`.

Expected: transport and reboot simulations pass without network access.

- [ ] **Step 6: Commit**

```bash
but commit product-readiness -m $'feat: control targets through OpenSSH\n\nCo-authored-by: Codex <noreply@openai.com>'
```

### Task 11: Implement release manifests, caching, and attestation verification

**Files:**
- Create: `release/release-manifest.schema.json`
- Create: `crates/planeradarctl/src/release.rs`
- Create: `crates/planeradarctl/tests/release.rs`
- Create: `tests/fixtures/releases/valid.json`
- Create: `tests/fixtures/releases/invalid-*.json`

**Interfaces:**
- Produces `ReleaseManifest`, `DriverLock`, `Artifact`, `ReleaseClient::resolve`,
  `Verifier::verify`.

- [ ] **Step 1: Add hostile manifest tests**

Cover unknown schema, duplicate artifact name, wrong repository, wrong commit,
wrong architecture, path traversal, invalid SHA-256, size mismatch, driver-lock
mismatch, trailing JSON, a valid read-only local release directory, and a local
directory whose file differs from its manifest.

- [ ] **Step 2: Run and observe missing release implementation**

Run `mise exec -- cargo test -p planeradarctl --test release`.

Expected: compile failure for manifest and verifier types.

- [ ] **Step 3: Implement strict manifest parsing**

Use `#[serde(deny_unknown_fields)]` for:

```rust
pub struct ReleaseManifest {
    pub schema_version: u32,
    pub version: String,
    pub source_commit: String,
    pub supported: SupportedTarget,
    pub driver: LockedDriver,
    pub artifacts: Vec<Artifact>,
}
```

Require full lowercase 40-hex commits, 64-hex SHA-256, unique safe basenames,
and exact supported model/OS/architecture.

- [ ] **Step 4: Implement cache and verification**

Download to a temporary file, enforce declared size while streaming, compute
SHA-256, fsync, then move into a content-addressed cache. Run
`gh release verify` with the resolved release tag and
`gh attestation verify` with each downloaded runnable artifact through the
command-runner interface. Pass `-R shayne/RPi-Plane-Radar` as separate
arguments and require both commands to succeed for stable releases. Unit tests
assert complete argument vectors using tag `v0.1.0` and artifact
`planeradar-aarch64-linux-gnu.tar.zst`.

- [ ] **Step 5: Run tests and verification**

Run:

```bash
mise exec -- cargo test -p planeradarctl --test release
mise run verify
```

Expected: all fixture cases pass.

- [ ] **Step 6: Commit**

```bash
but commit product-readiness -m $'feat: verify immutable release artifacts\n\nCo-authored-by: Codex <noreply@openai.com>'
```

### Task 12: Implement Mac and target preflight

**Files:**
- Create: `crates/planeradarctl/src/preflight.rs`
- Create: `crates/planeradarctl/tests/preflight.rs`
- Modify: `src/install.rs`
- Modify: `tests/install.rs`

**Interfaces:**
- Consumes: `Transport::probe`, target package list.
- Produces `PreflightReport { checks: Vec<CheckResult> }` and
  `PreflightReport::require_success`.

- [ ] **Step 1: Add table-driven preflight tests**

Use exact cases for unsupported Darwin/architecture, missing host tools,
unreachable repositories, unavailable Buildx context, insufficient Mac disk,
wrong SSH identity, unavailable interactive sudo, wrong Pi model, Debian 12,
armhf, Desktop/display-manager active, missing `/boot/firmware`, missing
tryboot, wrong system time, unreachable package repository, port 80 conflict,
unsafe overlay, insufficient target/boot space, unavailable or mismatched
headers, unexpected GPIO/display state, and successful Trixie Lite.

- [ ] **Step 2: Run and observe failure**

Run `mise exec -- cargo test -p planeradarctl --test preflight`.

Expected: missing preflight types.

- [ ] **Step 3: Implement structured probes**

The remote probe returns JSON fields rather than human-formatted shell output:

```json
{
  "model": "Raspberry Pi Zero 2 W Rev 1.0",
  "os_id": "debian",
  "os_version": "13",
  "architecture": "arm64",
  "kernel_release": "6.18.34+rpt-rpi-v8",
  "default_target": "multi-user.target",
  "boot_config": "/boot/firmware/config.txt",
  "port_80_free": true
}
```

Parse locally with `deny_unknown_fields`.

- [ ] **Step 4: Install complete target prerequisites**

Extend the target installer package set to include:

```text
dkms kmod device-tree-compiler linux-headers-rpi-v8 build-essential evtest pngcheck
```

Do not run `full-upgrade`. Return `RebootRequired` when installed and running
kernel/header versions require a restart.

- [ ] **Step 5: Run app and control tests**

Run:

```bash
mise exec -- cargo test -p planeradarctl --test preflight
mise exec -- cargo test --test install
mise run verify
```

Expected: all supported and rejected target cases are explicit.

- [ ] **Step 6: Commit**

```bash
but commit product-readiness -m $'feat: validate supported installation targets\n\nCo-authored-by: Codex <noreply@openai.com>'
```

### Task 13: Integrate the external driver release

**Files:**
- Create: `crates/planeradarctl/src/driver.rs`
- Create: `crates/planeradarctl/tests/driver.rs`
- Modify: `driver.lock.toml`
- Delete after parity: `kernel/`
- Delete after parity: driver-owned scripts listed in the design
- Modify: driver-related app tests to validate the external contract
- Modify: `mise.toml`

**Interfaces:**
- Consumes: `DriverLock`, release verifier, target probe, driver RC scripts.
- Produces `DriverResolver::resolve(&TargetProbe) -> DriverPlan` and
  `DriverTool::run(DriverAction)`, plus maintainer commands
  `planeradarctl driver sync` and `planeradarctl driver update`.

- [ ] **Step 1: Add resolver tests**

Test exact prebuilt selection and fallback:

```rust
assert!(matches!(
    resolver.resolve(&probe("6.18.34+rpt-rpi-v8"))?,
    DriverPlan::Prebuilt { .. }
));
assert!(matches!(
    resolver.resolve(&probe("6.18.35+rpt-rpi-v8"))?,
    DriverPlan::CrossBuild { .. }
));
```

Reject a prebuilt archive whose internal vermagic, kernel release, or manifest
digest differs.

- [ ] **Step 2: Run and observe missing adapter**

Run `mise exec -- cargo test -p planeradarctl --test driver`.

Expected: compile failure for driver resolver/tool.

- [ ] **Step 3: Implement locked release acquisition**

Verify `driver.lock.toml` against the application release manifest. Extract the
source archive into a content-addressed cache with no symlinks, hardlinks,
absolute paths, or parent traversal. Wire `mise run driver:sync` to
`planeradarctl driver sync`; it resolves the lock, verifies release integrity
and attestation, and materializes the exact revision under the ignored cache.

Wire `mise run driver:update -- 0.1.0` to
`planeradarctl driver update 0.1.0`; it verifies repository, semantic version,
full commit, manifest digest, and compatibility before atomically changing
`driver.lock.toml`. Add tests that a failed check leaves the lock byte-for-byte
unchanged.

- [ ] **Step 4: Implement driver tool invocation**

Map typed actions to exact generic scripts:

```rust
pub enum DriverAction {
    ExportKernel,
    Build,
    StageTryboot,
    VerifyBoot,
    CommitBoot,
    RollbackBoot,
    Uninstall,
}
```

Pass target and paths as separate arguments/environment values. Parse the
verification JSON from Task 6.

- [ ] **Step 5: Remove the in-repo driver only after parity**

Run both old and external tooling against the same live kernel context.
Because generic symbol and module renaming changes the module bytes, compare
kernel release/vermagic, dependency set, applied-DTB semantics, DRM mode,
touch result, renderer, and live verification output rather than module SHA.
Then delete `kernel/` and driver-owned app scripts; retain only application
orchestration and external-contract tests.

- [ ] **Step 6: Run full verification**

Run:

```bash
mise exec -- cargo test -p planeradarctl --test driver
mise run verify
! test -d kernel
```

Expected: app CI no longer compiles driver source and lock validation passes.

- [ ] **Step 7: Commit**

```bash
but commit product-readiness -m $'build: consume external HyperPixel driver\n\nCo-authored-by: Codex <noreply@openai.com>'
```

### Task 14: Implement the resumable installation state machine

**Files:**
- Create: `crates/planeradarctl/src/install.rs`
- Create: `crates/planeradarctl/tests/install.rs`
- Modify: `crates/planeradarctl/src/main.rs`
- Modify: `src/main.rs`
- Modify: `src/install.rs`

**Interfaces:**
- Consumes: state store, transport, preflight, release, driver adapter.
- Produces `Installer::run(InstallRequest) -> InstallOutcome`.

- [ ] **Step 1: Add a scripted happy-path test**

Use fakes and assert the exact transition sequence:

```rust
assert_eq!(phases, [
    InstallPhase::Discovered,
    InstallPhase::PreflightPassed,
    InstallPhase::ApplicationAcquired,
    InstallPhase::DriverReady,
    InstallPhase::TrybootStaged,
    InstallPhase::TrybootVerified,
    InstallPhase::DriverAccepted,
    InstallPhase::ApplicationInstalled,
    InstallPhase::HostnameChanged,
    InstallPhase::FinalRebooted,
    InstallPhase::FinalVerified,
    InstallPhase::Complete,
]);
```

- [ ] **Step 2: Add interruption and idempotence tests**

For every persisted phase, stop the first run, construct a new `Installer`,
resume, and assert completed actions are not repeated. Add verification drift
that invalidates only the affected phase and its successors. Include SSH loss,
Mac process interruption, hostname change, a tryboot target that does not
return, a tryboot verification failure, final-service failure, and rerun after
completion. Assert the target-side and Mac records agree before any resume.

- [ ] **Step 3: Run and observe missing installer**

Run `mise exec -- cargo test -p planeradarctl --test install`.

Expected: compile failure for installer traits and outcomes.

- [ ] **Step 4: Implement one phase at a time**

Define an `InstallBackend` trait containing typed operations. After each
successful operation, verify its postcondition, atomically persist the next
phase, and emit one concise status event. Never persist a phase before its
postcondition succeeds. For tryboot timeout, report that the one-shot boot will
fall back on the next power cycle and preserve enough state for `doctor`,
`rollback`, or a resumed install. For hostname changes, reconnect only through
the identity checks from Task 10.

- [ ] **Step 5: Add target-side machine output**

Add `--json` to the internal target `planeradar install` command and emit:

```json
{
  "schema_version": 1,
  "files_changed": true,
  "boot_config_changed": false,
  "reboot_required": false,
  "revision": "0123456789abcdef0123456789abcdef01234567",
  "sha256": "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"
}
```

Keep existing human output for direct diagnostics.

- [ ] **Step 6: Run tests and full verification**

Run:

```bash
mise exec -- cargo test -p planeradarctl --test install
mise run verify
```

Expected: all phase interruption cases and app tests pass.

- [ ] **Step 7: Commit**

```bash
but commit product-readiness -m $'feat: orchestrate resumable Pi installation\n\nCo-authored-by: Codex <noreply@openai.com>'
```

### Task 15: Implement status, doctor, and real screenshot retrieval

**Files:**
- Create: `crates/planeradarctl/src/operations.rs`
- Create: `crates/planeradarctl/tests/operations.rs`
- Modify: `crates/planeradarctl/src/main.rs`
- Modify: `src/app.rs` if capture acknowledgement is required

**Interfaces:**
- Produces `status`, `doctor`, and `screenshot` operation reports.

- [ ] **Step 1: Add doctor mismatch tests**

Cover app revision mismatch, checksum mismatch, wrong kernel, absent module,
wrong overlay, inactive service, restart count, HTTP failure, missing touch,
wrong DRM mode, and healthy state.

- [ ] **Step 2: Add screenshot validation tests**

Use fixtures for valid 480×480 RGBA PNG, wrong dimensions, truncated PNG,
symlink remote path, and unchanged stale capture.

- [ ] **Step 3: Implement status and doctor**

`status` is concise and read-only. `doctor --json` emits stable fields for
target identity, app, driver, kernel, DRM, touch, service, HTTP, mDNS, and
settings state. Exit zero only when all required checks agree.

- [ ] **Step 4: Implement screenshot**

Record the prior debug-frame metadata, send `SIGUSR1` through systemd, wait for
a newly replaced root-owned regular file, copy it, decode it locally, require
480×480 RGBA, and atomically write the requested destination.

- [ ] **Step 5: Run tests**

Run:

```bash
mise exec -- cargo test -p planeradarctl --test operations
mise run verify
```

Expected: every mismatch has a distinct diagnostic and screenshot rejects
stale/unsafe output.

- [ ] **Step 6: Commit**

```bash
but commit product-readiness -m $'feat: diagnose and capture running radar\n\nCo-authored-by: Codex <noreply@openai.com>'
```

### Task 16: Implement upgrade, rollback, and uninstall

**Files:**
- Modify: `crates/planeradarctl/src/operations.rs`
- Create: `crates/planeradarctl/tests/lifecycle.rs`
- Modify: `src/install.rs`
- Modify: `tests/install.rs`

**Interfaces:**
- Consumes: accepted artifact history and driver actions.
- Produces `upgrade`, `rollback`, and `uninstall` outcomes with explicit
  application-only versus driver-changing paths.

- [ ] **Step 1: Add application-only upgrade tests**

Assert atomic binary replacement, settings preservation, health verification,
and restoration of the old binary when the new service fails.

- [ ] **Step 2: Add driver-changing and uninstall tests**

Assert driver upgrade invokes stage/tryboot/verify/commit; app-only upgrade
does not reboot. Assert uninstall removes only owned files/DKMS entries and
preserves unrelated boot lines and settings unless `--purge-settings` is
explicit.

- [ ] **Step 3: Run and observe failures**

Run `mise exec -- cargo test -p planeradarctl --test lifecycle`.

Expected: missing lifecycle operations.

- [ ] **Step 4: Implement version selection and accepted-history retention**

Keep the current and previous two accepted app/driver pairs. `--version`
selects an immutable release. Rollback chooses the latest prior accepted pair
unless a version is supplied.

- [ ] **Step 5: Implement uninstall ownership rules**

Use recorded ownership manifests; do not infer ownership from broad directory
globs. Restore the first-install boot backup only when the current file still
matches the installer's accepted source relationship.

- [ ] **Step 6: Run verification**

Run:

```bash
mise exec -- cargo test -p planeradarctl --test lifecycle
mise run verify
```

Expected: app-only, driver-changing, rollback, and uninstall cases pass.

- [ ] **Step 7: Commit**

```bash
but commit product-readiness -m $'feat: manage Plane Radar lifecycle\n\nCo-authored-by: Codex <noreply@openai.com>'
```

## Phase 4: Package, test, and document the public product

### Task 17: Add Plane Radar release packaging, CI matrix, SBOM, and attestations

**Files:**
- Create: `scripts/package-release.sh`
- Create: `scripts/install.sh`
- Create: `.github/workflows/release.yml`
- Modify: `.github/workflows/ci.yml`
- Create: `tests/release_contract.rs`
- Modify: `mise.toml`

**Interfaces:**
- Consumes: app binary, two macOS control binaries, release schema, driver lock.
- Produces: complete Plane Radar draft-release asset set.

- [ ] **Step 1: Add release contract tests**

Assert exact assets:

```text
planeradar-aarch64-linux-gnu.tar.zst
planeradarctl-aarch64-apple-darwin.tar.zst
planeradarctl-x86_64-apple-darwin.tar.zst
install.sh
release-manifest.json
SHA256SUMS
SBOM.spdx.json
```

Verify the manifest's driver identity equals `driver.lock.toml`.

- [ ] **Step 2: Run and observe failure**

Run `mise exec -- cargo test --test release_contract`.

Expected: missing workflow, script, and assets.

- [ ] **Step 3: Implement deterministic packaging**

Build the app in the ARM64 Trixie container and control tool on macOS runners.
Archive with normalized ownership/timestamps. Generate manifest, SHA256SUMS,
and SPDX SBOM. Make `install.sh` require `gh`, verify the immutable release and
control-tool attestation, then execute the downloaded binary.

- [ ] **Step 4: Expand CI**

Keep current verify coverage and add:

- Linux ARM64 app release build;
- Apple Silicon control build/test;
- Intel control build/test;
- release schema/lock validation;
- installer fixture tests;
- README command test hook.

Pin action major versions and grant write permissions only to the release job.

- [ ] **Step 5: Test packaging locally and in CI**

Run:

```bash
mise run verify
mise run package-release -- 0.1.0-rc.1
shasum -a 256 -c dist/release/SHA256SUMS
```

Expected: every declared asset exists and verifies.

- [ ] **Step 6: Commit**

```bash
but commit product-readiness -m $'ci: publish verified Plane Radar releases\n\nCo-authored-by: Codex <noreply@openai.com>'
```

### Task 18: Replace staging-only smoke tests with end-to-end fixtures

**Files:**
- Create: `tests/ctl_end_to_end.rs`
- Create: `tests/fixtures/pi-os-trixie/`
- Replace: `scripts/smoke-pi.sh`
- Modify: `tests/deploy_scripts.rs`
- Modify: `mise.toml`

**Interfaces:**
- Consumes: packaged release and `planeradarctl`.
- Produces `mise run smoke-pi` as a full installed-target health check.

- [ ] **Step 1: Build a clean filesystem fixture**

Include minimal regular files for model, os-release, passwd, boot config,
systemd, modules, hostname, and installer state. Do not include host secrets or
copy the live filesystem.

- [ ] **Step 2: Add a fixture install/resume test**

Run control logic against fake transport backed by the fixture. Assert package
commands, versioned files, hostname, service, URLs, and resume behavior.

- [ ] **Step 3: Replace the smoke script**

Make `smoke-pi.sh` invoke:

```bash
mise run status -- "$target"
mise run doctor -- "$target"
mise run screenshot -- "$target" --output dist/smoke-radar.png
```

Then validate PNG dimensions and compare installed/published revision and hash.

- [ ] **Step 4: Run fixture and live smoke**

Run:

```bash
mise exec -- cargo test --test ctl_end_to_end
PLANERADAR_PI_TARGET="$PLANERADAR_PI_TARGET" mise run smoke-pi
```

Expected: fixture and live target report healthy.

- [ ] **Step 5: Commit**

```bash
but commit product-readiness -m $'test: exercise installed Pi end to end\n\nCo-authored-by: Codex <noreply@openai.com>'
```

### Task 19: Rewrite public documentation and wire the real screenshot path

**Required skill:** `$plainspoken-voice` for README and public prose.

**Files:**
- Rewrite: `README.md`
- Rewrite: `docs/architecture.md`
- Create: `docs/install.md`
- Create: `docs/upgrading.md`
- Create: `docs/recovery.md`
- Create: `docs/development.md`
- Create: `CONTRIBUTING.md`
- Create: `SECURITY.md`
- Modify: `docs/troubleshooting.md`
- Retain through execution: this accepted design and implementation plan
- Use: `docs/images/planeradar-radar.png`

**Interfaces:**
- Consumes: final public command surface and current 480×480 device capture.
- Produces: command-first README and complete public operations documentation.

- [ ] **Step 1: Invoke and reread `$plainspoken-voice`**

Use it only for human-facing README/docs. Preserve commands, requirements,
warnings, URLs, state paths, licenses, and maturity claims exactly.

- [ ] **Step 2: Write README command checks before the rewrite**

Create a test that extracts fenced install commands and verifies every mise
task exists. Require support statement, screenshot path, ESP32 credit, driver
link, AI disclosure, license, and recovery link.

- [ ] **Step 3: Rewrite the README**

Lead with project identity, honest one-configuration maturity, and:

```markdown
![Plane Radar running on a Raspberry Pi Zero 2 W](docs/images/planeradar-radar.png)
```

Then provide requirements, four-command install, installer effects/reboots,
first-run QR, lifecycle commands, recovery, architecture links, upstream
credit, separate driver, and AI disclosure.

- [ ] **Step 4: Write focused public documents**

Move maintainer details out of README. Document durable/temporary state,
tryboot recovery, `.env`, release verification, upgrades, DKMS/kernel limits,
debug capture, logs, and contribution/security reporting.

- [ ] **Step 5: Consolidate and remove internal ledgers**

Compare each `docs/superpowers/` decision against architecture and public docs.
Move lasting facts and remove obsolete specs, plans, and unchecked execution
ledgers, but retain this accepted design and implementation plan while Tasks
20–23 still depend on them. Their final disposition occurs in Task 23 only
after every checkbox has normal documentation or release evidence.

- [ ] **Step 6: Run voice and command review**

Perform the skill's nine-dimension LLM-only voice check. Run:

```bash
mise run verify
mise run docs-check
```

Expected: commands resolve, facts match code, and no tracked file contains a
maintainer-specific SSH target.

- [ ] **Step 7: Commit**

```bash
but commit product-readiness -m $'docs: publish end-to-end project guide\n\nCo-authored-by: Codex <noreply@openai.com>'
```

## Phase 5: Rewrite history and accept the release

### Task 20: Rewrite Plane Radar history and verify Codex attribution

**Files/refs:**
- Backup: `/Users/shayne/code/RPi-Plane-Radar-pre-attribution-2026-07-27.bundle`
- Rewrite: Plane Radar commits after `upstream/main`
- Preserve: original 20 upstream commits exactly

**Interfaces:**
- Consumes: completed `product-readiness` branch.
- Produces: rewritten `main` with identical final tree and Codex trailers.

- [ ] **Step 1: Freeze and back up the pre-rewrite state**

Verify no uncommitted changes and create a bundle that records the exact
pre-rewrite `product-readiness` ref:

```bash
but status
git rev-parse product-readiness
git rev-parse product-readiness^{tree}
git bundle create /Users/shayne/code/RPi-Plane-Radar-pre-attribution-2026-07-27.bundle product-readiness
git bundle verify /Users/shayne/code/RPi-Plane-Radar-pre-attribution-2026-07-27.bundle
```

Expected: clean workspace and valid recoverable bundle.

- [ ] **Step 2: Expose project history above the untouched upstream base**

Set the GitButler target to the exact original base:

```bash
but config target upstream/main
but apply product-readiness
but status
```

Verify `upstream/main` is still
`69c10785afbc91285865abaf81027815a9dec7d1`.

- [ ] **Step 3: Squash only redundant adjacent public-history groups**

Using change IDs printed by `but status`, squash each design/plan commit into
its adjacent implementation for:

```text
radar visual weight
transparent radar text
range label outline
```

Copy the exact source and destination change IDs from `but status`, then invoke
`but squash` with those IDs and the preserved implementation subject. Do not
invent IDs. Do not squash third-party commits, independent fixes, security
hardening, or hardware-driver changes whose separation explains a real failure
boundary.

- [ ] **Step 4: Add Codex attribution to every project commit**

For each commit after `upstream/main`, inspect the complete message with
`but status -fv` and `but show`. If the exact trailer is absent, copy the
printed change ID and run `but reword` with the complete preserved subject/body
plus a final blank line and
`Co-authored-by: Codex <noreply@openai.com>`. Never invent an ID or replace a
useful message with generic text. Ensure exactly one trailer.

- [ ] **Step 5: Rename the rewritten branch to `main` and verify identity**

Copy the branch ID printed by `but status` and rename that branch to `main`
with `but reword`. Resolve the old tip from the recovery bundle and verify:

```bash
old_tip="$(git bundle list-heads /Users/shayne/code/RPi-Plane-Radar-pre-attribution-2026-07-27.bundle refs/heads/product-readiness | awk '{print $1}')"
test -n "$old_tip"
test "$(git rev-parse main^{tree})" = "$(git rev-parse "${old_tip}^{tree}")"
test "$(git rev-parse upstream/main)" = 69c10785afbc91285865abaf81027815a9dec7d1
```

Audit trailers with a script that parses NUL-delimited commit messages and
requires exactly one `Co-authored-by: Codex <noreply@openai.com>` after the
upstream base.

- [ ] **Step 6: Force-push once through GitButler**

Run:

```bash
but config push-remote origin
but push main --dry-run
but push main
```

Expected: protection reports an intentional rewrite and updates `origin/main`.
Do not use raw `git push`.

- [ ] **Step 7: Verify public history**

Use GitHub GraphQL commit authors for the new tip and representative commits.
Require co-author user login `codex`, primary author `shayne`, unchanged
upstream authors, and no public release/tag pointing to the old app history.

Set GitButler target back to `origin/main` and run `but pull`.

### Task 21: Build and deploy exact release candidates

**Files:**
- Produce: Plane Radar `v0.1.0-rc.1` draft assets
- Produce/promote: driver `v0.1.0` after driver acceptance
- Record: `docs/acceptance/0.1.0-rc.1.json`

**Interfaces:**
- Consumes: rewritten app tip and accepted driver RC.
- Produces: exact installed candidate and machine-readable acceptance record.

- [ ] **Step 1: Promote the accepted driver**

Deploy the driver RC alone to the live Pi through its generic tooling. Verify
tryboot, DRM, touch, colors, cold boot, and rollback. If unchanged and healthy,
build driver `v0.1.0` as an unpublished draft, download and verify its exact
assets, deploy those draft assets, and repeat driver acceptance. Publish the
unchanged accepted draft as immutable `v0.1.0`; update `driver.lock.toml` to
its exact commit/manifest digest and commit/push through GitButler with the
Codex trailer.

- [ ] **Step 2: Build the Plane Radar release candidate**

Trigger the app release workflow for `v0.1.0-rc.1`. Download assets, verify
SHA256SUMS, release integrity, SBOM, and attestations.

- [ ] **Step 3: Install the RC through the public command**

From a fresh clone directory with only `.env` supplying the maintainer target:

```bash
mise install
mise run install
```

Expected: the tool discovers config, uses the published app and driver,
performs required reboots, resumes, and ends healthy without manual Pi
commands.

- [ ] **Step 4: Capture automated acceptance evidence**

Run:

```bash
mise run doctor -- "$PLANERADAR_PI_TARGET" --json
mise run screenshot -- "$PLANERADAR_PI_TARGET" \
  --output docs/images/planeradar-radar.png
```

Record exact hardware, OS, kernel, app/driver versions and hashes, DRM mode,
renderer, touch, service restart count, HTTP/mDNS/IP URLs, CPU, RSS, warm boot,
and cold-boot result in the acceptance JSON.

- [ ] **Step 5: Perform human visual and gesture acceptance**

Confirm on the physical display: black edge-to-edge background, correct colors,
uncorrupted radar lines, transparent labels, outlined range label, correct
aircraft placement, QR screen, tap range cycling without QR flash, hold
behavior, QR dismissal, web settings, and persisted location.

- [ ] **Step 6: Commit the accepted screenshot and evidence**

```bash
but commit main -m $'docs: record v0.1 hardware acceptance\n\nCo-authored-by: Codex <noreply@openai.com>'
but push main
```

Because this changes the app source revision, build `v0.1.0-rc.2`, reinstall
its exact artifact, rerun automated smoke, and confirm the image-only/source
evidence commit did not change runtime behavior.

### Task 22: Perform clean-room installation, upgrade, rollback, and removal

**Hardware:**
- Fresh SD card
- Reference Pi Zero 2 W and HyperPixel 2.1 Round
- Mac with a clean repository clone

**Interfaces:**
- Consumes: `v0.1.0-rc.1` and `v0.1.0-rc.2`.
- Produces: public end-to-end acceptance evidence.

- [ ] **Step 1: Image the supported OS**

Use Raspberry Pi Imager with 64-bit Raspberry Pi OS Lite Trixie. Configure only
networking, SSH public key, admin username, and initial hostname. Do not install
display or application packages manually.

- [ ] **Step 2: Prove the stock prerequisite state**

SSH once only to record model, OS, architecture, kernel, and absence of Plane
Radar/driver state. Do not change the target.

- [ ] **Step 3: Install RC1 from a clean Mac clone**

Run exactly the README commands. Record prompts, elapsed time, reboots,
fallback/prebuilt selection, final URLs, and `doctor --json`.

Expected: no Pi-side command, final hostname defaults to `planeradar`, display
and touch work, and service survives cold power cycle.

- [ ] **Step 4: Upgrade RC1 to RC2**

Run:

```bash
mise run upgrade -- pi@planeradar.local --version 0.1.0-rc.2
```

Expected: application-only or pinned-driver path chosen correctly, settings
preserved, final doctor healthy.

- [ ] **Step 5: Roll back and uninstall**

Roll back to RC1, verify health, upgrade again to RC2, then run uninstall.
Verify unrelated boot config and networking remain, owned service/module/files
are gone, and the Pi still boots and accepts SSH.

- [ ] **Step 6: Reinstall RC2**

Run the public install again and confirm a clean healthy final appliance.
Attach clean-room evidence to `docs/acceptance/0.1.0-clean-room.json`.

- [ ] **Step 7: Commit clean-room evidence and accept the resulting tip**

Commit the clean-room JSON through GitButler with
`docs: record v0.1 clean-room acceptance` and the required Codex trailer, then
push `main`. Build `v0.1.0-rc.3` from that exact source revision, install it,
rerun automated doctor/screenshot/smoke checks, and repeat the short physical
display/touch/gesture acceptance. Do not create another tracked evidence file
from the RC3 run; retain its immutable workflow and release-asset evidence for
the final provenance report.

### Task 23: Publish immutable stable releases and close the program

**Files/remote state:**
- Final driver release: `shayne/hyperpixel2r-kms@v0.1.0`
- Final app release: `shayne/RPi-Plane-Radar@v0.1.0`
- Public README and screenshot
- Accepted live Pi installation

**Interfaces:**
- Consumes: all previous acceptance gates.
- Produces: complete public v0.1 product.

- [ ] **Step 1: Run final repository verification**

In both repos run:

```bash
mise run verify
but status
```

Expected: all checks pass, no uncommitted changes, branches target
`origin/main`, and remote main matches local.

- [ ] **Step 2: Verify public provenance**

Require green CI, Codex co-authorship on every AI-assisted commit, unchanged
upstream authors, correct MIT/GPL boundaries, valid SBOMs, passing checksums,
and GitHub attestations for every runnable archive.

- [ ] **Step 3: Consolidate completed planning ledgers**

Confirm every accepted design decision and every completed checkbox has a
durable home in public documentation, tests, acceptance JSON, or release
evidence. Remove obsolete `docs/superpowers/` ledgers, including this plan,
only if that audit passes; otherwise retain the exact file that still carries
unique evidence. Commit any deletion through GitButler with the required Codex
trailer and push. Repeat repository verification and proceed only if that
post-consolidation commit remains the current `main` tip.

- [ ] **Step 4: Build the final `v0.1.0` draft**

Create the final app tag from the exact post-consolidation commit and build an
unpublished draft release. Download the complete asset set to
`dist/final-v0.1.0`, verify checksums, manifest, SBOM, attestations, tag, and
source commit, and make the local release directory read-only for acceptance.

- [ ] **Step 5: Accept the exact stable draft**

Use the control tool's tested local-release input to upgrade from
`dist/final-v0.1.0`, then run:

```bash
mise run doctor -- "$PLANERADAR_PI_TARGET"
mise run screenshot -- "$PLANERADAR_PI_TARGET" --output dist/final-radar.png
```

Run the complete automated and physical acceptance suite from Task 21 without
creating another tracked change. Attach the generated acceptance JSON,
screenshot hash, and logs to the draft. Expected: installed revision and
hashes match the draft stable manifest, service is active with zero unexpected
restarts, KMSDRM/opengles2 and touch are active, all gestures work, and both
`.local` and IP URLs work.

- [ ] **Step 6: Publish immutable `v0.1.0` and verify public acquisition**

Confirm the draft assets are byte-for-byte the accepted files, publish the
release without rebuilding or replacing any asset, and use `v0.1.1` for any
later defect. From a clean clone, run the public upgrade command with
`--version 0.1.0`, then rerun `doctor` and compare installed hashes to the
published manifest.

- [ ] **Step 7: Verify the public README from scratch**

Open the GitHub page logged out, confirm the real screenshot renders, clone
into a new temporary directory, run the documented non-mutating status/doctor
setup, and verify all links and commands.

- [ ] **Step 8: Report completion**

Report both public URLs, stable tags and commits, artifact SHA-256 values, CI
run URLs, driver/kernel identity, installed app revision, screenshot path,
clean-room result, service state, renderer, CPU/RSS snapshot, and recovery
bundle location.

## Definition of Done evidence map

| Design item | Planned evidence |
|---|---|
| Clean, attributed `main` histories | Tasks 4, 7, 20, and 23 history/GraphQL audits |
| Green CI and immutable attested releases | Tasks 7, 17, 21, and 23 workflow and artifact checks |
| Fresh Mac-to-fresh-Pi install | Task 22 clean-room record |
| No manual Pi-side commands | Tasks 14, 19, 21, and 22 command transcript |
| Resume across reboot and interruption | Tasks 10, 14, and 18 deterministic tests |
| Upgrade and rollback | Tasks 16 and 22 RC1/RC2 evidence |
| Display, color, acceleration, touch, QR, web, and gestures | Task 21 physical acceptance record |
| Cold power-cycle health | Tasks 21 and 22 acceptance records |
| Complete `doctor` agreement | Tasks 15, 21, 22, and 23 JSON output |
| Screenshot from accepted device | Tasks 15, 19, and 21 validated 480×480 PNG |
| No maintainer defaults tracked | Tasks 1, 5, 18, and 19 repository scans |
| Installed hashes match releases | Tasks 11, 15, 18, 21, and 23 manifest checks |
| AI, ESP32, and third-party credit | Tasks 4, 7, 19, 20, and 23 public audits |
| Deferred edge cases fixed | Tasks 2 and 23 regression verification |
| CPU and memory recorded | Tasks 21 and 23 acceptance JSON/report |

The program is complete only when every Definition of Done item in
`docs/superpowers/specs/2026-07-27-rpi-plane-radar-product-readiness-design.md`
has direct evidence.

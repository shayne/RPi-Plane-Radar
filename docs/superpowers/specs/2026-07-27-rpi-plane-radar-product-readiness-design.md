# RPi Plane Radar Product Readiness Design

**Status:** Approved design

**Date:** 2026-07-27

**Repositories:** `shayne/RPi-Plane-Radar`, proposed `shayne/hyperpixel2r-kms`

## Purpose

RPi Plane Radar already runs successfully on the reference Raspberry Pi Zero
2 W and HyperPixel 2.1 Round display. The application, KMS/OpenGL ES renderer,
touch gestures, web setup, ADS-B feed, QR screen, and systemd service have all
been exercised on the physical device.

The remaining problem is distribution. The repository currently reproduces the
maintainer's bring-up process, not a complete public installation. A new user
must understand several internal scripts, export the target kernel build
context, cross-build a custom driver, stage a one-shot boot, verify it manually,
commit the accepted boot configuration, stage the application, and invoke the
target installer separately. Upgrades are similarly fragmented.

This design turns the proven implementation into an end-to-end public project.
A supported user starts with a networked Raspberry Pi running a stock supported
Raspberry Pi OS image, clones the repository on a Mac, and runs one documented
command. The tooling owns the transition to a working radar, including safe
reboots, verification, resumption, rollback, and later application upgrades.

## Product contract

### Supported installation host

The public installation path assumes:

- macOS on Apple Silicon or Intel;
- Git;
- [mise](https://mise.jdx.dev/);
- Docker Desktop, OrbStack, or an equivalent Docker Buildx implementation;
- SSH access to the target;
- internet access for GitHub release downloads and Raspberry Pi OS packages.

The primary documented path is:

```bash
git clone https://github.com/shayne/RPi-Plane-Radar
cd RPi-Plane-Radar
mise install
mise run install -- pi@raspberrypi.local
```

The target argument is optional. When omitted, the installer prompts for it.
The suggested starting username is `pi`, but the tool must not assume the
account exists. Current Raspberry Pi OS images ask the owner to choose an admin
username.

An optional release bootstrap may provide:

```bash
curl -fsSL \
  https://github.com/shayne/RPi-Plane-Radar/releases/latest/download/install.sh |
  bash -s -- pi@raspberrypi.local
```

The clone-and-mise path remains the primary, inspectable installation method.
The bootstrap is a convenience wrapper around the same versioned control tool
and release manifests, not an independent installer.

### Supported target

Version 0.1 supports exactly:

- Raspberry Pi Zero 2 W;
- HyperPixel 2.1 Round touch display;
- 64-bit Raspberry Pi OS Lite Trixie;
- ARM64 userland and kernel;
- `/boot/firmware` Raspberry Pi boot layout;
- a preconfigured network connection and SSH service.

The documentation may note that Raspberry Pi OS was previously called
Raspbian, but the supported requirement uses the current product name and an
exact release.

Other Raspberry Pi models, 32-bit images, Desktop images, Bookworm, other Linux
distributions, other displays, and unknown kernels are not implicitly
supported. The direct KMS appliance has been accepted against a Lite image with
no display manager competing for DRM ownership. Preflight must stop with a
useful explanation before changing the target.

Wi-Fi setup remains out of scope. The target must already be reachable from the
Mac over SSH.

### Target identity and hostname

The installer prompts for the desired hostname and defaults to `planeradar`.
The application derives its canonical `.local` URL from the installed hostname
instead of compiling `planeradar.local` as universal truth.

The control tool identifies a target by its SSH host key and Raspberry Pi
hardware identity, not only by a mutable hostname. This allows it to reconnect
after changing `raspberrypi.local` to `planeradar.local`. A custom hostname is
supported for owners with multiple devices.

### Maintainer-local configuration

No tracked script may default to a maintainer-specific SSH target.

Maintainer-specific values live in a gitignored `.env`. A committed
`.env.example` documents the supported keys:

```dotenv
PLANERADAR_PI_TARGET=pi@raspberrypi.local
PLANERADAR_HOSTNAME=planeradar
PLANERADAR_DOCKER_CONTEXT=
```

The local maintainer file contains the corresponding private target and Docker
context values. Those values are not repeated in tracked files.

`mise.toml` loads the file using the current `env._.file` mechanism. Command
line arguments take precedence over `.env`; `.env` takes precedence over public
defaults. The file is optional: a fresh clone without `.env` must still run the
public commands. SSH credentials and passwords are not stored in `.env`.

## Repository boundaries

### Application repository

`shayne/RPi-Plane-Radar` remains an MIT-licensed standalone project containing:

- the Rust display application;
- ADS-B, airport, settings, HTTP, QR, rendering, and gesture logic;
- the target-side application installer and systemd unit;
- the macOS control tool;
- the driver dependency lock;
- application CI, packaging, release, and public documentation.

The repository is not a GitHub fork. Its README prominently credits and links
to [MatixYo/ESP32-Plane-Radar](https://github.com/MatixYo/ESP32-Plane-Radar) as
the project that inspired the UX and behavior.

### Driver repository

A new public repository, `shayne/hyperpixel2r-kms`, owns the GPL-2.0-only
HyperPixel kernel work:

- generic DRM panel and shared touch-bus kernel sources;
- device-tree overlay;
- DKMS metadata;
- host-side GPIO and protocol tests;
- cross-build and local-build tooling;
- overlay compilation and applied-DTB validation;
- safe tryboot staging, verification, commit, and rollback logic;
- release manifests, checksums, SBOM, and CI;
- kernel compatibility and source-provenance documentation.

Plane Radar-specific identifiers are removed from the driver. Module, overlay,
device-tree, DKMS, artifact, and source names use a generic
`hyperpixel2r-kms` identity.

The driver repository's first non-empty commit is one clean import containing
the accepted implementation, tests, licenses, and provenance. GitButler's
generated empty root commit may precede it because the tool requires a target
base for its first authored change; that commit contains no project content.
The repository does not import a filtered copy of Plane Radar's mixed
application history.

### Driver dependency

Plane Radar does not use a Git submodule and does not vendor a second mutable
copy of the driver.

It pins a driver release in a tracked lock file:

```toml
repository = "https://github.com/shayne/hyperpixel2r-kms"
version = "0.1.0"
commit = "<full commit SHA>"
manifest_sha256 = "<release manifest SHA-256>"
```

Rules:

- installers consume an exact release and verify its manifest;
- application releases never follow driver `main`;
- `mise run driver:sync` clones the pinned revision into an ignored cache;
- `mise run driver:update -- <version>` updates the lock only after validation;
- CI fails when repository, version, commit, and digest disagree;
- an application upgrade changes the driver only when the application release
  explicitly pins another version.

The driver owns its low-level boot transaction. Plane Radar owns the higher
level sequence that combines the driver with the application.

## Host control plane

### `planeradarctl`

A new Rust binary, `planeradarctl`, runs on the Mac. It is separate from the
SDL-linked ARM64 display binary and has no graphics dependency.

The top-level command surface is:

```text
planeradarctl install
planeradarctl upgrade
planeradarctl status
planeradarctl doctor
planeradarctl screenshot
planeradarctl rollback
planeradarctl uninstall
```

Mise tasks provide the public interface:

```bash
mise run install -- user@host
mise run upgrade -- user@host
mise run status -- user@host
mise run doctor -- user@host
mise run screenshot -- user@host
mise run rollback -- user@host
mise run uninstall -- user@host
```

The control tool owns:

- target discovery and identity;
- SSH and interactive sudo execution;
- preflight;
- release selection and verification;
- artifact download and caching;
- Docker/OrbStack fallback builds;
- transfer and target-side execution;
- reboot, reconnect, retry, timeout, and resumption;
- hardware and service verification;
- rollback and operator-facing diagnostics.

The current target installer remains useful but becomes an internal,
versioned target-side operation invoked by `planeradarctl`, not a manual README
step.

### Resumable transaction

Installation is an explicit state machine:

```text
discover
  -> preflight
  -> acquire application release
  -> acquire or build driver
  -> stage tryboot
  -> reboot into tryboot
  -> verify display and touch
  -> accept driver boot configuration
  -> install application and service
  -> change hostname
  -> final reboot
  -> verify service, display, web, and URLs
  -> complete
```

Non-secret progress is recorded:

- on the Mac in a per-user state directory outside the repository;
- on the target under `/var/lib/planeradar/installer/`.

Each completed phase records the target hardware identity, inputs, artifact
digests, and verification evidence required to prove that it remains valid.
Repeating the same command resumes from verified state. It does not blindly
repeat boot edits or package installation.

The transaction must handle:

- SSH loss;
- Mac sleep or process interruption;
- sudo password prompts;
- slow package installation;
- hostname changes;
- planned reboots;
- a target that does not return after tryboot;
- a reachable target whose display, touch, or service checks fail;
- rerunning after successful completion.

State files do not contain SSH keys, sudo passwords, tokens, or user
application settings.

### Preflight

Preflight runs before persistent target changes.

Mac checks:

- Darwin architecture and supported release;
- Git, mise, SSH, GitHub CLI, and Docker Buildx;
- working configured Docker or OrbStack context;
- sufficient disk space;
- release and driver repository reachability.

Target checks:

- SSH host key and non-root login;
- interactive sudo access;
- exact Raspberry Pi Zero 2 W model;
- Raspberry Pi OS Trixie;
- ARM64 userland and running kernel;
- `/boot/firmware/config.txt`;
- tryboot support;
- package repository access and correct system time;
- sufficient disk and boot-partition space;
- current and installed kernel/header relationship;
- conflicting display overlays;
- existing Plane Radar or legacy driver installation;
- port 80 conflicts;
- expected GPIO/display environment.

Physical display function is accepted only after the one-shot driver boot. A
preflight cannot infer that a physically attached panel has correct colors or
working touch.

### Declared target packages

The installer explicitly installs all required runtime and maintenance
dependencies:

```text
libsdl2-2.0-0
libegl1
libgles2
libgl1-mesa-dri
ca-certificates
avahi-daemon
dkms
kmod
device-tree-compiler
linux-headers-rpi-v8
build-essential
evtest
pngcheck
```

This closes the current gap in which EGL, GLES, and Mesa runtime packages exist
on the reference Pi but are not declared by the installer.

The installer updates APT metadata and installs required packages. It does not
silently perform a full operating-system distribution upgrade. If package
installation puts a matching supported kernel on disk but the old kernel is
still running, the transaction records the condition, reboots, reconnects, and
resumes.

## Artifact selection and boot safety

### Release-driven hybrid

CI-built artifacts are the fast path. A target-specific local build is the
fallback.

The installer:

1. downloads and verifies the selected Plane Radar release;
2. reads its pinned driver identity;
3. downloads and verifies that driver release;
4. searches for an exact prebuilt driver bundle;
5. accepts a prebuilt module only when board, architecture, kernel release,
   modversions, and vermagic match exactly;
6. otherwise uses the pinned driver source and exact target kernel build
   context to cross-build through Docker;
7. records whether the accepted artifact was CI-built or locally built.

The fallback preserves the proven Mac cross-build workflow without requiring
every user to execute its internal scripts manually.

### Safe driver acceptance

Driver staging uses the existing one-shot tryboot safety property:

- preserve the accepted boot configuration and artifacts;
- stage versioned module, overlay, and trial configuration;
- reboot once with the tryboot flag;
- reconnect using hardware identity and known addresses;
- verify the expected DRM device, module, overlay, 480-by-480 mode, touch
  device, input range, SDL KMSDRM backend, and OpenGL ES renderer;
- accept the boot declaration only after automated checks succeed;
- restore or retain the previous accepted state on failure.

If the target does not return after tryboot, the tryboot state has already been
cleared by firmware. A power cycle returns to the previous configuration. The
tool must explain this recovery path clearly rather than pretending it can
remotely repair an unreachable computer.

## Installation, upgrade, and removal semantics

### First installation

The transaction:

- installs declared packages;
- selects or builds the exact driver;
- performs safe driver acceptance;
- registers versioned DKMS source;
- installs the application and systemd service atomically;
- creates protected application and installer state directories;
- changes the hostname, defaulting to `planeradar`;
- enables Avahi;
- reboots when required;
- verifies display, touch, service, HTTP, mDNS, and IP URLs;
- prints the canonical `.local` and IP-based `http://` URLs.

No manual target-side command is part of the supported happy path.

### Application upgrades

Application upgrades are explicit, not unattended.

An application-only upgrade:

- downloads a selected stable or explicitly pinned version;
- verifies the immutable release, attestation, manifest, and artifact digest;
- preserves settings;
- atomically replaces the binary and provenance files;
- restarts the service;
- verifies process, HTTP, display, and service health;
- restores the previous binary when verification fails.

### Driver upgrades

A release that pins a different driver version performs the full tryboot
transaction. It never replaces the accepted boot declaration in place before
verification.

### Kernel upgrades

Normal Raspberry Pi OS kernel packages remain owned by APT. Installed DKMS
source provides the standard rebuild path. `doctor` reports whether the
running kernel, installed module, overlay, and pinned driver agree.

Version 0.1 does not claim unattended safety for an arbitrary future kernel.
The installer and upgrade command reject unsupported or unbuildable kernels
rather than advertising speculative compatibility. The project does not hold
security-sensitive kernel packages or silently run a full OS upgrade.

### Rollback

`rollback` selects a previously accepted application/driver pair recorded in
installer state. An application-only rollback is atomic and does not reboot.
A driver rollback uses tryboot and the same verification gate as an upgrade.

### Uninstall

Uninstall:

- stops and disables the service;
- removes owned application binaries, provenance, unit, and state;
- removes owned driver artifacts and DKMS registrations;
- restores only configuration entries owned by this installer;
- preserves unrelated boot configuration;
- leaves a first-install backup and an explicit recovery report when safe
  automatic restoration is impossible;
- does not change Wi-Fi or delete unrelated user data.

## Release contract

### Plane Radar release assets

A stable Plane Radar release contains:

```text
planeradar-aarch64-linux-gnu.tar.zst
planeradarctl-aarch64-apple-darwin.tar.zst
planeradarctl-x86_64-apple-darwin.tar.zst
install.sh
release-manifest.json
SHA256SUMS
SBOM.spdx.json
```

The release manifest includes:

- schema version;
- application semantic version;
- full source commit;
- each artifact name, architecture, size, and SHA-256;
- supported target model, OS, architecture, and kernel policy;
- required target packages;
- pinned driver repository, version, commit, and manifest digest;
- minimum compatible control-tool version;
- build workflow identity and timestamp.

### Driver release assets

A stable driver release contains:

```text
hyperpixel2r-kms-source.tar.zst
driver-manifest.json
SHA256SUMS
SBOM.spdx.json
hyperpixel2r-kms-<kernel-release>-aarch64.tar.zst  # when available
```

An exact-kernel bundle contains:

- kernel module;
- device-tree overlay;
- source revision;
- kernel release and architecture;
- module metadata and vermagic;
- module and overlay checksums;
- applied-DTB validation evidence.

### Integrity and provenance

Stable release assets are:

- attached to semantic GitHub Releases;
- built from tagged source by GitHub Actions;
- accompanied by SHA-256 manifests;
- accompanied by SBOMs;
- covered by GitHub artifact attestations;
- verified by the installer before execution or target staging.

Mise manages GitHub CLI for release and attestation verification on the primary
clone-based path. The optional bootstrap must verify the same release contract
before delegating to a downloaded control binary.

Releases begin as drafts or release candidates. They become immutable stable
releases only after the hardware acceptance gate.

## Continuous integration

### Plane Radar CI

Every pull request and main-branch change runs:

- Rust formatting and Clippy;
- unit and integration tests;
- dependency, license, and advisory policy;
- deterministic rendering and screenshot goldens;
- ARM64 Linux application release build;
- Apple Silicon and Intel macOS control-tool builds and tests;
- target installer filesystem fixtures;
- control state-machine tests for interruption, retry, reboot, rollback, and
  resume;
- clean Raspberry Pi OS filesystem fixtures;
- release and driver-lock manifest validation;
- README command validation.

The current smoke test, which only invokes the staged binary's version command,
is not an adequate end-to-end gate and is replaced by control-tool health and
target verification.

### Driver CI

Every driver change runs:

- host protocol tests;
- host GPIO/error-path tests;
- `W=1` build against supported Raspberry Pi kernel headers;
- DKMS package validation;
- device-tree compilation;
- overlay application to a Raspberry Pi Zero 2 W base tree;
- local-fixup and phandle validation;
- module metadata, architecture, vermagic, and checksum validation;
- release-manifest validation;
- GPL and upstream source-provenance checks.

### Hardware acceptance

CI cannot prove physical color, touch orientation, or cold-boot behavior.
Before a stable release, exact release artifacts are deployed to the reference
Pi and an acceptance record captures:

- Pi model and hardware identity;
- OS, architecture, and kernel;
- application and driver versions, commits, and hashes;
- DRM/KMS device and 480-by-480 mode;
- SDL KMSDRM and OpenGL ES renderer;
- colors, radar geometry, QR screen, text layering, and range outline;
- tap, hold, QR dismissal, and range cycling;
- web setup and persisted configuration;
- systemd health and restart count;
- CPU and memory snapshot;
- warm reboot and cold power cycle;
- device-side 480-by-480 screenshot.

The acceptance record is tied to the draft release artifacts. The release is
not considered ready merely because CI uploaded files.

### Clean-room acceptance

Before Plane Radar `v0.1.0`:

1. Image a fresh SD card with 64-bit Raspberry Pi OS Trixie.
2. Configure only networking, SSH, and an admin user in Raspberry Pi Imager.
3. Attach the SD card to the supported Pi and HyperPixel hardware.
4. Clone the public repository on a Mac.
5. Run the documented installation command.
6. Execute no manual command on the Pi.
7. Verify the full display, touch, web, reboot, and service contract.
8. Exercise a release-candidate upgrade and rollback.

This test is the public product boundary.

## Public history and attribution

### Plane Radar history rewrite

At the start of this design pass, the public branch contained 54 Plane Radar
implementation commits on top of 20 commits from the original ESP32 project.
This specification and later planning commits are also included in the rewrite.

The one-time rewrite:

- preserves all original upstream commits, authors, messages, and dates;
- rewrites only Plane Radar work;
- adds exactly one trailer to every Codex-assisted project commit:

  ```text
  Co-authored-by: Codex <noreply@openai.com>
  ```

- folds redundant design/plan/implementation triplets and temporary debugging
  steps into coherent public feature commits where doing so improves history;
- preserves meaningful feature and fix boundaries;
- creates a local recovery bundle before mutation;
- force-pushes `main` once with lease;
- rebuilds and redeploys the rewritten tip;
- performs no stable release before the rewrite is complete.

The project currently has no public Plane Radar tags or GitHub Releases, so
there are no published application release identities to migrate.

Future AI-assisted commits use the same trailer. Human-only contributions do
not receive false Codex attribution.

### AI disclosure

The README states plainly that:

- Shayne and OpenAI Codex collaboratively designed and implemented the project;
- much of the code, tests, and documentation was AI-generated under human
  direction;
- the implementation was repeatedly built, deployed, and accepted on physical
  hardware;
- AI provenance does not replace review, testing, or responsibility.

The disclosure is informational, not marketing copy or a warranty.

## README and public documentation

The README is rewritten using the explicitly requested `$plainspoken-voice`
style. The style applies to public human-facing prose, not the implementation
plan or agent instructions.

README order:

1. project identity and current maturity;
2. real device screenshot;
3. exact supported hardware and OS;
4. shortest installation path;
5. installer effects and reboots;
6. first-run QR and web configuration;
7. status, upgrade, doctor, screenshot, rollback, and uninstall;
8. recovery and durable state;
9. relationship to the ESP32 inspiration and separate driver;
10. AI-development disclosure;
11. licenses, third-party data, fonts, and contributions.

After installing the rewritten release candidate, `planeradarctl screenshot`
triggers the device-side frame capture, retrieves it, validates a 480-by-480
PNG, and stores the accepted image at:

```text
docs/images/planeradar-radar.png
```

The caption identifies it as a frame captured through the real Pi Zero 2 W
display path, not a mockup.

Detailed documentation lives in:

```text
docs/install.md
docs/upgrading.md
docs/recovery.md
docs/architecture.md
docs/development.md
CONTRIBUTING.md
SECURITY.md
```

Obsolete internal execution ledgers and unchecked plans under
`docs/superpowers/` are removed after durable architectural decisions are
incorporated into normal documentation. The approved design and implementation
plan may remain until that consolidation task, but the final public surface
describes the current system rather than the process used to discover it.

## Existing loose ends incorporated by this design

The previous work stopped short in several strict ways. This design explicitly
resolves them:

- undeclared EGL, GLES, and Mesa runtime packages become installer
  dependencies;
- the missing CPU and memory snapshot becomes part of hardware acceptance;
- exact final-revision visual and touch acceptance is repeated after the
  history rewrite and release build;
- the installer root lock-path edge case must return a controlled result rather
  than panic;
- extreme caller-supplied HTTP durations must return a controlled error rather
  than silently saturating;
- stale architecture text about source revision selection is corrected;
- hard-coded maintainer target and canonical hostname assumptions are removed;
- the driver becomes independently versioned;
- release, upgrade, rollback, clean-image, and full target smoke tests are
  added;
- obsolete unchecked plan artifacts are removed from the final public surface.

## Explicit non-goals for version 0.1

- Wi-Fi provisioning;
- Raspberry Pi boards other than Zero 2 W;
- 32-bit Raspberry Pi OS;
- Raspberry Pi OS Desktop images;
- Bookworm or non-Raspberry Pi distributions;
- displays other than HyperPixel 2.1 Round;
- unattended application updates;
- automatic operating-system distribution upgrades;
- guaranteed compatibility with arbitrary future kernels;
- a custom Raspberry Pi OS image;
- a mobile application;
- hiding AI involvement or rewriting third-party authorship.

## Definition of done

Plane Radar `v0.1.0` is ready only when:

1. both public repositories have clean, attributed histories on `main`;
2. both repositories have green CI and immutable, attested stable releases;
3. a fresh Mac clone installs onto a freshly imaged supported Pi using only the
   documented command;
4. no manual Pi-side command is required;
5. installation resumes safely across every planned reboot and a simulated
   interruption;
6. release-candidate upgrade and rollback succeed;
7. display, color, hardware acceleration, touch, QR setup, web configuration,
   and range gestures pass on the reference hardware;
8. a cold power cycle returns to a healthy service without login;
9. `doctor` reports matching application, driver, kernel, overlay, and service
   state;
10. the README screenshot comes from the accepted device build;
11. no tracked file contains the maintainer username or private local defaults;
12. installed revisions and hashes match the published release manifests;
13. public documentation discloses AI assistance and credits the original ESP32
    project and all third-party sources;
14. the two deferred code edge cases have regression tests and fixes;
15. the final acceptance record includes CPU and memory measurements.

## Primary references

- [Raspberry Pi OS](https://www.raspberrypi.com/documentation/computers/os.html)
- [Raspberry Pi headless setup](https://www.raspberrypi.com/documentation/computers/getting-started.html)
- [Raspberry Pi kernel headers](https://www.raspberrypi.com/documentation/computers/linux_kernel.html)
- [Raspberry Pi tryboot](https://www.raspberrypi.com/documentation/computers/raspberry-pi.html)
- [Pimoroni HyperPixel 2.1 Round](https://shop.pimoroni.com/products/hyperpixel-round)
- [GitHub artifact attestations](https://docs.github.com/en/actions/concepts/security/artifact-attestations)
- [GitHub release integrity](https://docs.github.com/en/code-security/how-tos/secure-your-supply-chain/secure-your-dependencies/verify-release-integrity)
- [mise environments](https://mise.jdx.dev/environments/)

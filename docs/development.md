# Develop Plane Radar

Plane Radar is a Rust workspace with a Linux display application and a native
macOS control tool. The application is small enough for a Pi Zero 2 W; the
verification surface is not small, because boot loaders are famously
unimpressed by confidence.

## Tooling

Install the pinned tools:

```sh
mise install
mise run verify
mise run docs-check
```

`mise run verify` checks formatting, Clippy with warnings denied, the complete
workspace through cargo-nextest, and dependency policy. On Linux, native
application checks also need SDL2 development headers and `pkg-config`.

The ARM64 application build uses Docker Buildx with a pinned 64-bit Raspberry
Pi OS Trixie environment. Docker Desktop and OrbStack both provide compatible
engines on macOS.

## Workspace map

- `src/` contains the display application, renderer, ADS-B and geocoding
  clients, settings server, target-side installer, and capture protocol.
- `crates/planeradarctl/` contains the macOS CLI, release verification,
  OpenSSH transport, resumable transactions, external driver adapter, and
  lifecycle operations.
- `tests/fixtures/pi-os-trixie/` is a synthetic regular-file target used for
  end-to-end install and resume tests. It contains no live filesystem data.
- `release/` and `scripts/package-release.sh` define the release contract.
- `driver.lock.toml` is the only application-owned pointer to the external
  driver release.

The driver source and boot scripts live in
[shayne/hyperpixel2r-kms](https://github.com/shayne/hyperpixel2r-kms). Do not
copy them back into this repository. Use:

```sh
mise run driver:sync
```

The verified source is cached under ignored `.cache/driver` state.

## Documentation checks

```sh
mise run docs-check
```

The documentation contract extracts fenced README commands, checks that every
`mise run` task exists, validates local Markdown links, verifies the accepted
480×480 RGBA device capture, requires public credit and disclosure, and scans
tracked files for maintainer-specific targets and secret markers.

`mise run readme-commands` remains an alias because CI used that name before
the check grew up.

## Package a local release

The build requires a clean tracked source because the revision in the
manifest must identify the bytes being packaged:

```sh
mise run package-release -- 0.1.0-rc.N
```

The release directory contains:

```text
planeradar-aarch64-linux-gnu.tar.zst
planeradarctl-aarch64-apple-darwin.tar.zst
planeradarctl-x86_64-apple-darwin.tar.zst
install.sh
release-manifest.json
SHA256SUMS
SBOM.spdx.json
```

Archives use normalized ownership and timestamps derived from the source
commit. The manifest binds the application, both control binaries, supported
hardware, and exact driver lock.

To exercise an assembled release without publishing it:

```sh
mise run install -- user@host --release-dir /absolute/path/to/release
```

The command decides which verification path applies:

- A local `--release-dir` verifies its local manifest, checksums, and release
  identity.
- An explicit source-controller release candidate selected with `--version`
  verifies its release-candidate manifest, checksums, and release identity, but
  skips the stable-only GitHub release and attestation policy.
- Stable source-controller versions add `gh release verify` and runnable
  artifact attestations.

The separate release bootstrap verifies release-candidate attestations before
executing the downloaded controller. “Published” is not one security mode
here; the entry point and release maturity decide which checks run.

## CI and release flow

CI runs the full workspace and release contracts on Linux ARM64, then builds
and tests `planeradarctl` on Apple Silicon and Intel macOS runners. The release
workflow resolves one exact commit, builds each platform artifact, packages
the release twice to prove determinism, validates checksums and metadata,
generates an SPDX SBOM, attests the subjects, and creates a draft prerelease.

Stable publication is a separate acceptance decision. A green archive build
does not prove a round panel displayed the right colors or that a tryboot
survived a cold power cycle.

[Releasing](releasing.md) documents the candidate, tagless stable draft, exact
hardware acceptance, and no-rebuild promotion commands.

## Fixture and live smoke

The fixture end-to-end test is part of normal verification:

```sh
mise exec -- cargo test --locked --test ctl_end_to_end
```

Maintainers can run a read-only smoke against an installed candidate:

```sh
mise run smoke-pi -- user@host
```

It runs `status`, `doctor --json`, and `screenshot`, then compares the installed
application and driver identities with `dist/release`. It does not install,
upgrade, reboot, or repair the Pi.

## Screenshots and private data

The README image at `docs/images/planeradar-radar.png` is a user-accepted
physical-device capture. Release acceptance replaces it only through the
tested screenshot operation.

Debug frames may contain live callsigns. `.env` may contain a private target.
Target state contains hardware identity. None belongs in fixtures, test
output, issue attachments, or commits. Use synthetic model and serial values
in tests, review `doctor --json` before sharing it, and keep `.env` ignored.

## Making changes

Start with a failing test that crosses the same boundary as the bug. A
renderer change needs a golden or geometry test; an installer change needs
interruption and idempotence coverage; a release change needs hostile metadata
fixtures; a boot change belongs in the driver repository and needs tryboot
recovery evidence.

Then run:

```sh
mise run verify
git diff --check
```

See [Contributing](../CONTRIBUTING.md) for public contribution rules and
[Architecture](architecture.md) for the state model.

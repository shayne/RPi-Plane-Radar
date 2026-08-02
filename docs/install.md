# Install Plane Radar

The installer does the interesting work from the Mac because the Pi is the
thing being changed. You should not need to copy binaries, edit boot files, or
run a second set of Pi-side commands.

## Supported setup

Plane Radar currently supports exactly:

- Raspberry Pi Zero 2 W;
- Pimoroni HyperPixel 2.1 Round attached to the GPIO header;
- 64-bit Raspberry Pi OS Lite Trixie;
- a macOS 14 or newer install host;
- Git, mise, OpenSSH, an authenticated GitHub CLI (`gh`) session for release
  verification, and at least 16 GiB of free Mac disk;
- Docker Desktop, OrbStack, or an equivalent Docker Buildx engine for an
  exact-kernel driver fallback; and
- an existing SSH public-key login to the Pi with interactive sudo.

The Pi needs working Wi-Fi, correct system time, package-repository access,
2 GiB free on `/`, 128 MiB free on `/boot/firmware`, and port 80 available.
Its default systemd target must be `multi-user.target`, not a desktop display
manager. Plane Radar does not provision Wi-Fi or SSH.

Use Raspberry Pi Imager to create 64-bit Raspberry Pi OS Lite Trixie. Configure
networking, your SSH public key, an admin username, and an initial hostname in
the imager. The username may be `pi`, but modern Raspberry Pi OS does not
promise that default. Use the account you actually created.

## Source install

On the Mac:

```sh
git clone https://github.com/shayne/RPi-Plane-Radar.git
cd RPi-Plane-Radar
mise install
mise run install -- user@host
```

The default command resolves and installs the latest immutable stable release.
To reproduce the first accepted release exactly, pin `v0.1.0`:

```sh
mise run install -- user@host --version 0.1.0
```

`--version` and `--release-dir` are mutually exclusive. Maintainers accepting
an already assembled release can use:

```sh
mise run install -- user@host --release-dir /absolute/path/to/release
```

A local directory is not a trust bypass. The controller still checks the
manifest, checksums, repository, source commit, architecture, driver lock, and
release identity before using it. A local stable draft must pass the exact
artifact-attestation checks described below even though its GitHub release is
not public yet. A published stable release must also pass GitHub release
verification.

## Optional `.env`

Copy `.env.example` to `.env` when you use the same target repeatedly:

```sh
cp .env.example .env
```

The file accepts only:

```dotenv
PLANERADAR_PI_TARGET=pi@raspberrypi.local
PLANERADAR_HOSTNAME=planeradar
PLANERADAR_DOCKER_CONTEXT=
```

`.env` is ignored and optional. It must not contain passwords, tokens, Wi-Fi
credentials, coordinates, or SSH keys. A command-line target, hostname, or
Docker context wins over `.env`; `.env` wins over the generic hostname
default. The target has no implicit private default inside `planeradarctl`.

With `.env` in place, the command can omit repeated values:

```sh
mise run install
```

## What the installer checks

Before mutation, every source install verifies:

- macOS version and architecture, required tools, Docker Buildx, free space,
  system time, and GitHub repository access;
- the target's OpenSSH host key, model, serial, OS, architecture, kernel,
  headers, default systemd target, boot filesystem, tryboot support, free
  space, package access, port 80, and existing display state;
- the application manifest, checksums, full source commit, architecture, SPDX
  metadata, repository, workflow, tag, and release identity; and
- the external driver repository, version, full commit, manifest digest,
  kernel release, vermagic, overlay, and artifact hashes.

The source controller keeps a stable-only attestation policy. A local stable
draft verifies each runnable artifact against the exact repository, workflow,
branch, and source commit without requiring a tag that does not exist yet. A
published stable release selected with `--version` additionally runs GitHub
release verification. Explicit source release candidates still enforce
manifests, checksums, and release identity, but they do not run those
stable-only `gh release verify` and `gh attestation verify` checks. The
separate release bootstrap verifies release-candidate attestations before it
executes the downloaded controller.

If the target is absent from both the command and `.env`, the controller asks
for it only when standard input is an interactive terminal and
`--non-interactive` is not set. It never prompts for the hostname; the
documented default remains `planeradar`. `--non-interactive` fails when the
target is missing. Redirected or piped input also fails without reading input
or writing a prompt.

## What changes on the Pi

The target installs only the required runtime and driver-build packages. It
does not run a full OS upgrade.

The application lives under `/opt/planeradar`; its service unit is
`/etc/systemd/system/planeradar.service`; private application state lives under
`/var/lib/planeradar`; and controller transaction state lives under
`/var/lib/planeradar-installer`. The external driver owns
`/usr/lib/hyperpixel2r-kms`, `/var/lib/hyperpixel2r-kms`, its exact module,
overlay, DKMS source when needed, and only the boot lines recorded by its
transaction.

The first driver activation uses one-shot tryboot. A healthy trial is verified
before normal boot changes are committed. The hostname change and final
verification may require another reboot. The controller reconnects only after
the SSH host key, Pi model, and serial match the recorded target.

Each successful phase is saved on both sides before the next operation. If the
Mac closes, the network drops, or a reboot takes too long, rerun the exact same
install command. Do not delete state to force progress; see
[Recovery](recovery.md).

## First-run setup

With no saved location, the display stays on the setup QR screen. It shows:

- `http://planeradar.local` when using the default hostname; and
- the discovered numeric `http://` URL.

Open one URL from the same LAN. Search for a place through Nominatim or enter
latitude and longitude manually, then save units, runway visibility, and
range. Browser geolocation is not requested. Search text is cached only within
the private Pi state, and the selected location is never part of health output
or normal logs. Location, range, and runway visibility remain the complete
default setup path; existing installations keep their current display until an
owner enables an optional feature.

Radar text size changes radar typography from 80% through 130%. Three native
expandable groups hold the opt-in controls:

- **Aircraft labels:** **Show callsign** defaults on. **Show origin and
  destination** and **Show expanded aircraft model** default off.
- **Footer:** separately select **Weather condition**, temperature, humidity,
  time, and date. Temperature can use Celsius or Fahrenheit. Time and date can
  use Radar location or Zulu, and time can use a 12-hour or 24-hour clock.
- **Traffic filter:** Minimum altitude and Maximum altitude are optional and
  always use feet, independent of the distance unit. Blank bounds are open.
  With a minimum or maximum bound active, unknown-altitude aircraft are hidden.

Provider-backed enrichment and environment data are off by default; all new
provider features are optional. Routes send the aircraft callsign to ADSBDB;
expanded models send the aircraft identifier. When both are selected, those
values may share one request. Weather and radar-local time send the configured
coordinates to Open-Meteo. Zulu-only time and date send nothing to Open-Meteo.
The settings page explains these boundaries beside the affected controls.

## Verified release bootstrap

Release assets contain a native macOS control binary and `install.sh`. Download
the bootstrap with GitHub CLI:

```sh
gh release download -R shayne/RPi-Plane-Radar --pattern install.sh --output planeradar-install.sh
bash planeradar-install.sh user@host
```

The script requires macOS and an authenticated `gh` session. It resolves the
exact tag, rejects draft or mismatched releases, verifies GitHub release
integrity for stable releases and release candidates, downloads bounded
metadata into a private temporary directory, checks all hashes, verifies
attestations, selects the native control archive, and hands terminal control
to the verified binary. Its temporary directory is removed after success or
failure.

Do not pipe a network response into a shell. That path is shorter because it
leaves the verification step on the cutting-room floor.

## Confirm the result

```sh
mise run status -- user@planeradar.local
mise run doctor -- user@planeradar.local
mise run screenshot -- user@planeradar.local --output planeradar-radar.png
```

`doctor` exits successfully only when the installed release, driver, kernel,
DRM mode, touch, renderer, service, HTTP endpoint, and mDNS result agree.

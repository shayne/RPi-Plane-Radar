# RPi Plane Radar Design

Status: approved in conversation  
Date: 2026-07-25  
Target repository: `shayne/RPi-Plane-Radar`  
Upstream: `MatixYo/ESP32-Plane-Radar` at `69c10785afbc`

## Context

This project ports the ESP32 Plane Radar experience to a Raspberry Pi Zero 2 W
with a Pimoroni HyperPixel 2.1 Round touchscreen. The upstream firmware renders a
240×240 circular ADS-B radar, stores a small set of preferences, offers a local
web configuration portal, and uses a physical button to change range or reset
configuration.

The Raspberry Pi runs Raspberry Pi OS Lite 64-bit (Debian 13) without a desktop.
Its installed kernel includes the `vc4-kms-dpi-hyperpixel2r` overlay and exposes
rotation, backlight, and touch-axis parameters. The display is not configured
yet. The Pi already has working Wi-Fi, SSH, mDNS, and access to the adsb.fi API.

## Goals

- Reproduce the upstream radar's visual identity and behavior at 480×480.
- Run directly on DRM/KMS without X11, Wayland, Chromium, or a desktop session.
- Replace the ESP32 button with simple whole-screen touch gestures.
- Provide QR-driven local web configuration without managing operating-system
  Wi-Fi.
- Start automatically at boot and recover from transient failures.
- Be installable, testable, documented, and suitable for public open-source use.
- Run the same committed revision on the Pi that is published to GitHub.

## Non-goals

- Captive-portal or Wi-Fi credential management.
- A touch-heavy redesign with permanent on-screen controls.
- User accounts, remote Internet access, or cloud storage.
- Browser geolocation over local HTTP.
- A general-purpose ADS-B receiver; aircraft data continues to come from
  adsb.fi.

## Architecture

The application will be a Python package using Pygame/SDL2 for direct KMS
rendering. It will not start a graphical desktop. A small standard-library HTTP
server will run in a background thread for configuration. Shared runtime state
will pass through a narrow, thread-safe application model.

This will be an independent derivative project, not a GitHub fork. The
repository keeps the upstream Git history, MIT license, and attribution so its
provenance remains explicit. The ESP32-specific PlatformIO implementation will
be replaced by the Raspberry Pi application, tests, installer, service unit,
and documentation. The original repository will remain configured as the
`upstream` remote even though ongoing merge compatibility is not a goal.

### Components

| Component | Responsibility |
| --- | --- |
| `app` | Startup, event loop, state transitions, scheduling, and shutdown |
| `display` | Pygame/SDL initialization, KMS display selection, and frame presentation |
| `radar` | Pure radar geometry and 480×480 rendering |
| `touch` | Tap and long-press recognition from SDL/Linux input events |
| `adsb` | TLS-verified adsb.fi requests, response parsing, and immutable snapshots |
| `airports` | Embedded large-airport/runway data and projection |
| `settings` | Validation, defaults, atomic persistence, and update notifications |
| `web` | Local responsive settings page and form endpoints |
| `geocode` | Explicit address/place lookup with caching and rate limiting |
| `setup_screen` | QR code, mDNS URL, IP URL, and network/status messages |

The production service will run as a dedicated `planeradar` system user. It will
have only the groups needed for DRM and input devices plus
`CAP_NET_BIND_SERVICE` for port 80. Persistent data will live in
`/var/lib/planeradar`. Application files will live in `/opt/planeradar`.

## Display and Interaction

The supported kernel overlay will drive the HyperPixel as a 480×480 KMS display.
The installer will add one idempotent overlay declaration:

```text
dtoverlay=vc4-kms-dpi-hyperpixel2r
```

The existing `vc4-kms-v3d` configuration remains in place. Standard I2C must
remain disabled as required by the display overlay. Rotation and touch-axis
parameters will be selected during live hardware verification and recorded by
the installer.

The radar uses the upstream layout at exactly 2× its original pixel dimensions:

- dark blue circular field;
- four subdued green rings and crosshairs;
- white N/S/E/W labels and center dot;
- range label on the east spoke;
- teal runway lines and airport identifiers;
- red aircraft heading triangles;
- magenta track vectors;
- white, yellow, and blue aircraft tags; and
- red rim dots for traffic beyond the outer ring.

Pygame will maintain a cached static background containing the grid and runway
overlay. Each ADS-B update copies that background and draws the current aircraft
snapshot, avoiding partial frames and flicker.

### Gestures

- A short tap anywhere advances 5 → 10 → 15 → 25 km and persists the choice.
- A continuous three-second hold anywhere opens the QR/settings screen.
- Once a long press fires, releasing it does not also count as a tap.
- When a location is configured, tapping the QR/settings screen returns to the
  radar.
- Before initial location setup, the QR screen remains until valid coordinates
  are saved.

Gesture recognition will include movement tolerance and debounce so a slightly
moving finger does not trigger both actions.

## Settings and First-run Flow

The minimum required setting is a valid latitude/longitude pair. With no saved
location, the application displays:

- a QR code encoding `http://planeradar.local`;
- `http://planeradar.local` as text;
- the current `http://<IP>` URL as a fallback; and
- a short instruction to open the page and set the radar location.

The web server starts before the display enters this state and stays available
while the radar is running.

The settings page provides:

- explicit address or place search;
- selectable geocoding results;
- manual latitude and longitude;
- kilometres or miles;
- runway overlay on or off; and
- the current range preset.

Address lookup uses the public OpenStreetMap Nominatim endpoint only after the
user submits a search. It will not implement autocomplete. Requests will use an
identifying User-Agent, remain below one request per second, cache results, show
OpenStreetMap attribution, and tell the user that the entered place is sent to
OpenStreetMap. The backend provider URL will be configurable. Manual coordinates
remain available if geocoding is unavailable.

Browser geolocation is intentionally omitted because the Geolocation API is
restricted to secure contexts and the product uses an ordinary local HTTP URL.

Settings will use a versioned JSON document in
`/var/lib/planeradar/settings.json`. Writes will validate the complete proposed
document, write and fsync a temporary file, then atomically replace the previous
file. Invalid or interrupted writes cannot destroy the last valid settings.

The settings server is intentionally unauthenticated on the local network,
matching the upstream product model. Mutating forms will include a CSRF token
and reject unexpected origins. The server exposes no shell, file browser, or
arbitrary URL-fetch functionality.

## Aircraft and Geographic Data

The ADS-B client will request:

```text
https://opendata.adsb.fi/api/v3/lat/<lat>/lon/<lon>/dist/<nautical-miles>
```

It will poll every three seconds, stay comfortably below the documented public
rate limit, use bounded connect/read timeouts, verify TLS certificates, and cap
the rendered aircraft count. Parsing preserves the upstream preference order
for heading, track, speed, callsign, type, and altitude fields. Ground aircraft
remain hidden by default.

Screen projection will preserve the upstream visual semantics while correcting
its longitude scaling approximation. A local tangent/equirectangular projection
with the cosine of mean latitude is sufficiently accurate over the maximum
roughly 33 km radar radius. Tests will cover cardinal bearings, distance,
projection, rim placement, and high-latitude behavior.

Airport/runway data remains derived from OurAirports. The existing data
generation path will be ported so the embedded dataset can be refreshed
reproducibly.

## Runtime States and Failure Handling

The display has four explicit states:

1. `SETUP_REQUIRED`: QR screen until valid coordinates exist.
2. `WAITING_FOR_NETWORK`: configured, but no usable network address or ADS-B
   connectivity yet.
3. `RADAR`: current or recently successful aircraft snapshot.
4. `SETTINGS`: QR screen opened by a three-second hold.

Brief ADS-B failures keep the last successful aircraft snapshot. After 30
seconds without fresh data, a small `DATA STALE` notice appears without
destroying the radar view. It clears after the next successful update.

Network loss never invokes Wi-Fi setup or changes NetworkManager. The app keeps
retrying with bounded backoff. Failed address searches leave existing
coordinates untouched and return a useful error with manual-coordinate entry.

Missing touch input logs a warning but does not prevent rendering or web
configuration. Display initialization failure exits non-zero so systemd can
restart the process. Logs go to the journal and must not contain address search
text or other unnecessary location details.

## Installation and Service Management

The installer will:

1. verify a supported Raspberry Pi OS environment;
2. install Debian-packaged runtime dependencies;
3. create the `planeradar` service account and persistent directory;
4. install the current checkout into `/opt/planeradar`;
5. configure the HyperPixel overlay idempotently, preserving a boot-config
   backup before the first change;
6. install and enable the systemd unit; and
7. reboot only when boot configuration changed.

Expected runtime packages include Python 3, Pygame/SDL2, Pillow, QR-code support,
requests, and a packaged sans-serif font. No global `pip` installation is
required on Debian 13.

The systemd unit starts after the network-online target, restarts on failure
with bounded delay, runs without root, grants only low-port capability, and
restricts writable paths to `/var/lib/planeradar`. Hardening options will be
applied only when they do not hide the required DRM or input devices.

## Testing

### Automated

- ADS-B response fixtures, missing fields, ground filtering, and failures.
- Range cycling, unit labels, and settings persistence.
- Latitude/longitude validation and correct geographic projection.
- Address-search throttling, cache hits, provider failures, and result parsing.
- HTTP first-run flow, CSRF handling, manual coordinates, and settings updates.
- Tap, movement tolerance, long press, and long-press release behavior.
- Deterministic 480×480 renderer fixtures and approved golden PNGs.
- Installer idempotence and boot-config editing against temporary fixtures.
- Static checks and tests in GitHub Actions on pushes and pull requests.

### Live Pi acceptance

- KMS reports a connected 480×480 HyperPixel display.
- Linux/SDL reports touch input with the correct orientation.
- The first-run QR and both HTTP URLs are legible on the physical screen.
- A phone can search for a place, select it, and start the radar.
- Settings persist across application and operating-system restarts.
- Live ADS-B aircraft and runway overlays render correctly.
- Tap and three-second hold gestures behave exactly as designed.
- The app recovers after service restart and network interruption.
- A cold boot reaches the application without manual login.
- A debug capture of the rendered surface matches the physical orientation and
  approved visual layout.

## Publishing

The public repository will be `github.com/shayne/RPi-Plane-Radar`. It will:

- be created as an independent repository rather than through GitHub's fork
  mechanism;
- preserve the upstream Git history;
- keep `MatixYo/ESP32-Plane-Radar` as the `upstream` remote;
- retain the MIT license and acknowledge the original project;
- document hardware, installation, configuration, gestures, architecture, and
  troubleshooting;
- include representative screenshots and GitHub Actions status; and
- publish the exact commit installed on the Pi.

The repository will be created and pushed only after the implementation and live
acceptance checks pass.

## Risks and Mitigations

- **Touch support differs between product-page wording and the installed
  overlay.** The live kernel advertises touch parameters, but acceptance requires
  verifying the actual input device after enabling the overlay. Web settings
  remain usable if touch needs follow-up calibration.
- **Direct KMS behavior can vary by SDL build.** Debian 13 supplies Pygame and
  SDL2 packages. A minimal display probe will be the first implementation slice
  before building the full renderer.
- **Physical rotation is unknown until the panel is active.** Rotation and
  touch-axis configuration will be calibrated together and captured
  declaratively.
- **Nominatim is a shared public service.** Explicit searches, caching, rate
  limiting, configurable provider URL, and manual coordinates avoid making it a
  hard dependency.
- **mDNS may resolve slowly on some phones.** The QR uses the stable `.local`
  name while the screen also shows the current numeric IP URL.

## References

- Upstream project: <https://github.com/MatixYo/ESP32-Plane-Radar>
- HyperPixel 2.1 Round: <https://shop.pimoroni.com/en-us/products/hyperpixel-round>
- Nominatim usage policy:
  <https://operations.osmfoundation.org/policies/nominatim/>
- W3C Geolocation API: <https://www.w3.org/TR/geolocation/>

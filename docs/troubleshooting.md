# Troubleshooting

Start with the service, health, and current-boot logs:

```sh
systemctl is-enabled planeradar
systemctl is-active planeradar
systemctl status planeradar --no-pager --full
curl --fail -H 'Host: planeradar.local' http://127.0.0.1/healthz
sudo journalctl -b -u planeradar --no-pager
systemctl --failed
```

Normal logs intentionally omit location, place searches, aircraft responses,
form bodies, session values, and CSRF tokens. Treat settings files and debug
frames as private data when collecting diagnostics.

## Build cannot reach Docker or OrbStack

`mise run build-pi` needs a running Docker-compatible Buildx engine:

```sh
docker info
docker buildx ls
orbctl status
orbctl start
```

The build also refuses a dirty tracked workspace because its revision must
identify the exact source archive:

```sh
but status
git diff --check
```

Install the pinned tools with `mise install`. On Linux, install
`libsdl2-dev` and `pkg-config` before native checks. Do not copy a host-native
binary to the Pi; the verified artifact must report `ARM aarch64`.

## Display is blank or wrongly rotated

Check the KMS connector and service renderer:

```sh
for status in /sys/class/drm/*/status; do
  printf '%s: ' "$status"
  cat "$status"
done
for modes in /sys/class/drm/*/modes; do
  printf '%s:\n' "$modes"
  cat "$modes"
done
sudo journalctl -b -u planeradar --no-pager |
  grep -E 'SDL display ready|SDL failure|kmsdrm|opengles2'
```

The accepted result is one connected DPI connector with a 480×480 mode and a
readiness line containing `video_driver=KMSDRM` (case may vary) and
`render_driver=opengles2`.

Do not experiment directly in the normal boot configuration. The custom panel
driver uses revisioned overlays and a one-shot tryboot acceptance flow. Follow
[the HyperPixel runbook](hardware/hyperpixel2r-driver.md) for rotation,
driver rebuilds, kernel upgrades, commit, or rollback. Supported calibration
parameters are `rotate=0|90|180|270`, `touchscreen-inverted-x`,
`touchscreen-inverted-y`, and `touchscreen-swapped-x-y`; test one change at a
time.

## Touch does not respond or axes are wrong

Identify the bound FT5x06 input device:

```sh
cat /proc/bus/input/devices
readlink -f /sys/class/input/event0/device
udevadm info --query=property --name=/dev/input/event0
sudo evtest /dev/input/event0
```

The device ancestry should descend from
`/sys/devices/platform/planeradar-hyperpixel2r/`, and its X/Y axes should span
the native 0–479 surface. Confirm the service account and device policy:

```sh
id planeradar
systemctl show planeradar \
  -p User -p SupplementaryGroups -p DevicePolicy -p DeviceAllow
```

The unit should use `video render input`, `DevicePolicy=closed`,
`DeviceAllow=char-drm rw`, and `DeviceAllow=char-input r`. Device-path
wildcards are not valid systemd device allow rules.

If raw coordinates are correct but the UI action is not, remember that motion
beyond 18 pixels cancels a tap and a long press must remain continuous for
three seconds. Releasing a completed long press intentionally causes no second
tap.

## `planeradar.local` does not resolve

The installer enables Avahi, but the client and Pi must be on the same
multicast-capable LAN:

```sh
hostname
systemctl is-active avahi-daemon
ip -4 address
ip route
getent hosts planeradar.local
```

Use the `http://<ip-address>` shown on the round display when `.local` is not
available. Plane Radar does not repair Wi-Fi or change network configuration;
fix networking through Raspberry Pi OS.

Requests sent directly to `127.0.0.1` need an allowed Host header:

```sh
curl --fail -H 'Host: planeradar.local' http://127.0.0.1/healthz
```

## Port 80 is already in use

```sh
sudo ss -ltnp '( sport = :80 )'
sudo systemctl status planeradar --no-pager
```

Stop or reconfigure the conflicting service. Plane Radar binds port 80 with
only `CAP_NET_BIND_SERVICE`; do not run the whole application as root.

## Radar says `WAITING FOR NETWORK`

This state means a location is saved but the current location has not produced
one successful ADS-B response since startup or a location change.

```sh
curl --fail -H 'Host: planeradar.local' http://127.0.0.1/healthz
ip route
getent ahosts opendata.adsb.fi
timedatectl status
sudo journalctl -b -u planeradar --no-pager
```

Check routing, DNS, system time, CA certificates, and upstream availability.
The client uses HTTPS-only bounded requests. It retries with backoff and wakes
immediately after a settings change.

## Radar says `DATA STALE`

After 30 seconds without fresh data, the last valid aircraft remain visible
with a stale notice. This is intentional. Check the same route/DNS/time items
above. Fresh data automatically clears the notice; restarting or deleting
settings is not required.

## Place search fails

The page keeps manual latitude/longitude entry available whenever Nominatim
search fails. Check system time, CA certificates, DNS, and access to
`nominatim.openstreetmap.org`.

Search misses are rate-limited to one request start per 1.05 seconds.
Successful results are cached privately for seven days. An empty result is not
an application error; try a more specific place name or enter coordinates
manually.

## Settings file is invalid

Invalid JSON, unknown fields, schema versions other than 1, out-of-range
coordinates, and range indices outside 0–3 are rejected rather than silently
replaced. The service may enter its restart policy until the file is repaired.

Preserve a private root-only copy, remove the invalid live file, and let the
application return to mandatory setup:

```sh
sudo systemctl stop planeradar
sudo install -m 0600 -o root -g root \
  /var/lib/planeradar/settings.json /root/planeradar-settings.invalid.json
sudo rm /var/lib/planeradar/settings.json
sudo systemctl start planeradar
```

Open the QR URL and save a new location. Do not paste the backup into public
issues or logs.

## Service installation or permissions fail

Verify the installed types, owners, modes, unit syntax, and artifact identity:

```sh
sudo systemd-analyze verify /etc/systemd/system/planeradar.service
sudo stat -c '%a %U:%G %n' \
  /opt/planeradar/bin/planeradar \
  /opt/planeradar/REVISION \
  /opt/planeradar/SHA256 \
  /var/lib/planeradar \
  /var/lib/planeradar/settings.json
sudo sha256sum /opt/planeradar/bin/planeradar
sudo cat /opt/planeradar/REVISION
sudo cat /opt/planeradar/SHA256
```

Expected modes are 0755 for the binary, 0644 for provenance and the unit, 0750
for the state directory, and 0600 for settings. The installer refuses symlink
or special-file destinations rather than following them.

Re-run the exact staged installer to repair supported content or mode drift.
It verifies all source sidecars again and reports whether anything changed.

## Capture and inspect the logical frame

```sh
sudo rm -f /var/lib/planeradar/debug.png
sudo systemctl kill --signal=SIGUSR1 planeradar
sudo file /var/lib/planeradar/debug.png
sudo sha256sum /var/lib/planeradar/debug.png
```

The file must be a fresh decodable 480×480 RGBA PNG. It represents the logical
renderer output and helps separate drawing defects from physical orientation
or scanout defects. It can contain live callsigns.

## Check journal privacy and restart loops

```sh
systemctl show planeradar -p MainPID -p NRestarts -p SubState
sudo journalctl -b -u planeradar --no-pager
```

Repeated renderer, permission, settings, or bind errors are not normal. Avoid
sharing raw settings or frames. Before publishing a diagnostic excerpt, verify
that it contains no coordinates, place/query text, aircraft response data,
cookies, CSRF values, form bodies, or other local identifiers.

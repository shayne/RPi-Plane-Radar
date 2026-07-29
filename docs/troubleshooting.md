# Troubleshooting

Start from the Mac:

```sh
mise run status -- user@host
mise run doctor -- user@host
mise run doctor -- user@host --json
```

`status` is concise. `doctor` is deliberately picky: the app revision and
hash, driver release, kernel, module, overlay, service, renderer, touch, HTTP,
and mDNS all need to describe the same accepted installation. "Most of it is
green" is how boot drift gets promoted into normal state.

If installation or a lifecycle command stopped halfway through, rerun the same
command before doing manual repair. See [Recovery](recovery.md).

## The controller cannot reach the Pi

Confirm that the target text is the account you created in Raspberry Pi
Imager, followed by the current hostname or IP:

```sh
mise run status -- pi@raspberrypi.local
```

Plane Radar accepts `user@hostname` or `user@IPv4`, uses batch-mode OpenSSH for
probes, and requires the existing public-key login. It does not accept a URL,
password, or an SSH option disguised as a hostname.

If the installer already changed the hostname, try the desired `.local` name.
During resume the controller probes both the original and desired names, then
accepts only the host whose key, model, and serial match its durable record.

For a changed host key, stop and confirm the physical Pi before editing
`known_hosts`. A reimaged SD card is a new target, not a continuation of the
old transaction.

## Preflight rejects the Mac

The supported host is macOS 14 or newer with Git, mise, OpenSSH, GitHub CLI,
and a Docker Buildx engine. Check the tools:

```sh
mise install
gh auth status
docker info
docker buildx ls
```

OrbStack users can also check:

```sh
orbctl status
orbctl start
```

The controller needs 16 GiB of free Mac disk for verified downloads, extracted
payloads, target kernel context, and a fallback driver build. Its private cache
is `~/.cache/planeradar`.

## Preflight rejects the Pi

The accepted target is Raspberry Pi Zero 2 W, 64-bit Raspberry Pi OS Lite
Trixie, ARM64, `multi-user.target`, `/boot/firmware`, working tryboot, correct
system time, reachable package repositories, port 80 free, matching kernel
headers, 2 GiB free on `/`, and 128 MiB free on `/boot/firmware`.

A desktop image fails because its display manager competes for the same DRM
device. Debian 12 fails because it is not the accepted OS. A different Pi may
be perfectly capable and still outside the tested recovery contract.

The installer adds required runtime and build packages, but it does not run
`apt full-upgrade`. If the installed headers do not match the running kernel,
reboot into the already installed kernel first or install the matching headers
through normal Raspberry Pi OS administration.

## The display is blank or corrupted

Run:

```sh
mise run doctor -- user@host
```

Look for a kernel mismatch, module mismatch, overlay mismatch, wrong DRM
device, wrong mode, or wrong renderer. The accepted runtime is one DPI
connector at 480×480, SDL `KMSDRM`, and renderer `opengles2`.

When SSH still works, these read-only target probes can separate scanout from
application failure. Start a Pi shell, then run the probes there:

```sh
ssh user@host
```

```sh
for status in /sys/class/drm/*/status; do
  printf '%s: ' "$status"
  cat "$status"
done
for modes in /sys/class/drm/*/modes; do
  printf '%s:\n' "$modes"
  cat "$modes"
done
systemctl show planeradar -p ActiveState -p SubState -p NRestarts
sudo journalctl -b -u planeradar --no-pager
```

Do not edit the normal boot configuration to experiment. The external driver
uses revisioned artifacts and one-shot tryboot precisely so an incorrect
candidate does not become the default. Use
[driver recovery](https://github.com/shayne/hyperpixel2r-kms/blob/main/docs/operations.md).

## Touch does not respond or points the wrong way

`doctor` reports whether the expected touch device exists. For lower-level
inspection, start a Pi shell:

```sh
ssh user@host
```

Then run:

```sh
cat /proc/bus/input/devices
udevadm info --query=property --name=/dev/input/event0
sudo evtest /dev/input/event0
```

The FT5x06 device should descend from the bound `hyperpixel2r-kms` platform
device and span native coordinates 0–479. The service account needs the
`input` supplementary group and its systemd device policy must allow
`char-input r`.

If raw coordinates are right but a gesture is ignored, motion beyond 18 pixels
cancels a tap. A long press must remain continuous for three seconds, and its
release is consumed. That last rule prevents one hold from also changing
range.

## The local URL does not resolve

The installed hostname determines the `.local` URL. With the default:

```sh
open http://planeradar.local
```

The Mac and Pi must share a multicast-capable LAN. The installer enables
Avahi, but it does not change Wi-Fi or router policy. Use the numeric
`http://<ip-address>` printed on the round display when mDNS is unavailable.

Start a Pi shell:

```sh
ssh user@host
```

Then check hostname, Avahi, and routing:

```sh
hostname
systemctl is-active avahi-daemon
ip -4 address
ip route
getent hosts planeradar.local
```

Requests to loopback require an accepted Host header:

```sh
curl --fail -H 'Host: planeradar.local' http://127.0.0.1/healthz
```

## Port 80 is occupied

Plane Radar binds port 80 with `CAP_NET_BIND_SERVICE`; it does not run the
whole application as root.

```sh
ssh user@host
```

On that Pi shell:

```sh
sudo ss -ltnp '( sport = :80 )'
systemctl status planeradar --no-pager
```

Stop or reconfigure the conflicting service, then resume the original install.

## The radar waits for network

`WAITING FOR NETWORK` means a location is saved but the current location has
not received one successful ADS-B response since startup or the latest
location change.

```sh
ssh user@host
```

On that Pi shell:

```sh
ip route
getent ahosts opendata.adsb.fi
timedatectl status
sudo journalctl -b -u planeradar --no-pager
```

Check routing, DNS, system time, CA certificates, and upstream availability.
The ADS-B client uses HTTPS-only bounded requests. It backs failures off and
wakes immediately after a settings change.

`DATA STALE` is different. It appears after 30 seconds without fresh traffic
and keeps the last valid aircraft visible. Fresh data clears it; deleting
settings does not help.

## Place search fails

Manual latitude and longitude remain available when Nominatim is unavailable.
Check time, CA certificates, DNS, and access to
`nominatim.openstreetmap.org`.

Search misses begin at least 1.05 seconds apart. Successful results are cached
privately for seven days, and no more than five are returned. An empty result
is not an application failure; use a more specific place or enter coordinates
manually.

## Settings are invalid

The settings parser rejects unknown fields, unsupported schema versions,
out-of-range coordinates, and range indices outside 0–3. Preserve a private
copy before removing a bad file. Start a Pi shell:

```sh
ssh user@host
```

Then stop the service, preserve the file, and reset only settings:

```sh
sudo systemctl stop planeradar
sudo install -m 0600 -o root -g root /var/lib/planeradar/settings.json /root/planeradar-settings.invalid.json
sudo rm /var/lib/planeradar/settings.json
sudo systemctl start planeradar
```

The display returns to mandatory QR setup. Do not paste the backup into a
public issue.

## Capture the logical frame

Use the controller:

```sh
mise run screenshot -- user@host --output planeradar-debug.png
```

It records prior metadata, asks systemd to send SIGUSR1, and waits for the
`planeradar` service to write a new service-owned regular frame. A privileged
helper validates that source and publishes a root-private snapshot. The
controller rejects stale or unsafe bytes and decodes exact 480×480 8-bit RGBA
before replacing the local destination.

The capture is the logical renderer output, not a framebuffer scrape. If it is
correct while the panel is wrong, look below the renderer at KMS, the overlay,
or the panel driver. If both are wrong, look at application rendering.

The image may contain live callsigns. Keep it private unless you have reviewed
and redacted it.

## Service restarts or permissions drift

```sh
ssh user@host
```

On that Pi shell:

```sh
systemctl show planeradar -p MainPID -p ActiveState -p SubState -p NRestarts
sudo systemd-analyze verify /etc/systemd/system/planeradar.service
sudo stat -c '%a %U:%G %n' \
  /opt/planeradar/bin/planeradar \
  /opt/planeradar/REVISION \
  /opt/planeradar/SHA256 \
  /var/lib/planeradar
sudo sha256sum /opt/planeradar/bin/planeradar
```

The binary is root-owned mode 0755; provenance and the unit are mode 0644;
application state is private; installer and lifecycle records are root-owned
mode 0600. Symlinks and special-file destinations are refused.

Normal logs omit coordinates, place searches, aircraft payloads, form bodies,
session values, and CSRF tokens. Still review any excerpt before sharing it.
Private state and screenshots do not become public merely because a command
called them diagnostics.

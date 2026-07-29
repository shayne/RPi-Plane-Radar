# Security policy

Plane Radar changes boot configuration, installs a kernel module, accepts a
LAN settings form, and uses SSH with sudo. A bug in any one of those paths is
more useful to an attacker than a misspelled radar label, so please report it
privately first.

## Report a vulnerability

Use the repository's
[private GitHub security advisory form](https://github.com/shayne/RPi-Plane-Radar/security/advisories/new).
Include the affected version or commit, the supported hardware and OS when
relevant, reproduction steps, impact, and the smallest useful diagnostic.

Do not open a public issue for an unpatched vulnerability. Do not attach:

- SSH keys or `known_hosts`;
- `.env` or target addresses;
- passwords, tokens, or Wi-Fi credentials;
- Pi serials or private network details;
- settings, coordinates, place searches, cookies, or CSRF values; or
- debug frames containing live callsigns.

If a screenshot is essential, crop or redact operational data and say how it
was produced.

## Supported versions

There is no stable release yet. Security fixes currently target the latest
published release candidate and `main`. Once `v0.1.0` is published, this file
will name the supported stable line explicitly.

Only the documented Raspberry Pi Zero 2 W, HyperPixel 2.1 Round, and 64-bit
Raspberry Pi OS Lite Trixie configuration is within the present hardware
support boundary.

The external HyperPixel kernel driver has its own issue and release boundary.
Report driver-specific problems to
[shayne/hyperpixel2r-kms](https://github.com/shayne/hyperpixel2r-kms) and use a
private advisory there when the report is security-sensitive.

## What the project protects

Every source install verifies release identity, checksums, full commits, and
manifests. Stable source installs additionally verify GitHub release integrity
and runnable artifact attestations. Explicit source release candidates keep
the manifest, checksum, and identity checks but skip that stable-only
attestation policy; the separate release bootstrap verifies
release-candidate attestations before executing the downloaded controller.
The controller uses strict OpenSSH host keys and binds durable state to the Pi
model and serial. Target state is root-owned and private. Archive extractors
reject traversal, links, unknown metadata, and trailing data.

The LAN server validates Host, Origin or Referer, session cookies, CSRF tokens,
content type, and body size. Normal health and logs omit location and request
content.

These controls reduce the trusted surface; they do not make the LAN or the Pi
hostile-tenant safe. Keep the device on a network you trust, protect the SSH
account, review upgrades, and keep recoverable SD-card backups before kernel or
OS changes.

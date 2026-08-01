# Recover Plane Radar

Most recovery starts with the same three commands:

```sh
mise run status -- user@host
mise run doctor -- user@host
mise run doctor -- user@host --json
```

Use the JSON report when filing a private diagnostic. It is structured and
bounded, but still review it before sharing. A screenshot can contain live
callsigns, and settings contain a real location.

## An install or upgrade was interrupted

Rerun the exact command with the same target, version or release directory,
hostname, and purge choice:

```sh
mise run install -- user@host --version X.Y.Z
```

The Mac record lives at
`${XDG_STATE_HOME}/planeradar/installer/<host-key-sha256>/state.json`, or under
`~/.local/state` when `XDG_STATE_HOME` is unset. The Pi record lives at
`/var/lib/planeradar-installer/state.json`; accepted lifecycle history lives at
`/var/lib/planeradar-installer/lifecycle.json`.

The controller compares both records with the current SSH host key, Pi model,
serial, and artifact identities. Completed phases are verified and skipped.
Do not delete either state file to make an error disappear. That replaces a
known interrupted transaction with an unknown one, which is a very efficient
way to turn recovery into archaeology.

## SSH reports a changed host key

Stop. A changed key can mean the SD card was reimaged, the hostname now points
to a different device, or a network path is being intercepted. Confirm the Pi
physically and compare the new model and serial with your installation record
before changing `~/.ssh/known_hosts`.

If the Pi was deliberately reimaged, treat it as a clean target and install
from a clean Mac state. Do not force an old transaction onto new hardware.

## The tryboot trial did not return

The display driver is activated with Raspberry Pi's one-shot tryboot. A failed
trial is not committed to the normal boot configuration.

1. Wait long enough to rule out a slow package or filesystem operation.
2. Power-cycle the Pi once.
3. Connect to the original hostname or address.
4. Run `status` and `doctor`.
5. For an initial installation, resume the exact install command.

An initial install has no prior accepted pair, so application rollback is not
available. If the failed tryboot happened during a driver-changing upgrade and
there is a prior accepted pair, rollback is valid:

```sh
mise run rollback -- user@host
```

The next power cycle should return to the prior normal boot configuration. If
SSH returns but the display remains blank, do not edit
`/boot/firmware/config.txt` by hand. The driver transaction owns exact source
hashes, candidate files, and any pre-existing `tryboot.txt`; manual edits break
that relationship.

The standalone driver project documents lower-level recovery:
[hyperpixel2r-kms operations](https://github.com/shayne/hyperpixel2r-kms/blob/main/docs/operations.md).

## The application upgrade is unhealthy

Run doctor first. The lifecycle manager automatically restores the prior
application when the new service fails health verification. If the controller
was interrupted during that repair, rerun rollback:

```sh
mise run rollback -- user@host
mise run doctor -- user@host
```

Plane Radar keeps three accepted pairs, so rollback selects recorded bytes
rather than rebuilding an approximation of the old version.

## The display is blank but SSH works

Start with the controller:

```sh
mise run doctor -- user@host
```

The distinct diagnostics tell you whether the running kernel, module,
vermagic, overlay, DRM device, 480×480 mode, renderer, touch device, or service
differs from the accepted pair. See [Troubleshooting](troubleshooting.md) for
read-only target probes.

If the kernel changed outside Plane Radar, the installed module may be correct
for the old kernel and useless for the new one. Resolve the exact driver
artifact or restore the prior kernel. Repeated blind reboots do not change
vermagic.

## The settings file is invalid

Invalid JSON, unknown fields, unsupported schema versions, non-finite or
out-of-range coordinates, and bad range indices are rejected. Preserve a
private backup before changing it. The least surprising recovery is to remove
only the invalid settings file and repeat QR setup; never paste that file into
a public issue.

## Uninstall recovery

An interrupted uninstall is also a transaction. Retry with the same command:

```sh
mise run uninstall -- user@host
```

If the original command included `--purge-settings`, include it again. The
controller refuses a different choice until the recorded uninstall finishes.
Uninstall performs a mandatory normal reboot after removing the accepted
display driver, then waits for an identity-bound reconnect before final
cleanup. If either the reconnect or cleanup was interrupted, retry the exact
same uninstall command. Temporary SSH loss during the normal reboot is
expected.

The application and driver ownership manifests limit what can be removed.
Networking and unrelated boot configuration remain outside that boundary.

## Make a private debug capture

```sh
mise run screenshot -- user@host --output planeradar-debug.png
```

The controller requires a new service-owned regular source and an exact
480×480 8-bit RGBA PNG before replacing the local destination. The
`planeradar` service writes `/var/lib/planeradar/debug.png`; the privileged
helper validates that ownership and publishes the root-private snapshot at
`/var/lib/planeradar-installer/captures/current.png`.

Delete local captures when the diagnosis is finished. They may contain live
callsigns even when the health report does not.

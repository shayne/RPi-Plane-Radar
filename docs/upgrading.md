# Upgrade, roll back, and remove Plane Radar

Version changes are explicit. That is slightly less magical than unattended
OS upgrades, and much more useful when the display driver is tied to an exact
kernel.

## Check the current installation

```sh
mise run status -- user@host
mise run doctor -- user@host
mise run doctor -- user@host --json
```

`status` gives the current accepted pair and first failure. `doctor` compares
application version, full revision, binary hash, driver version and revision,
driver manifest, kernel, module, overlay, service, restart count, HTTP, touch,
DRM mode, renderer, and mDNS.

## Upgrade to an immutable version

```sh
mise run upgrade -- user@host --version 0.1.0-rc.N
```

An application-only upgrade stages a content-addressed payload, atomically
activates it, restarts the service, and verifies health. If health fails, the
old binary is restored.

A driver-changing upgrade is a different transaction. It stages the exact
kernel artifact or cross-builds it, boots once through tryboot, verifies the
module, overlay, 480×480 mode, touch, KMSDRM, and OpenGL ES renderer, commits
the normal boot, then activates the application. That path reboots because the
kernel is involved, not because rebooting feels reassuring.

The driver stage is deliberately non-rebooting when Plane Radar calls it. The
controller first saves the staged phase locally and on the Pi, then owns the
one tryboot reboot and reconnect. This ordering matters: SSH disappearing
before the phase is durable turns a successful stage into an ambiguous retry.

For a cross-build, the controller normally refreshes the target kernel export
and rebuilds. If that export later fails because the package index has retired
the installed kernel's exact source version, a byte-valid cached build may be
resumed. The driver stage still revalidates its target manifest, source
revision, kernel, DTB, and artifact hashes before changing tryboot state. A
missing or modified cache does not enable this fallback.

Plane Radar retains the current and previous two accepted application/driver
pairs. Settings are outside those immutable payloads and survive an upgrade.

For an assembled local release:

```sh
mise run upgrade -- user@host --release-dir /absolute/path/to/release
```

The local directory receives the same manifest and identity checks as a
downloaded release.

## Roll back

```sh
mise run rollback -- user@host
```

Without a version, rollback selects the newest prior accepted pair. To choose a
specific retained release:

```sh
mise run rollback -- user@host --version 0.1.0-rc.N
```

Application-only rollback is atomic. A driver rollback uses the boot-safe
driver lifecycle and may reboot. The lifecycle transaction is durable, so a
second invocation resumes instead of replaying accepted phases.

## Remove Plane Radar

```sh
mise run uninstall -- user@host
```

This removes the service, installed application payloads, management helpers,
recorded display driver state, and only the boot and driver files proven by
the ownership manifests. It preserves `/var/lib/planeradar/settings.json` and
the rest of the application state by default. Once the identity-bound
uninstall completes, the Mac retires the superseded initial-install
transaction so the same Pi can be installed again from a release bootstrap.

To remove the saved location and preferences too:

```sh
mise run uninstall -- user@host --purge-settings
```

The purge choice is written into the uninstall transaction. If an uninstall is
interrupted, retry with the same choice. Switching flags halfway through is
rejected because the durable state and the command would disagree.

Both uninstall forms remove the accepted display driver through its lifecycle
protocol, perform a mandatory normal reboot, and wait for an identity-bound
reconnect before final cleanup. SSH will disappear during that reboot. If the
reconnect or final cleanup is interrupted, retry the exact same uninstall
command with the same `--purge-settings` choice.

Neither form changes Wi-Fi, SSH, unrelated packages, unrelated boot lines, or
files that cannot be proven as installer-owned.

## Maintain the driver lock

Most users should not run these commands. They change the source dependency
that future application releases consume:

```sh
mise run driver:sync
mise run driver:update -- 0.1.1
```

`driver:sync` verifies and materializes the exact release already named in
`driver.lock.toml`. `driver:update` verifies a new published version before
atomically changing the lock. Review and commit that lock change like any
other dependency update.

The driver is maintained in
[shayne/hyperpixel2r-kms](https://github.com/shayne/hyperpixel2r-kms). It is
not a submodule and its source is not vendored here.

## Kernel and OS upgrades

Do not assume a new Raspberry Pi kernel can load an old module. The module,
vermagic, overlay, applied DTB, and headers are bound to the running kernel.
Plane Radar can build a missing exact-kernel artifact through Docker, but it
does not claim arbitrary `apt full-upgrade` operations are unattended-safe.

Before changing the OS or kernel, make a recoverable SD-card backup and confirm
that the target kernel headers are available. After the change, run an upgrade
that resolves the exact driver and let tryboot verify it before committing.
If the Pi does not return, follow [Recovery](recovery.md).

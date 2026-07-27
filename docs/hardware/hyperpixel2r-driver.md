# HyperPixel 2.1 Round driver operations

This runbook covers the Plane Radar out-of-tree DRM panel driver, its
revisioned Device Tree overlay, and the FT5x06 touch child on the Raspberry Pi
Zero 2 W. The normal boot remains the recovery control plane until a one-shot
tryboot has passed both automated and physical acceptance.

## Electrical ownership and lifecycle

The custom platform driver owns GPIO10 (SDA), GPIO11 (SCL), GPIO18 (active-low
chip select), and GPIO19 (backlight). The overlay also assigns the DPI
`dpi_18bit_cpadhi_gpio0` pin group and gives the FT5x06 child its GPIO27
falling-edge interrupt. The panel and its bit-banged I2C adapter share GPIO
lines under the kernel driver's lifecycle; user space never opens `/dev/i2c-*`
or changes those GPIO directions.

Probe failure and panel-unprepare leave chip select inactive, the backlight
off, and the shared lines released. DRM prepare/unprepare serializes against
touch transfers. An active module is never unloaded: upgrades and rollback
take effect only across a reboot, after DRM and the input child have shut down
normally.

## Build the exact artifacts

Start with a clean GitButler `rpi-port` workspace and a reachable
`shayne@planeradar.local`. Exporting headers is read-only on the Pi and records
the exact `uname -r`, AArch64 architecture, `.config`, `Module.symvers`, common
headers, kbuild tools, Zero 2 W base DTB, and its checksum:

```sh
but status
mise run verify
mise run build-pi
mise run export-pi-kernel-build
mise run build-hyperpixel-driver
mise run check-hyperpixel-artifacts
```

The module and overlay are cross-built for ARM64 in the pinned Debian Trixie
OrbStack container. The driver build uses the exported target headers,
`Module.symvers`, and `W=1`; the overlay is compiled warning-free and applied
to the exported base DTB before publication.

Both the app and driver builds use the clean synthesized GitButler workspace
`HEAD`, not the underlying stack commit name. They record the exact revision
and tree as `planeradar.revision`, `planeradar.tree`, and the matching driver
manifest fields. This matters because a synthesized workspace commit can have
the same tree as `rpi-port` but a different commit ID; staging requires exact
app/driver revision and tree equality and never weakens identity to a
tree-only comparison. The app container receives a temporary `git archive
HEAD` build context rather than the mutable checkout, so untracked Cargo
configuration, build scripts, and other workspace files cannot affect an
artifact labeled with that clean revision.

`dist/hyperpixel/<kernel-release>/manifest.txt` records the clean source
revision and tree, target release and architecture, build image and command,
base-DTB checksum, exact revisioned overlay basename and checksum, applied-DTB
basename, and module basename, checksum, vermagic, and GPL license. Staging
rejects missing or duplicate manifest fields, dirty/mismatched source, unsafe
names or paths, a non-AArch64 target, a live release mismatch, checksum
mismatch, a differently revisioned app, symlinks, and an existing versioned
destination with different contents.

## Stage a one-shot boot

The optional overlay parameters are repeatable and limited to `rotate=0`,
`rotate=90`, `rotate=180`, `rotate=270`, `touchscreen-inverted-x`,
`touchscreen-inverted-y`, and `touchscreen-swapped-x-y`:

```sh
mise run stage-hyperpixel-tryboot
# Or, when calibration requires it:
./scripts/stage-hyperpixel-tryboot.sh \
  --parameter touchscreen-swapped-x-y \
  --parameter touchscreen-inverted-x
```

Staging atomically publishes the complete app/driver/source bundle under
`/usr/lib/planeradar/hyperpixel/<revision>/<kernel-release>/`, installs the
accepted cross-built module in `/lib/modules/<release>/extra/`, installs only
the revisioned custom overlay in `/boot/firmware/overlays/`, registers the
source with DKMS, and runs `depmod -a <release>`.

Published directories are root-owned mode `0755`; the staged app is `0755`;
and evidence, driver, overlay, and DKMS source files are root-owned mode
`0644`. Repeated staging verifies type, mode, and `root:root` ownership for
every object recursively, as well as byte content; nested ownership drift is
rejected rather than repaired in place. For reused package and DKMS trees, the
complete relative entry/type set, symlink/special-file absence, recursive
ownership, and exact modes are checked before any privileged content
comparison and rechecked afterward. The module and overlay installers reject
directory or symlink leaves and recheck the final regular file, checksum,
ownership, and mode.

The normal `/boot/firmware/config.txt` checksum is captured before apt,
package, module, overlay, DKMS, or depmod changes and is rechecked throughout
staging. The original checksum is also passed to the Rust `stage-display`
operation, which acquires the normal config's sibling lock, verifies that
exact source digest under the lock, validates the candidate, and durably
publishes tryboot before releasing the lock. Only
`/boot/firmware/tryboot.txt` is written; a cooperating concurrent normal
config edit aborts staging without overwriting that edit. Every boot-config
line is at most 98 bytes, excluding the CR byte of a CRLF terminator exactly
as the Rust validator does. The script syncs storage and prints, but does not
execute:

```sh
sudo reboot '0 tryboot'
```

Execute that command explicitly only at the hardware checkpoint. The tryboot
flag is one-shot. If the candidate hangs or loses the display or network,
remove and restore power once; firmware returns to the unchanged stock normal
configuration. Do not retry the candidate until stock boot and SSH recovery
are confirmed.

## Accept the candidate

After the tryboot returns over SSH, run:

```sh
mise run verify-hyperpixel-boot -- --expect-tryboot
```

The automated check requires the tryboot flag, exact release and AArch64
architecture, the custom driver and all dependencies, a bound platform node,
one connected 480×480 DRM connector, and an EDT/FT5 input device whose sysfs
ancestry descends from that bound platform device and whose axes have 479/480
maxima. It also requires matching deployed and `/healthz` revisions, captures
a journal cursor immediately before launching the transient service, and
accepts only the exact SDL readiness fields after that cursor:
`video_driver=kmsdrm` or SDL's equivalent `video_driver=KMSDRM`, together
with exact `render_driver=opengles2`. The transient unit uses
`StateDirectory=planeradar` with
`StateDirectoryMode=0750`, so systemd creates `/var/lib/planeradar` for the
`shayne` app user without making it world-writable and retains that persistent
state after the unit stops. The verifier removes any stale
`/var/lib/planeradar/debug.png` before launch; the SIGUSR1 debug capture must
then be newly created, pass full `pngcheck` decoding, and be 480×480. Service
shutdown is bounded; no failed unit, driver warning, blocked-task warning, or
kernel oops may appear in the current boot.

Automated checks are necessary but not sufficient. Before permanent commit,
visually confirm an edge-to-edge, sharp 480×480 image; test center and four
cardinal touches with `evtest`; prove SDL receives press, motion, and release;
verify a tap causes exactly one action; verify a continuous three-second hold
causes the alternate action without a release tap; and confirm the touch
orientation matches the display.

## Commit or roll back

Only after the exact staged revision and parameters pass automated and
physical acceptance:

```sh
mise run commit-hyperpixel-boot
# Review the printed command, then explicitly run:
ssh shayne@planeradar.local sudo reboot
./scripts/verify-hyperpixel-boot.sh --expect-normal
```

Commit preserves the first normal-config backup as
`config.txt.planeradar-backup`, atomically selects exactly one accepted custom
overlay in normal `config.txt`, syncs storage, and prints the normal reboot
command without executing it.

Explicit stock rollback is:

```sh
mise run rollback-hyperpixel-boot
# Review the printed command, then explicitly run:
ssh shayne@planeradar.local sudo reboot
```

Rollback atomically selects exactly one stock
`dtoverlay=vc4-kms-dpi-hyperpixel2r` declaration, removes every active custom
declaration and its owned parameters, syncs storage, and retains all
revisioned artifacts for diagnosis. It does not unload the running module;
the normal reboot performs the lifecycle transition.

## Accepted Raspberry Pi Zero 2 W installation

The physically accepted installation on `planeradar.local` is:

- source revision
  `eefaf3ae40fd1b2728bea80fa2a7286f7426d34e`, source tree
  `b01b6974b9681b5c4812be10f3606c025eb0ff8b`;
- Raspberry Pi OS kernel `6.18.34+rpt-rpi-v8`;
- overlay `planeradar-hyperpixel2r-eefaf3ae40fd.dtbo`, SHA-256
  `82cd144d86fc88a31198b7d61f538ebaa2f990f2103a738c1fe34b52a52a5a92`;
- module `planeradar_hyperpixel2r.ko`, SHA-256
  `900de8a80d31d091682f5b00019f277a49869558dfb34d33d19f8bd685afb05b`,
  with vermagic
  `6.18.34+rpt-rpi-v8 SMP preempt mod_unload modversions aarch64`;
- native `rotate=0` orientation with no optional touch inversion or axis-swap
  parameters; and
- normal-config SHA-256
  `04491a62e16d6baf80654d552e1659aa423498137c53460d0f7353f86960ec0b`.

The recovery baseline remains
`/boot/firmware/config.txt.task6-baseline.20260727T003128Z.bak`, SHA-256
`d237a211ad67b941f2c36e08917984143d256793f1aaf348cf7ee4249df7dbeb`.
Do not replace or edit that file.

Automated verification passed after both a normal reboot and a cold power
cycle with `tryboot=0`, a connected `card0-DPI-1` at 480×480,
`video_driver=KMSDRM`, `render_driver=opengles2`, VC4/V3D rendering, and zero
failed units. The accepted touch device is `/dev/input/event0`,
`11-0015 generic ft5x06 (00)`, with `ABS_X`, `ABS_Y`,
`ABS_MT_POSITION_X`, and `ABS_MT_POSITION_Y` spanning 0 through 479.
Physical samples landed at center `(256,264)`, top `(255,58)`, right
`(430,265)`, bottom `(250,433)`, and left `(95,230)`. A continuous center
hold lasted 3.60 seconds and showed the QR screen without a release tap; the
following 0.12-second tap returned to radar. The accepted radar is
edge-to-edge, sharp, true black, and uses the one-pixel outlined range label.

The live app stopped in 0.907 seconds during the final measured shutdown,
released port 80, and left zero failed units. A rollback-generation proof
against a temporary copy produced exactly one stock declaration, zero custom
declarations, and no boot-config line longer than 98 bytes; the live accepted
normal configuration remained byte-identical and the temporary copy was
deleted.

## Kernel upgrades and DKMS

The source is installed as `/usr/src/planeradar-hyperpixel2r-0.1.0` and
registered once with DKMS. Membership parsing accepts the exact source-only
`planeradar-hyperpixel2r/0.1.0: added` status and exact built comma-form
records with a safe kernel-release field, exact `aarch64`, and only `built` or
`installed` status. Removed, broken, garbage, trailing-data, unsafe-kernel,
wrong-architecture, and name/version substring records do not satisfy
membership. For the currently accepted release, the exact cross-built and
verified `.ko` remains authoritative: staging never runs `dkms build` or
`dkms install` for it. After a Raspberry Pi OS kernel upgrade, DKMS may build
and install the small GPL module when matching headers are
available. Before booting that new kernel, repeat the release, overlay,
tryboot, automated, and physical acceptance flow; never assume the prior
kernel's module or acceptance evidence applies.

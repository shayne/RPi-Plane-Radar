# HyperPixel 2.1 Round driver operations

Plane Radar consumes the HyperPixel driver from the independently released
[`shayne/hyperpixel2r-kms`](https://github.com/shayne/hyperpixel2r-kms)
repository. The app no longer owns kernel sources or shell lifecycle scripts.
`driver.lock.toml` is the authoritative release identity: repository, version,
commit, and release-manifest digest must all match.

## Resolve the locked source

Download and verify the exact locked release:

```sh
mise run driver:sync
```

The resolver verifies the release checksums and GitHub attestations before
extracting the source into a content-addressed directory under `.cache/`.
Extraction rejects unsafe archive paths, and publication of a verified cache
entry is atomic. Repeating `driver:sync` reuses only a complete cache entry
whose release identity still matches the lock.

To intentionally move the lock to another published version:

```sh
mise run driver:update -- 0.1.0-rc.16
mise run driver:sync
```

Review and commit the resulting `driver.lock.toml` change. Never edit its
digests by hand or substitute a mutable checkout for the resolved source.

## Driver lifecycle

Application workflows call the typed driver adapter. Its only permitted
actions are `ExportKernel`, `Build`, `StageTryboot`, `VerifyBoot`,
`CommitBoot`, `RollbackBoot`, and `Uninstall`; each maps to the corresponding
tool shipped in the verified driver release. The adapter supplies the Pi
target, exact target kernel release, build/output directories, locked driver
source revision, expected driver version and overlay, and strict verification
mode as separate arguments.

Do not invoke app-local HyperPixel scripts: they were removed when ownership
moved to the external release. Use the external driver's runbook for the
one-shot tryboot, physical acceptance, commit, rollback, and kernel-upgrade
procedures.

## Accepted Raspberry Pi Zero 2 W installation

The next accepted external lifecycle source is the pending release
`v0.1.0-rc.16`, commit
`58c42896c8829a034f42a4bc92886dd6f21775a8`, locked by manifest SHA-256
`5494a9f7f5c4a3214fff669efa26574b7d2602bafda5992472cd94b90885573c`.
The running driver remains the already accepted RC13-built image. RC14 through
the pending RC16 contain migration, release-control, and crash-recovery
improvements and were deliberately verified here without staging or rebooting
the Pi. It targets kernel `6.18.34+rpt-rpi-v8` with:

- module `hyperpixel2r_kms`, vermagic
  `6.18.34+rpt-rpi-v8 SMP preempt mod_unload modversions aarch64`;
- dependencies `drm,i2c-algo-bit` and soft dependency `pre: edt_ft5x06`;
- overlay `hyperpixel2r-kms-d965cd55b5a6.dtbo`;
- compatible string `shayne,hyperpixel2r-kms`;
- connected `card0-DPI-1` at 480×480 and touch device
  `11-0015 generic ft5x06 (00)`; and
- SDL `KMSDRM` with renderer `opengles2`.

Strict verification passed in both the original one-shot tryboot and committed
normal boot. The RC14 verifier then reaccepted that unchanged normal boot, and
the RC14 cleanup proved the legacy state already absent. The normal boot ID
remains
`a1d7d580-c332-4f37-ba00-1814b2322394`, with `tryboot=0`, no active
transaction, no failed units, and no throttling.

The recovery baseline remains
`/boot/firmware/config.txt.task6-baseline.20260727T003128Z.bak`, SHA-256
`d237a211ad67b941f2c36e08917984143d256793f1aaf348cf7ee4249df7dbeb`.
Do not replace or edit that file.

The exact guarded migration removed the inactive
`planeradar-hyperpixel2r/0.1.0` DKMS source and the eight contract-listed
legacy overlays, then a second run proved the already-absent path. Its
root-owned audit is retained under
`/var/lib/hyperpixel2r-kms/migrations/planeradar-hyperpixel2r-v1/`;
RC14 independently repeated the already-absent proof without changing the
accepted installation. Only the external `hyperpixel2r-kms` overlay is active.

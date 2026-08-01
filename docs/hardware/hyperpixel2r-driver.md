# HyperPixel 2.1 Round driver

Plane Radar consumes the display driver from the separate
[shayne/hyperpixel2r-kms](https://github.com/shayne/hyperpixel2r-kms)
repository. The application repository does not vendor kernel source, retain a
mutable checkout, or use a submodule.

`driver.lock.toml` pins the exact driver repository, semantic version, full
commit, release-manifest SHA-256, and lifecycle protocol. The controller
verifies that identity before selecting an exact-kernel prebuilt archive or
building against the target's kernel context.

## Maintainer commands

Resolve the current lock:

```sh
mise run driver:sync
```

Verify and update it to a published version:

```sh
mise run driver:update -- 0.1.1
mise run driver:sync
```

Review the resulting `driver.lock.toml`. Do not edit a digest by hand or
substitute a mutable source directory.

## Why the lifecycle is separate

Application bytes can be switched and rolled back while Linux is running. A
panel module and Device Tree overlay decide whether the next boot has a
display, touch, or sometimes a useful SSH session. Treating those changes as
the same operation would make the simple path look simple by hiding all of its
risk in reboot.

The driver release owns exact-kernel packaging, one-shot tryboot staging,
module/overlay verification, normal-boot commit, rollback, and uninstall. Read
the driver's
[operations guide](https://github.com/shayne/hyperpixel2r-kms/blob/main/docs/operations.md)
and
[compatibility matrix](https://github.com/shayne/hyperpixel2r-kms/blob/main/docs/compatibility.md)
for the current contract.

Plane Radar calls that lifecycle through typed actions. Users should use
`mise run install`, `upgrade`, `rollback`, and `uninstall` rather than invoking
driver scripts directly.

# HyperPixel KMS Touch Driver Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add a fail-safe, out-of-tree Linux driver that keeps VC4/KMS/V3D
graphics accelerated while exposing HyperPixel 2.1 Round touch through the
normal Linux input subsystem on the Plane Radar Raspberry Pi Zero 2 W.

**Architecture:** One GPL-2.0-only platform module owns the panel's shared
GPIO10/GPIO11 control bus, temporarily bit-bangs the ST7701's 9-bit SPI
protocol during panel transitions, and exposes those pins as an
`i2c-algo-bit` adapter during steady-state operation. The existing
`edt-ft5x06` kernel driver owns the FT5x06 child and emits standard input
events; the MIT Rust application consumes them through SDL and performs no raw
I2C. Mise drives host tests, exact-header ARM64 builds in OrbStack, artifact
validation, one-shot `tryboot.txt` deployment, physical acceptance, and
permanent installation.

**Tech Stack:** Rust 1.97.1, Rust 2024, SDL2 0.38, Linux 6.18 DRM panel API,
GPIO descriptors, `i2c-algo-bit`, `edt-ft5x06`, Device Tree overlays, GCC 14
for `aarch64-linux-gnu`, Debian 13 ARM64 containers in OrbStack, DKMS, kmod,
Raspberry Pi `tryboot`, systemd, mise, and GitButler.

## Global Constraints

- The approved design in
  `docs/superpowers/specs/2026-07-26-hyperpixel-kms-touch-driver-design.md`
  is authoritative.
- The initial hardware target is `shayne@planeradar.local`, Raspberry Pi Zero
  2 W, AArch64 kernel `6.18.34+rpt-rpi-v8`; every build must rediscover and
  compare the live release instead of assuming it remained unchanged.
- Preserve the currently working normal boot declaration
  `dtoverlay=vc4-kms-dpi-hyperpixel2r` until candidate display, touch, SSH, and
  application checks pass under a one-shot tryboot.
- The custom overlay name starts with `planeradar-hyperpixel2r-`, never
  overwrites Raspberry Pi's stock overlay, and stays within the documented
  98-byte `config.txt` line limit.
- The module owns GPIO10, GPIO11, GPIO18, and GPIO19. The standard FT5x06 child
  is the sole functional owner of GPIO27's falling-edge interrupt.
- GPIO10 and GPIO11 have one descriptor owner. No simultaneous `spi-gpio` and
  `i2c-gpio` platform devices are permitted.
- The panel mode is 480×480, 19.2 MHz pixel clock, RGB666
  `MEDIA_BUS_FMT_RGB666_1X24_CPADHI`, negative H/V sync, and
  `DRM_BUS_FLAG_PIXDATA_DRIVE_NEGEDGE`, matching the working Raspberry Pi
  ST7701 HyperPixel descriptor.
- The panel command source is Raspberry Pi Linux commit
  `33bb14b06b3fb5a682d4a7a3db3963fe558fc6f9`; retain its relevant copyright
  notices while licensing the kernel package as GPL-2.0-only.
- VC4 owns scanout, V3D owns rendering, and SDL must report `kmsdrm` plus
  `opengles2`. The custom module never writes pixels.
- Remove `i2cdev`, `/dev/i2c-11`, `HyperpixelTouch`, and synchronous touch
  polling from the Rust process. They must not return as a fallback.
- SDL is the first kernel-input consumer. A nonblocking evdev worker is added
  only when the hardware checkpoint proves `evtest` receives events while SDL
  does not.
- Kernel artifacts are cross-built on this Mac in OrbStack against the exact
  header tree and `Module.symvers` exported from the live Pi. The Pi does not
  compile the normal development artifact.
- DKMS metadata is installed as the kernel-upgrade safety net. It is registered
  but does not replace the accepted cross-built module for the current kernel.
- Every generated module bundle records source revision, kernel release, build
  command, `vermagic`, base-DTB checksum, module checksum, and overlay
  checksum.
- The build refuses a missing `Module.symvers`, a module/kernel release
  mismatch, a non-AArch64 module, a non-GPL module, or an overlay that cannot
  apply to `bcm2710-rpi-zero-2-w.dtb`.
- Driver upgrades and rollback occur through reboot. Deployment scripts never
  unload an active display module.
- A failed candidate boot is recovered by one power cycle into the untouched
  normal configuration.
- The permanent boot configuration is changed only after automated tryboot
  checks and the user's physical tap, long-press, orientation, and visual
  approval.
- `Task 16` in the main Plane Radar plan remains incomplete until tap and
  three-second hold pass on the physical panel.
- All developer entry points use `mise run`; all content commits use
  GitButler on `rpi-port`.
- Run `but status` and `but diff` before every commit. Do not push during this
  implementation unless the user explicitly asks.

## Planned File Map

| Path | Responsibility |
| --- | --- |
| `Cargo.toml`, `Cargo.lock` | Remove the raw-I2C dependency and retain SDL as the application input boundary |
| `src/display.rs`, `src/lib.rs` | Consume SDL finger events without any touch-device polling |
| `tests/display.rs`, `tests/no_raw_i2c.rs` | Prove bounded display shutdown and prohibit raw I2C from the app |
| `kernel/planeradar_hyperpixel2r_protocol.{h,c}` | GPL command table and host-testable 9-bit word encoder |
| `kernel/tests/protocol_test.c` | Exact protocol, delay, bit-order, and fixed-mode tests |
| `kernel/planeradar_hyperpixel2r_main.c` | DRM panel, shared-bus lifecycle, GPIO ownership, and bit-banged I2C adapter |
| `kernel/planeradar-hyperpixel2r-overlay.dts` | Pi Zero 2 W DPI graph, driver node, GPIOs, and FT5x06 child |
| `kernel/{Kbuild,Makefile,dkms.conf,LICENSE,README.md}` | Out-of-tree build, DKMS metadata, licensing, and source provenance |
| `packaging/Dockerfile.kernel` | Pinned Debian 13 ARM64 kernel build environment |
| `scripts/export-pi-kernel-build.sh` | Export exact live headers, `Module.symvers`, kbuild scripts, and base DTB |
| `scripts/build-hyperpixel-driver.sh` | Cross-build and validate `.ko`, `.dtbo`, and manifest |
| `scripts/check-hyperpixel-artifacts.sh` | Fail closed on artifact metadata, checksums, and target mismatch |
| `scripts/stage-hyperpixel-tryboot.sh` | Install versioned candidate artifacts and atomically write `tryboot.txt` |
| `scripts/verify-hyperpixel-boot.sh` | Verify SSH, driver binding, KMS, input, SDL, app health, and shutdown |
| `scripts/commit-hyperpixel-boot.sh` | Apply an accepted overlay to normal `config.txt` |
| `scripts/rollback-hyperpixel-boot.sh` | Restore the stock overlay without deleting recoverable artifacts |
| `src/install.rs`, `src/cli.rs`, `src/main.rs` | Validated boot-config selection and stage/commit/rollback commands |
| `tests/boot_config.rs`, `tests/driver_artifacts.rs` | Boot line, atomic-file, manifest, and script contracts |
| `docs/hardware/hyperpixel2r-driver.md` | Operator build, tryboot, verification, DKMS, and recovery runbook |
| `.superpowers/sdd/2026-07-25-rpi-plane-radar/progress.md` | Exact hardware acceptance evidence |
| `mise.toml`, `.github/workflows/ci.yml`, `.dockerignore`, `scripts/build-pi.sh` | Developer tasks, host protocol CI, container context, and non-destructive dist output |

---

### Task 1: Remove Raw I2C from the Rust Display Loop

**Files:**

- Modify: `Cargo.toml`
- Modify: `Cargo.lock`
- Modify: `src/display.rs`
- Modify: `src/lib.rs`
- Modify: `tests/display.rs`
- Create: `tests/no_raw_i2c.rs`
- Delete: `src/hyperpixel.rs`
- Delete: `tests/hyperpixel_touch.rs`

**Interfaces:**

- Consumes: SDL `Event::FingerDown`, `Event::FingerMotion`, and
  `Event::FingerUp`.
- Produces:
  `DisplayConfig { width, height, video_driver, render_driver, fullscreen }`.
- Produces: `run_display<H: DisplayHandler>(DisplayConfig, &mut H)`, whose
  per-frame input collection is only nonblocking `event_pump.poll_iter()`.
- Produces: a source guard that rejects `i2cdev`, `/dev/i2c-`, and
  `HyperpixelTouch` in application code.
- Produces one successful initialization log containing the selected SDL video
  and render driver names.

- [ ] **Step 1: Write the failing raw-I2C boundary test**

```rust
// tests/no_raw_i2c.rs
use std::path::Path;

#[test]
fn application_has_no_raw_hyperpixel_i2c_path() {
    let manifest = include_str!("../Cargo.toml");
    let display = include_str!("../src/display.rs");
    let library = include_str!("../src/lib.rs");

    assert!(!manifest.contains("i2cdev"));
    assert!(!display.contains("/dev/i2c-"));
    assert!(!display.contains("HyperpixelTouch"));
    assert!(!library.contains("mod hyperpixel"));
    assert!(!Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/src/hyperpixel.rs")).exists());
}
```

- [ ] **Step 2: Run the source guard and verify the expected failure**

Run:

```bash
mise exec -- cargo test --test no_raw_i2c
```

Expected: FAIL because `Cargo.toml`, `src/display.rs`, `src/lib.rs`, and
`src/hyperpixel.rs` still contain the rejected direct-I2C path.

- [ ] **Step 3: Remove the direct touch implementation**

Delete the Linux `i2cdev` dependency, `src/hyperpixel.rs`,
`tests/hyperpixel_touch.rs`, the `pub mod hyperpixel` export,
`DisplayConfig::touch_device`, `default_touch_device`, the
`HyperpixelTouch::open` block, and the synchronous `touch.poll()` block.
Retain the existing SDL touch-device count warning and change it to:

```rust
let sdl_touch_available = sdl2::touch::num_touch_devices() > 0;
if !sdl_touch_available {
    log::warn!("SDL touch input is unavailable; display and web setup remain active");
}
```

The render loop remains:

```rust
let events: Vec<_> = event_pump
    .poll_iter()
    .filter_map(|event| normalize_sdl_event(&event))
    .collect();
let update = handler.step(&events, frame_start);
```

After both driver-name checks pass, log:

```rust
log::info!(
    "SDL display ready: video_driver={actual_driver} render_driver={actual_renderer}"
);
```

Run `mise exec -- cargo check --all-targets` once to remove `i2cdev` and its
unused transitive packages from `Cargo.lock`.

- [ ] **Step 4: Write the bounded shutdown regression test**

Add this test to `tests/display.rs`:

```rust
use std::time::{Duration, Instant};
use planeradar::display::{
    DisplayHandler, DisplayUpdate, run_display,
};

struct ExitImmediately {
    shutdown_calls: usize,
}

impl DisplayHandler for ExitImmediately {
    fn step(&mut self, _events: &[InputEvent], _now: Instant) -> DisplayUpdate {
        DisplayUpdate {
            frame: None,
            exit: true,
        }
    }

    fn shutdown(&mut self) {
        self.shutdown_calls += 1;
    }
}

#[test]
fn display_exit_is_bounded_without_a_touch_read() {
    let started = Instant::now();
    let mut handler = ExitImmediately { shutdown_calls: 0 };
    let config = DisplayConfig {
        width: 16,
        height: 16,
        video_driver: "dummy".to_owned(),
        render_driver: "software".to_owned(),
        fullscreen: false,
    };

    run_display(config, &mut handler).expect("dummy display");
    assert_eq!(handler.shutdown_calls, 1);
    assert!(started.elapsed() < Duration::from_secs(5));
}
```

- [ ] **Step 5: Run focused and full Rust checks**

```bash
mise exec -- cargo test --test no_raw_i2c --test display
mise run verify
```

Expected: the focused tests pass, `Cargo.lock` contains no `i2cdev` package,
and the full suite passes.

- [ ] **Step 6: Commit the application boundary**

```bash
but status
but diff
but commit rpi-port -m "fix: move HyperPixel touch into kernel input"
```

---

### Task 2: Define and Test the ST7701 Protocol

**Files:**

- Create: `kernel/planeradar_hyperpixel2r_protocol.h`
- Create: `kernel/planeradar_hyperpixel2r_protocol.c`
- Create: `kernel/tests/protocol_test.c`
- Create: `kernel/LICENSE`
- Create: `kernel/README.md`
- Create: `scripts/test-hyperpixel-protocol.sh`
- Modify: `mise.toml`
- Modify: `.github/workflows/ci.yml`

**Interfaces:**

- Consumes: the HyperPixel descriptor, `st7701_init_sequence`, and
  `txw210001b0_gip_sequence` from Raspberry Pi Linux commit
  `33bb14b06b3fb5a682d4a7a3db3963fe558fc6f9`.
- Produces:
  `struct hp2r_command { hp2r_u8 command; hp2r_u8 data_len; hp2r_u16 delay_ms; hp2r_u8 data[16]; }`.
- Produces:
  `int hp2r_emit_command(const struct hp2r_command *, hp2r_word_sink, void *)`.
- Produces:
  `hp2r_prepare_commands`, `hp2r_prepare_command_count`,
  `hp2r_display_off_command`, and `hp2r_sleep_command`.
- Produces exact fixed-mode constants used by the kernel driver.

- [ ] **Step 1: Add the protocol types and failing host tests**

The public header uses kernel types in the module and fixed-width C types in
the host test:

```c
// SPDX-License-Identifier: GPL-2.0-only
#ifndef PLANERADAR_HYPERPIXEL2R_PROTOCOL_H
#define PLANERADAR_HYPERPIXEL2R_PROTOCOL_H

#ifdef __KERNEL__
#include <linux/types.h>
typedef u8 hp2r_u8;
typedef u16 hp2r_u16;
#else
#include <stdint.h>
#include <stddef.h>
typedef uint8_t hp2r_u8;
typedef uint16_t hp2r_u16;
#endif

#define HP2R_WIDTH 480
#define HP2R_HEIGHT 480
#define HP2R_CLOCK_KHZ 19200
#define HP2R_HSYNC_START 490
#define HP2R_HSYNC_END 506
#define HP2R_HTOTAL 562
#define HP2R_VSYNC_START 495
#define HP2R_VSYNC_END 555
#define HP2R_VTOTAL 570
#define HP2R_MAX_DATA 16

struct hp2r_command {
    hp2r_u8 command;
    hp2r_u8 data_len;
    hp2r_u16 delay_ms;
    hp2r_u8 data[HP2R_MAX_DATA];
};

typedef int (*hp2r_word_sink)(void *context, hp2r_u16 word);

int hp2r_emit_command(
    const struct hp2r_command *command,
    hp2r_word_sink sink,
    void *context
);

extern const struct hp2r_command hp2r_prepare_commands[];
extern const size_t hp2r_prepare_command_count;
extern const struct hp2r_command hp2r_display_off_command;
extern const struct hp2r_command hp2r_sleep_command;

#endif
```

`kernel/tests/protocol_test.c` must include named tests for:

- `0x11` becoming command word `0x011`;
- data byte `0x77` becoming data word `0x177`;
- all nine bits being emitted most-significant-bit first;
- the first prepare command being soft reset with a 5 ms delay;
- exit-sleep having a 120 ms delay;
- command-bank disable occurring before display-on;
- display-off preceding enter-sleep;
- every command having at most 16 data bytes; and
- the exact 480×480 timing constants above.

The test program returns nonzero on the first failed assertion and prints
`protocol tests passed` only after every assertion succeeds.

- [ ] **Step 2: Add the test task and verify it fails**

```bash
#!/usr/bin/env bash
# scripts/test-hyperpixel-protocol.sh
set -euo pipefail

build_dir="$(mktemp -d "${TMPDIR:-/tmp}/planeradar-protocol.XXXXXX")"
trap 'rm -rf "$build_dir"' EXIT
"${CC:-cc}" -std=c11 -Wall -Wextra -Werror -pedantic \
  -Ikernel \
  kernel/planeradar_hyperpixel2r_protocol.c \
  kernel/tests/protocol_test.c \
  -o "$build_dir/protocol-test"
"$build_dir/protocol-test"
```

Add:

```toml
[tasks.test-driver-protocol]
run = "./scripts/test-hyperpixel-protocol.sh"
```

Run:

```bash
mise run test-driver-protocol
```

Expected: compilation or assertions fail because the command implementation
and complete sequence do not exist.

- [ ] **Step 3: Implement the 9-bit encoder**

`hp2r_emit_command` emits a command with bit 8 clear, then each data byte with
bit 8 set, stopping immediately when the sink returns an error:

```c
int hp2r_emit_command(
    const struct hp2r_command *command,
    hp2r_word_sink sink,
    void *context
) {
    size_t index;
    int result;

    if (!command || !sink || command->data_len > HP2R_MAX_DATA)
        return -1;

    result = sink(context, command->command);
    if (result)
        return result;

    for (index = 0; index < command->data_len; index++) {
        result = sink(context, 0x100u | command->data[index]);
        if (result)
            return result;
    }

    return 0;
}
```

- [ ] **Step 4: Add the exact panel command sequence and provenance**

Translate the exact values from these pinned upstream units into
`hp2r_prepare_commands`:

- `st7701_init_sequence`;
- `txw210001b0_gip_sequence`;
- `hyperpixel2r_desc.pv_gamma`;
- `hyperpixel2r_desc.nv_gamma`; and
- the common display-on command.

Use explicit `struct hp2r_command` records, not computed voltage formulas at
runtime. Preserve the Raspberry Pi driver authors' copyright notices in
`kernel/README.md` and cite the exact commit and source path. Place the
canonical GNU GPL version 2 text in `kernel/LICENSE`. Every kernel C and header
file begins with `// SPDX-License-Identifier: GPL-2.0-only`.

- [ ] **Step 5: Run protocol and Rust verification**

```bash
mise run test-driver-protocol
mise run verify
```

Expected: `protocol tests passed`, followed by a clean Rust verification.

- [ ] **Step 6: Add protocol CI and commit**

Add `mise run test-driver-protocol` before `mise run verify` in
`.github/workflows/ci.yml`, then:

```bash
but status
but diff
but commit rpi-port -m "feat: define HyperPixel panel protocol"
```

---

### Task 3: Build the Combined DRM and I2C Kernel Module

**Files:**

- Create: `kernel/planeradar_hyperpixel2r_main.c`
- Create: `kernel/Kbuild`
- Create: `kernel/Makefile`
- Create: `packaging/Dockerfile.kernel`
- Create: `scripts/export-pi-kernel-build.sh`
- Create: `scripts/build-hyperpixel-driver.sh`
- Create: `scripts/check-hyperpixel-artifacts.sh`
- Create: `tests/driver_artifacts.rs`
- Modify: `mise.toml`
- Modify: `.dockerignore`
- Modify: `scripts/build-pi.sh`

**Interfaces:**

- Consumes: `hp2r_emit_command`, `hp2r_prepare_commands`, the live kernel
  release, exact target headers, exact `Module.symvers`, kbuild support files,
  and the live Pi Zero 2 W base DTB.
- Produces: platform modalias `of:N*T*Cplaneradar,hyperpixel2r`.
- Produces: one fixed DRM panel with prepare, enable, disable, unprepare,
  get-modes, and orientation callbacks.
- Produces: one `i2c_adapter` named `planeradar-hyperpixel2r` with
  `i2c_algo_bit_data { udelay = 4, timeout = HZ / 10 }`.
- Produces:
  `dist/hyperpixel/<kernel-release>/planeradar_hyperpixel2r.ko`.
- Produces a tab-separated `manifest.txt` checked by
  `scripts/check-hyperpixel-artifacts.sh`.

- [ ] **Step 1: Write the failing artifact contract**

`tests/driver_artifacts.rs` accepts
`PLANERADAR_DRIVER_ARTIFACT_DIR`; when unset it skips with a single explanatory
line. When set it requires:

```text
planeradar_hyperpixel2r.ko
manifest.txt
module.sha256
module.modinfo.txt
```

It parses `manifest.txt` as unique tab-separated key/value rows and asserts the
presence of:

```text
source_revision
source_dirty
kernel_release
kernel_arch
build_image
build_command
base_dtb_sha256
module_file
module_sha256
module_vermagic
module_license
```

It asserts `kernel_arch` is `aarch64`, `module_license` is `GPL`, the
`module_vermagic` begins with the recorded `kernel_release`, and the recorded
module checksum equals a fresh SHA-256 of the `.ko`. `source_dirty` must be
exactly `true` or `false`; hardware staging accepts only `false`.

Run:

```bash
PLANERADAR_DRIVER_ARTIFACT_DIR=dist/hyperpixel/missing \
  mise exec -- cargo test --test driver_artifacts
```

Expected: FAIL because no driver artifact bundle exists.

- [ ] **Step 2: Implement exact target export**

`scripts/export-pi-kernel-build.sh` uses
`${PLANERADAR_PI_TARGET:-shayne@planeradar.local}` and:

1. reads `uname -r` and requires `uname -m` to equal `aarch64`;
2. resolves `/lib/modules/<release>/build`;
3. requires `.config` and `Module.symvers`;
4. reads the absolute common-header directory from the target header
   `Makefile` include;
5. resolves the kbuild root from the target header's `scripts` symlink;
6. requires `/boot/firmware/bcm2710-rpi-zero-2-w.dtb`;
7. streams those three absolute directory trees and the base DTB through
   `tar` over SSH; and
8. extracts them under
   `dist/kernel-target/<release>/root` so their relative symlinks remain valid.

Write `dist/kernel-target/<release>/target.txt` with:

```text
kernel_release	<live uname -r>
kernel_arch	aarch64
header_path	/usr/src/linux-headers-<live uname -r>
common_header_path	<absolute included common-header directory>
kbuild_path	<absolute kbuild root>
base_dtb_path	/boot/firmware/bcm2710-rpi-zero-2-w.dtb
base_dtb_sha256	<sha256>
```

The script refuses an existing export whose release or base-DTB checksum does
not match the live target.

- [ ] **Step 3: Add the pinned kernel build container**

`packaging/Dockerfile.kernel` is:

```dockerfile
FROM debian:trixie-slim

RUN apt-get update \
    && apt-get install -y --no-install-recommends \
        bc \
        binutils-aarch64-linux-gnu \
        bison \
        ca-certificates \
        device-tree-compiler \
        file \
        flex \
        gcc-14-aarch64-linux-gnu \
        kmod \
        libelf-dev \
        libssl-dev \
        make \
        openssl \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /build
```

The build image tag is
`planeradar-kernel-builder:debian-trixie-gcc14`. Build and run it as
`--platform linux/arm64`.

- [ ] **Step 4: Implement the module state and GPIO ownership**

The private state is:

```c
struct planeradar_hyperpixel2r {
    struct device *dev;
    struct drm_panel panel;
    struct gpio_desc *sda;
    struct gpio_desc *scl;
    struct gpio_desc *cs;
    struct gpio_desc *backlight;
    struct i2c_adapter adapter;
    struct i2c_algo_bit_data bit;
    struct mutex state_lock;
    enum drm_panel_orientation orientation;
    bool prepared;
    bool enabled;
};
```

Probe `planeradar,hyperpixel2r` as a platform device. Acquire:

```c
devm_gpiod_get(dev, "sda", GPIOD_ASIS);
devm_gpiod_get(dev, "scl", GPIOD_ASIS);
devm_gpiod_get(dev, "cs", GPIOD_OUT_INACTIVE);
devm_gpiod_get(dev, "backlight", GPIOD_OUT_INACTIVE);
```

Reject sleeping SDA/SCL/CS GPIOs, release SDA and SCL as inputs, initialize the
state mutex, and leave the backlight inactive on every probe failure.
GPIO27 is not requested by this parent; the FT5x06 child receives it through
its Device Tree interrupt.

- [ ] **Step 5: Implement shared-line I2C**

The I2C callbacks model open drain without permanently marking the descriptors
open drain:

```c
static void hp2r_setsda(void *context, int high)
{
    struct planeradar_hyperpixel2r *hp = context;

    if (high)
        gpiod_direction_input(hp->sda);
    else
        gpiod_direction_output(hp->sda, 0);
}

static void hp2r_setscl(void *context, int high)
{
    struct planeradar_hyperpixel2r *hp = context;

    if (high)
        gpiod_direction_input(hp->scl);
    else
        gpiod_direction_output(hp->scl, 0);
}
```

Use `gpiod_get_value` for both getters. Set:

```c
hp->bit.data = hp;
hp->bit.setsda = hp2r_setsda;
hp->bit.setscl = hp2r_setscl;
hp->bit.getsda = hp2r_getsda;
hp->bit.getscl = hp2r_getscl;
hp->bit.udelay = 4;
hp->bit.timeout = HZ / 10;
hp->bit.can_do_atomic = true;

hp->adapter.owner = THIS_MODULE;
hp->adapter.algo_data = &hp->bit;
hp->adapter.dev.parent = hp->dev;
device_set_node(&hp->adapter.dev, dev_fwnode(hp->dev));
strscpy(hp->adapter.name, "planeradar-hyperpixel2r",
        sizeof(hp->adapter.name));
```

Register with `i2c_bit_add_bus` and install a managed cleanup action that calls
`i2c_del_adapter`. A child-driver probe failure remains local to the FT5x06
device and does not unregister the adapter or DRM panel.

- [ ] **Step 6: Implement temporary 9-bit SPI and panel lifecycle**

`hp2r_write_word` asserts chip select, emits bits 8 through 0, and deasserts
chip select. Clock idles low and each half cycle uses `udelay(5)`, matching the
working 100 kHz stock SPI overlay.

Every SPI transition uses this lock order:

```c
i2c_lock_bus(&hp->adapter, I2C_LOCK_ROOT_ADAPTER);
mutex_lock(&hp->state_lock);
```

It then switches SDA/SCL to push-pull outputs, sends commands, restores both
lines to released inputs, unlocks the state mutex, and finally unlocks the I2C
root adapter. A single exit path performs the I2C restoration after every
command error.

Implement these exact panel semantics:

- `prepare`: no-op when prepared; send `hp2r_prepare_commands`; restore I2C;
  set `prepared = true` only after every command succeeds; leave the
  backlight off. Return the first error after restoring safe I2C inputs.
- `enable`: require prepared; set backlight active; set `enabled = true` only
  after the GPIO operation succeeds.
- `disable`: set backlight inactive; set `enabled = false`.
- `unprepare`: no-op when not prepared; backlight off; send display-off and
  enter-sleep; restore I2C even on error; set `prepared = false`.
- `get_modes`: duplicate one preferred 480×480 mode using the constants in the
  protocol header; set 53 mm × 53 mm display dimensions, RGB666 media-bus
  format, negative sync flags, and negative-edge pixel drive.
- `get_orientation`: return the orientation parsed by
  `of_drm_get_panel_orientation`.

Set `panel.prepare_prev_first = true`, register with `drm_panel_add`, and
remove/disable/unprepare the panel before deleting the I2C adapter.
Finish the module with
`MODULE_DEVICE_TABLE(of, planeradar_hyperpixel2r_of_match)`,
`MODULE_DESCRIPTION("Plane Radar HyperPixel 2.1 Round display and touch bus")`,
`MODULE_LICENSE("GPL")`, and a soft dependency on `edt_ft5x06`.

- [ ] **Step 7: Add Kbuild and cross-build validation**

`kernel/Kbuild` builds:

```make
obj-m += planeradar_hyperpixel2r.o
planeradar_hyperpixel2r-y := \
	planeradar_hyperpixel2r_main.o \
	planeradar_hyperpixel2r_protocol.o
```

The main source is `kernel/planeradar_hyperpixel2r_main.c` so the composite
module retains the required output name `planeradar_hyperpixel2r.ko`.

`kernel/Makefile` delegates to:

```make
KERNELRELEASE ?= $(shell uname -r)
KDIR ?= /lib/modules/$(KERNELRELEASE)/build

modules:
	$(MAKE) -C $(KDIR) M=$(CURDIR) W=1 modules

clean:
	$(MAKE) -C $(KDIR) M=$(CURDIR) clean
```

`scripts/build-hyperpixel-driver.sh` copies `kernel/` into a writable container
directory. It bind-mounts the exported `usr/src` tree at `/usr/src` and the
exported kbuild root at its original absolute `/usr/lib/linux-kbuild-*` path;
this preserves the absolute include and `KBUILD_OUTPUT` paths in the Debian
header package. It then runs:

```bash
make -C "/usr/src/linux-headers-${release}" \
  M=/build/kernel \
  ARCH=arm64 \
  CROSS_COMPILE=aarch64-linux-gnu- \
  W=1 \
  modules
```

Copy the `.ko` to `dist/hyperpixel/<release>/`, run `file`, `readelf -h`,
`modinfo`, and `sha256sum`, then write the complete tab-separated manifest.
Record `source_dirty` from `git status --porcelain`.
`scripts/check-hyperpixel-artifacts.sh` compares live `uname -r` to the
manifest, verifies the checksum, architecture, license, `vermagic`, and
`depends` includes `i2c_algo_bit`.

Before building, require these exact target settings from the exported
`.config`:

```text
CONFIG_DRM_PANEL=y
CONFIG_I2C_ALGOBIT=m
CONFIG_TOUCHSCREEN_EDT_FT5X06=m
CONFIG_OF_OVERLAY=y
CONFIG_DRM_VC4=m
CONFIG_DRM_V3D=m
```

Change `scripts/build-pi.sh` to remove only its own four app artifacts instead
of deleting all of `dist/`, so app builds cannot erase driver bundles.

- [ ] **Step 8: Build against the real target and run tests**

Add:

```toml
[tasks.export-pi-kernel-build]
run = "./scripts/export-pi-kernel-build.sh"

[tasks.build-hyperpixel-driver]
run = "./scripts/build-hyperpixel-driver.sh"

[tasks.check-hyperpixel-artifacts]
run = "./scripts/check-hyperpixel-artifacts.sh"
```

Run:

```bash
mise run export-pi-kernel-build
mise run build-hyperpixel-driver
release="$(ssh shayne@planeradar.local uname -r)"
PLANERADAR_DRIVER_ARTIFACT_DIR="dist/hyperpixel/$release" \
  mise exec -- cargo test --test driver_artifacts
mise run check-hyperpixel-artifacts
```

Expected: the module builds with `W=1`, is AArch64, has GPL license metadata,
has `vermagic` beginning with the live release, and passes the artifact test.

- [ ] **Step 9: Commit the buildable module**

```bash
but status
but diff
but commit rpi-port -m "feat: add combined HyperPixel kernel driver"
```

---

### Task 4: Add and Validate the Plane Radar Device Tree Overlay

**Files:**

- Create: `kernel/planeradar-hyperpixel2r-overlay.dts`
- Modify: `scripts/build-hyperpixel-driver.sh`
- Modify: `scripts/check-hyperpixel-artifacts.sh`
- Modify: `tests/driver_artifacts.rs`

**Interfaces:**

- Consumes: Raspberry Pi base symbols `gpio`, `dpi`, and
  `dpi_18bit_cpadhi_gpio0`.
- Produces: `planeradar,hyperpixel2r` with GPIO properties and an
  `edt,edt-ft5406` I2C child at `0x15`.
- Produces:
  `dist/hyperpixel/<release>/planeradar-hyperpixel2r-<12-char-revision>.dtbo`.
- Produces overlay parameters `rotate`, `touchscreen-inverted-x`,
  `touchscreen-inverted-y`, and `touchscreen-swapped-x-y`.

- [ ] **Step 1: Extend the artifact test and prove it fails**

Require these additional files and manifest keys:

```text
overlay_file
overlay_sha256
overlay_applied_dtb
```

Assert the overlay filename matches
`planeradar-hyperpixel2r-[0-9a-f]{12}.dtbo`, its checksum is correct, and the
applied DTB exists.

Run:

```bash
release="$(ssh shayne@planeradar.local uname -r)"
PLANERADAR_DRIVER_ARTIFACT_DIR="dist/hyperpixel/$release" \
  mise exec -- cargo test --test driver_artifacts
```

Expected: FAIL because the bundle has no custom overlay.

- [ ] **Step 2: Write the custom overlay**

Use this node and graph shape:

```dts
// SPDX-License-Identifier: GPL-2.0-only
#include <dt-bindings/gpio/gpio.h>
#include <dt-bindings/interrupt-controller/irq.h>
#include <dt-bindings/pinctrl/bcm2835.h>

/dts-v1/;
/plugin/;

/ {
	compatible = "brcm,bcm2835";

	fragment@0 {
		target-path = "/";
		__overlay__ {
			planeradar_panel: planeradar-hyperpixel2r {
				compatible = "planeradar,hyperpixel2r";
				sda-gpios = <&gpio 10 GPIO_ACTIVE_HIGH>;
				scl-gpios = <&gpio 11 GPIO_ACTIVE_HIGH>;
				cs-gpios = <&gpio 18 GPIO_ACTIVE_LOW>;
				backlight-gpios = <&gpio 19 GPIO_ACTIVE_HIGH>;
				rotation = <0>;
				#address-cells = <1>;
				#size-cells = <0>;

				polytouch: touchscreen@15 {
					compatible = "edt,edt-ft5406";
					reg = <0x15>;
					interrupt-parent = <&gpio>;
					interrupts = <27 IRQ_TYPE_EDGE_FALLING>;
					touchscreen-size-x = <480>;
					touchscreen-size-y = <480>;
				};

				port {
					panel_in: endpoint {
						remote-endpoint = <&dpi_out>;
					};
				};
			};
		};
	};

	fragment@1 {
		target = <&dpi>;
		__overlay__ {
			status = "okay";
			pinctrl-names = "default";
			pinctrl-0 = <&dpi_18bit_cpadhi_gpio0>;

			port {
				dpi_out: endpoint {
					remote-endpoint = <&panel_in>;
				};
			};
		};
	};

	__overrides__ {
		rotate = <&planeradar_panel>,"rotation:0";
		touchscreen-inverted-x =
			<&polytouch>,"touchscreen-inverted-x?";
		touchscreen-inverted-y =
			<&polytouch>,"touchscreen-inverted-y?";
		touchscreen-swapped-x-y =
			<&polytouch>,"touchscreen-swapped-x-y?";
	};
};
```

- [ ] **Step 3: Compile and apply the overlay in OrbStack**

Preprocess the DTS with the exported common-header include directory and
compile it with symbols:

```bash
aarch64-linux-gnu-gcc-14 -E -nostdinc -undef -D__DTS__ -x assembler-with-cpp \
  -I"/target-root/usr/src/linux-headers-6.18.34+rpt-common-rpi/include" \
  /workspace/kernel/planeradar-hyperpixel2r-overlay.dts \
  -o /build/planeradar-hyperpixel2r-overlay.preprocessed.dts
dtc -@ -I dts -O dtb \
  -o "/out/${overlay_file}" \
  /build/planeradar-hyperpixel2r-overlay.preprocessed.dts
fdtoverlay \
  -i /target-root/boot/firmware/bcm2710-rpi-zero-2-w.dtb \
  -o /out/planeradar-hyperpixel2r-applied.dtb \
  "/out/${overlay_file}"
```

The script derives the common-header path from `target.txt`; it does not
hard-code `6.18.34` in implementation.

- [ ] **Step 4: Validate the merged tree and artifact bundle**

Decompile the merged DTB and require exactly:

- one `compatible = "planeradar,hyperpixel2r"` node;
- SDA 10, SCL 11, CS 18, and backlight 19;
- one FT5x06 child at address `0x15`;
- falling-edge GPIO27 interrupt;
- 480×480 touch bounds;
- reciprocal DPI/panel endpoints; and
- enabled DPI with the 18-bit CPADHI pinctrl.

Add overlay checksum, filename, and applied-DTB filename to the manifest, then:

```bash
mise run build-hyperpixel-driver
mise run check-hyperpixel-artifacts
release="$(ssh shayne@planeradar.local uname -r)"
PLANERADAR_DRIVER_ARTIFACT_DIR="dist/hyperpixel/$release" \
  mise exec -- cargo test --test driver_artifacts
```

Expected: overlay compilation has no warnings, `fdtoverlay` succeeds, and all
artifact checks pass.

- [ ] **Step 5: Commit the overlay**

```bash
but status
but diff
but commit rpi-port -m "feat: describe HyperPixel display and touch"
```

---

### Task 5: Implement Fail-Safe Boot Configuration and Packaging

**Files:**

- Modify: `src/install.rs`
- Modify: `src/cli.rs`
- Modify: `src/main.rs`
- Modify: `tests/boot_config.rs`
- Create: `kernel/dkms.conf`
- Create: `scripts/stage-hyperpixel-tryboot.sh`
- Create: `scripts/verify-hyperpixel-boot.sh`
- Create: `scripts/commit-hyperpixel-boot.sh`
- Create: `scripts/rollback-hyperpixel-boot.sh`
- Create: `docs/hardware/hyperpixel2r-driver.md`
- Modify: `mise.toml`

**Interfaces:**

- Consumes: normal `config.txt`, a validated versioned overlay name, optional
  overlay parameters, the cross-built bundle, and the ARM64 app bundle.
- Produces:
  `DisplaySelection::Stock` and
  `DisplaySelection::Candidate { overlay, parameters }`.
- Produces:
  `select_hyperpixel_overlay`, `validate_boot_config`,
  `stage_tryboot_config`, `commit_display_config`, and
  `rollback_display_config`.
- Produces CLI commands `stage-display`, `commit-display`, and
  `rollback-display`.
- Produces idempotent versioned installation under
  `/usr/lib/planeradar/hyperpixel/<revision>/<kernel-release>/`.

- [ ] **Step 1: Write failing boot-selection tests**

Add named tests for:

- stock selection removing every active custom declaration;
- candidate selection removing the stock declaration and older custom
  declarations;
- comments containing either declaration remaining byte-identical;
- candidate selection being idempotent;
- CRLF and missing-final-newline preservation;
- custom overlay names rejecting slashes, whitespace, commas, and names without
  the `planeradar-hyperpixel2r-` prefix;
- parameters accepting only `rotate=0`, `rotate=90`, `rotate=180`,
  `rotate=270`, `touchscreen-inverted-x`, `touchscreen-inverted-y`, and
  `touchscreen-swapped-x-y`;
- each parameter being emitted on its own `dtparam=` line;
- a 98-byte line passing and a 99-byte line returning
  `InstallError::BootLineTooLong { line, bytes }`;
- stage writing only `tryboot.txt` and leaving normal `config.txt`
  byte-identical;
- commit preserving one normal-config backup and atomically selecting the
  accepted overlay; and
- rollback atomically returning normal config to the stock declaration.

The central expected output is:

```rust
assert_eq!(
    select_hyperpixel_overlay(
        "[all]\ndtoverlay=vc4-kms-dpi-hyperpixel2r\n",
        DisplaySelection::Candidate {
            overlay: "planeradar-hyperpixel2r-0123456789ab",
            parameters: &[
                "touchscreen-swapped-x-y",
                "touchscreen-inverted-x",
            ],
        },
    )
    .expect("candidate config")
    .0,
    concat!(
        "[all]\n",
        "dtoverlay=planeradar-hyperpixel2r-0123456789ab\n",
        "dtparam=touchscreen-swapped-x-y\n",
        "dtparam=touchscreen-inverted-x\n",
    ),
);
```

- [ ] **Step 2: Run the focused tests and verify failure**

```bash
mise exec -- cargo test --test boot_config
```

Expected: compilation fails because `DisplaySelection` and the new functions
do not exist.

- [ ] **Step 3: Implement validated selection and atomic writes**

Add:

```rust
pub const STOCK_HYPERPIXEL_DECLARATION: &str =
    "dtoverlay=vc4-kms-dpi-hyperpixel2r";
pub const PLANERADAR_HYPERPIXEL_PREFIX: &str =
    "planeradar-hyperpixel2r-";
pub const MAX_BOOT_CONFIG_LINE_BYTES: usize = 98;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DisplaySelection<'a> {
    Stock,
    Candidate {
        overlay: &'a str,
        parameters: &'a [&'a str],
    },
}
```

`select_hyperpixel_overlay` removes active trimmed lines beginning with
`dtoverlay=vc4-kms-dpi-hyperpixel2r` or
`dtoverlay=planeradar-hyperpixel2r-`, plus contiguous supported `dtparam`
lines owned by that declaration. Insert exactly one selection under the final
`[all]` section. Validate every output line before returning it.

Refactor the existing durable file write into one private atomic writer used
by normal config and tryboot config. It writes in the destination directory,
preserves mode when replacing a file, uses `0644` for a new `tryboot.txt`,
calls `sync_all` on the file and parent directory, and rejects a changed
preview source while holding the existing sibling lock.

- [ ] **Step 4: Add noninteractive boot commands**

Add Clap variants:

```rust
StageDisplay {
    #[arg(long, default_value = "/boot/firmware/config.txt")]
    boot_config: PathBuf,
    #[arg(long, default_value = "/boot/firmware/tryboot.txt")]
    tryboot_config: PathBuf,
    #[arg(long)]
    overlay: String,
    #[arg(long = "parameter")]
    parameters: Vec<String>,
},
CommitDisplay {
    #[arg(long, default_value = "/boot/firmware/config.txt")]
    boot_config: PathBuf,
    #[arg(long)]
    overlay: String,
    #[arg(long = "parameter")]
    parameters: Vec<String>,
},
RollbackDisplay {
    #[arg(long, default_value = "/boot/firmware/config.txt")]
    boot_config: PathBuf,
},
```

`stage-display` prints `staged <tryboot path>` and never changes normal config.
`commit-display` prints `changed` or `unchanged`.
`rollback-display` prints `changed` or `unchanged`.

- [ ] **Step 5: Add DKMS metadata without building on the Pi**

`kernel/dkms.conf` contains:

```bash
PACKAGE_NAME="planeradar-hyperpixel2r"
PACKAGE_VERSION="0.1.0"
BUILT_MODULE_NAME[0]="planeradar_hyperpixel2r"
DEST_MODULE_LOCATION[0]="/extra"
AUTOINSTALL="yes"
CLEAN="make clean"
MAKE[0]="make KERNELRELEASE=${kernelver} KDIR=/lib/modules/${kernelver}/build modules"
```

The staging script installs the GPL kernel sources under
`/usr/src/planeradar-hyperpixel2r-0.1.0`, installs `dkms`, `evtest`, and `kmod`
with apt, and runs `dkms add` only when that exact name/version is not already
registered. It does not run `dkms build` or `dkms install` for the current
kernel; the accepted cross-built `.ko` remains authoritative.

- [ ] **Step 6: Implement candidate staging**

`scripts/stage-hyperpixel-tryboot.sh` must:

1. require a clean GitButler workspace;
2. read and validate the driver manifest;
3. require the live Pi release to equal the manifest release;
4. require matching `dist/planeradar`, revision, and checksum files;
5. copy the complete bundle to
   `/usr/lib/planeradar/hyperpixel/<revision>/<release>/`;
6. install the module as
   `/lib/modules/<release>/extra/planeradar_hyperpixel2r.ko`;
7. install the versioned overlay in `/boot/firmware/overlays/`;
8. install the DKMS source and register it;
9. run `depmod -a <release>`;
10. execute the staged app binary's `stage-display` command;
11. prove normal `config.txt` has the same checksum captured before staging;
12. prove every `tryboot.txt` line is at most 98 bytes;
13. call `sync`; and
14. print `sudo reboot '0 tryboot'` without executing it.

The script is idempotent when the exact manifest is staged twice.

- [ ] **Step 7: Implement verification, commit, and rollback scripts**

`scripts/verify-hyperpixel-boot.sh` fails unless:

- SSH opens within eight seconds;
- `uname -m` is `aarch64` and `uname -r` matches the manifest;
- `/proc/device-tree/chosen/bootloader/tryboot` reports one when invoked with
  `--expect-tryboot`, and does not report one with `--expect-normal`;
- `planeradar_hyperpixel2r`, `i2c_algo_bit`, `edt_ft5x06`, `vc4`, and `v3d`
  are loaded;
- the custom platform node is bound;
- one DRM connector is connected at 480×480;
- one input event device name matches `EDT` or `FT5`;
- the input axes report maxima of 479 or 480;
- the deployed app revision matches the manifest source revision;
- `/healthz` returns the same revision;
- SDL journal output contains `kmsdrm` and `opengles2`;
- stopping the transient app service completes within ten seconds; and
- the current boot journal has no driver warning, blocked-task warning, failed
  unit, or kernel oops.

The verification script starts the staged app with:

```bash
sudo systemd-run \
  --unit=planeradar-hyperpixel-checkpoint \
  --collect \
  --uid=shayne \
  --property=AmbientCapabilities=CAP_NET_BIND_SERVICE \
  --setenv=SDL_VIDEODRIVER=kmsdrm \
  --setenv=SDL_RENDER_DRIVER=opengles2 \
  --setenv=RUST_LOG=info \
  "$artifact_dir/planeradar" run \
  --settings /var/lib/planeradar/settings.json \
  --geocode-cache /var/lib/planeradar/geocode-cache.json \
  --debug-frame /var/lib/planeradar/debug.png \
  --http 0.0.0.0:80
```

It waits for `/healthz`, reads the success log added in Task 1, sends SIGUSR1,
requires a 480×480 debug PNG, and stops the unit with
`timeout 10 sudo systemctl stop planeradar-hyperpixel-checkpoint.service`.

`scripts/commit-hyperpixel-boot.sh` runs the staged app's `commit-display`,
verifies the resulting declaration, calls `sync`, and prints the normal reboot
command.

`scripts/rollback-hyperpixel-boot.sh` runs `rollback-display`, verifies exactly
one stock declaration and zero active custom declarations, calls `sync`, and
prints the normal reboot command. It does not delete versioned artifacts.

- [ ] **Step 8: Document the exact operator flow**

`docs/hardware/hyperpixel2r-driver.md` documents:

- electrical ownership and lifecycle;
- exact-header export and OrbStack build;
- manifest fields and rejection behavior;
- staging and `sudo reboot '0 tryboot'`;
- one-power-cycle recovery;
- automated and physical acceptance;
- permanent commit;
- explicit rollback;
- DKMS behavior after a kernel upgrade; and
- the rule that an active module is never unloaded.

- [ ] **Step 9: Run focused and full checks**

Add mise tasks:

```toml
[tasks.stage-hyperpixel-tryboot]
run = "./scripts/stage-hyperpixel-tryboot.sh"

[tasks.verify-hyperpixel-boot]
run = "./scripts/verify-hyperpixel-boot.sh"

[tasks.commit-hyperpixel-boot]
run = "./scripts/commit-hyperpixel-boot.sh"

[tasks.rollback-hyperpixel-boot]
run = "./scripts/rollback-hyperpixel-boot.sh"
```

Run:

```bash
mise exec -- cargo test --test boot_config --test driver_artifacts
mise run test-driver-protocol
mise run verify
```

Expected: all focused and full tests pass without touching the Pi boot
configuration.

- [ ] **Step 10: Commit the fail-safe packaging**

```bash
but status
but diff
but commit rpi-port -m "feat: stage HyperPixel driver with tryboot"
```

---

### Task 6: Run the One-Shot Hardware Checkpoint

**Files:**

- Modify only after acceptance:
  `.superpowers/sdd/2026-07-25-rpi-plane-radar/progress.md`
- Defects modify the responsible Task 1–5 files with a regression test before
  rebuilding.

**Interfaces:**

- Consumes: committed Rust app, module, overlay, manifest, target scripts, the
  working stock normal boot, and physical user input.
- Produces: automated tryboot evidence plus physical display, tap, motion,
  release, and orientation evidence.
- Produces: an exact decision between SDL input and the conditional evdev
  adapter.

- [ ] **Step 1: Capture the recoverable baseline**

```bash
ssh shayne@planeradar.local '
  set -eu
  test "$(uname -m)" = aarch64
  grep -Fx "dtoverlay=vc4-kms-dpi-hyperpixel2r" /boot/firmware/config.txt
  test ! -e /boot/firmware/tryboot.txt
  test "$(cat /sys/class/drm/card0-DPI-1/status)" = connected
  grep -Fx 480x480 /sys/class/drm/card0-DPI-1/modes
'
```

Copy normal config to a timestamped root-owned sibling backup in
`/boot/firmware/` and record its SHA-256 in the SDD ledger. Do not alter its
contents.

- [ ] **Step 2: Build exact clean artifacts**

```bash
but status
mise run verify
mise run build-pi
mise run export-pi-kernel-build
mise run build-hyperpixel-driver
mise run check-hyperpixel-artifacts
```

Expected: the workspace is clean, both app and driver bundles identify the
same source revision, and the module targets the live kernel release.

- [ ] **Step 3: Stage without changing normal config**

```bash
mise run stage-hyperpixel-tryboot
ssh shayne@planeradar.local '
  set -eu
  grep -Fx "dtoverlay=vc4-kms-dpi-hyperpixel2r" /boot/firmware/config.txt
  grep -E "^dtoverlay=planeradar-hyperpixel2r-[0-9a-f]{12}$" \
    /boot/firmware/tryboot.txt
'
```

Expected: normal config remains stock and `tryboot.txt` selects one versioned
custom overlay.

- [ ] **Step 4: Enter one-shot tryboot**

```bash
ssh shayne@planeradar.local "sudo reboot '0 tryboot'" || true
```

Poll SSH with three-second connection timeouts. If SSH never returns or the
screen remains unusable, ask the user to power-cycle once; verify the stock
normal config and display return before diagnosing and adding a regression
test.

- [ ] **Step 5: Verify kernel display and input**

```bash
./scripts/verify-hyperpixel-boot.sh --expect-tryboot
```

Expected: custom module bound, DRM 480×480 connected, VC4/V3D active, FT5x06
input present, SDL accelerated, app healthy, and bounded shutdown.

Identify the input device:

```bash
ssh shayne@planeradar.local '
  for name in /sys/class/input/event*/device/name; do
    if grep -Eiq "EDT|FT5" "$name"; then
      basename "$(dirname "$(dirname "$name")")"
    fi
  done
'
```

Run `sudo timeout 20 evtest /dev/input/<event>` while the user presses the
center and four cardinal regions. Record press, motion, release, and plausible
axis values.

- [ ] **Step 6: Prove SDL receives the kernel events**

Run the candidate `planeradar probe` as a transient KMS service for 30 seconds.
Ask the user to touch the center and four cardinal regions. Require journal
lines for pressed, moved, and released events and visible magenta touch dots in
matching positions.

If `evtest` succeeds but the SDL probe has zero finger events, add exactly this
fallback:

- Linux dependency `evdev = "0.13"`;
- `src/evdev_touch.rs` with one worker opening the discovered FT5x06 event
  device as `O_NONBLOCK`;
- a bounded `sync_channel<InputEvent>` consumed with `try_iter()` by the SDL
  loop;
- a stop flag plus join handle with no raw I2C access; and
- tests for ABS_MT_SLOT, ABS_MT_TRACKING_ID, ABS_MT_POSITION_X/Y, SYN_REPORT,
  disconnect, full channel, and worker shutdown.

Run the Task 1 source guard, focused evdev tests, full verification, clean
cross-build, restage, and repeat the one-shot tryboot checkpoint.

- [ ] **Step 7: Run Plane Radar gesture acceptance**

Start the full app with its existing private settings and verify:

1. the approved radar or QR screen remains edge-to-edge, true black, sharp,
   and visually identical to the accepted accelerated build;
2. one tap performs exactly one state action;
3. a continuous three-second hold performs the alternate action before
   release;
4. release after the hold does not also perform a tap;
5. touch coordinates match the visible orientation;
6. the web setup remains reachable; and
7. stopping the app and issuing a reboot do not hang SSH.

- [ ] **Step 8: Record and commit tryboot acceptance**

Record the app revision, kernel release, module/overlay checksums, manifest,
tryboot flag, DRM mode, renderer, input identity and axes, `evtest` result,
gesture result, visual result, CPU/memory snapshot, shutdown timing, and the
normal-config backup checksum in the SDD ledger.

```bash
but status
but diff
but commit rpi-port -m "docs: record HyperPixel tryboot acceptance"
```

**Hardware checkpoint:** do not continue to Task 7 until the user explicitly
accepts display appearance, tap, long press, and touch orientation.

---

### Task 7: Make the Accepted Driver Permanent and Verify Recovery

**Files:**

- Modify: `docs/hardware/hyperpixel2r-driver.md`
- Modify: `.superpowers/sdd/2026-07-25-rpi-plane-radar/progress.md`

**Interfaces:**

- Consumes: the exact physically accepted tryboot manifest and overlay
  parameters.
- Produces: normal cold boots using the custom driver, a verified stock
  rollback command, and completed main-plan Task 16 evidence.

- [ ] **Step 1: Commit the accepted overlay to normal config**

```bash
mise run commit-hyperpixel-boot
ssh shayne@planeradar.local '
  set -eu
  grep -E "^dtoverlay=planeradar-hyperpixel2r-[0-9a-f]{12}$" \
    /boot/firmware/config.txt
  ! grep -Eq "^dtoverlay=vc4-kms-dpi-hyperpixel2r([,[:space:]]|$)" \
    /boot/firmware/config.txt
'
```

Expected: exactly the accepted custom overlay is active in normal config, the
stock declaration is absent, and the preserved backup still contains the
stock declaration.

- [ ] **Step 2: Reboot normally and repeat automated verification**

```bash
ssh shayne@planeradar.local 'sudo reboot' || true
./scripts/verify-hyperpixel-boot.sh --expect-normal
```

Expected: tryboot is not active and every driver, KMS, input, SDL, app-health,
and shutdown check still passes.

- [ ] **Step 3: Repeat physical tap and long-press checks**

Run the full app and repeat center/cardinal touch, one tap, and continuous
three-second hold. The screen and debug capture must retain the approved
orientation and layout.

- [ ] **Step 4: Verify a cold power cycle**

Ask the user to remove and restore power once. After boot:

```bash
./scripts/verify-hyperpixel-boot.sh --expect-normal
ssh shayne@planeradar.local 'systemctl --failed --no-legend'
```

Expected: SSH, 480×480 KMS, V3D, touch input, HTTP, and Plane Radar all return
without a failed unit.

- [ ] **Step 5: Prove rollback generation without activating it**

Copy normal config to a temporary file and run `rollback-display` against the
copy. Verify the result has exactly one stock declaration, zero active custom
declarations, and no line longer than 98 bytes. Delete only that temporary
test file.

Do not activate rollback while the accepted normal boot is healthy. The
operator command remains:

```bash
mise run rollback-hyperpixel-boot
ssh shayne@planeradar.local 'sudo reboot'
```

- [ ] **Step 6: Finalize documentation and Task 16 evidence**

Update the runbook with the accepted overlay filename, kernel release,
orientation parameters, input device identity, checksums, and measured
shutdown behavior. Mark main-plan Task 16 complete in the SDD ledger only
after the physical checks above pass.

- [ ] **Step 7: Run final verification and commit**

```bash
mise run test-driver-protocol
mise run verify
mise run check-hyperpixel-artifacts
but status
but diff
but commit rpi-port -m "docs: complete HyperPixel touch acceptance"
```

Expected: all local checks pass, the accepted Pi boot is healthy, and
GitButler reports no uncommitted changes.

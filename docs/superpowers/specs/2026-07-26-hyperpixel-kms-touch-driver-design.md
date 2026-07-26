# HyperPixel KMS Touch Driver

## Status

The architecture, lifecycle, recovery, testing, and delivery design were
approved on 2026-07-26 after restoring and physically verifying the accelerated
480×480 display path on the Raspberry Pi Zero 2 W.

This is a narrow out-of-tree Linux driver for one awkward piece of hardware. It
is not a custom kernel distribution. The distinction matters: we need to own a
five-pin handoff, not inherit every future kernel maintenance problem for fun.

## Problem

The HyperPixel 2.1 Round uses GPIO10 and GPIO11 twice:

- the ST7701 panel consumes them as MOSI and clock during 9-bit SPI
  initialization; and
- the FT5x06 touch controller consumes them as SDA and SCL after the panel is
  running.

The current Raspberry Pi KMS overlay binds `spi-gpio` and the ST7701 panel
driver to those pins for the lifetime of the DRM panel. Its device tree also
contains an FT5x06 node, but leaves the associated `i2c-gpio` bus disabled.
Enabling both standard buses cannot work because both drivers request the same
GPIO descriptors exclusively.

Pimoroni's legacy stack proves the electrical handoff works. It initializes the
panel in userspace and then uses the pins for touch. But that stack disables
KMS/V3D, while Plane Radar requires accelerated KMS graphics. The hardware is
capable; the missing part is one owner that understands both phases.

Relevant references:

- [Pimoroni product and software notes](https://shop.pimoroni.com/products/hyperpixel-round)
- [Pimoroni legacy panel driver](https://github.com/pimoroni/hyperpixel2r)
- [Pimoroni FT5x06 touch library](https://github.com/pimoroni/hyperpixel2r-python)
- [Raspberry Pi 6.18 HyperPixel KMS overlay](https://raw.githubusercontent.com/raspberrypi/linux/rpi-6.18.y/arch/arm/boot/dts/overlays/vc4-kms-dpi-hyperpixel2r-overlay.dts)
- [Raspberry Pi ST7701 panel driver](https://raw.githubusercontent.com/raspberrypi/linux/rpi-6.18.y/drivers/gpu/drm/panel/panel-sitronix-st7701.c)

## Goal

Run the existing Plane Radar UX on the HyperPixel at 480×480 with:

- VC4/KMS/V3D and SDL's `opengles2` renderer;
- interrupt-driven touch through the normal Linux input subsystem;
- bounded shutdown and reboot behavior;
- a fail-safe first boot; and
- no synchronous I²C access from the Rust display loop.

## Non-goals

- Building or distributing a complete Raspberry Pi kernel.
- Supporting unrelated ST7701 panels or arbitrary GPIO assignments.
- Replacing the kernel's existing FT5x06 input driver.
- Upstreaming the driver during this implementation cycle.
- Falling back to the legacy framebuffer stack.
- Keeping the direct `/dev/i2c-11` polling path in the application.

## Options Considered

### Combined panel and bus driver

One module owns every shared GPIO, registers the DRM panel, implements the
temporary SPI phase, and then exposes a bit-banged I²C adapter to the standard
FT5x06 driver.

This is the selected design. The ownership model matches the hardware: one
physical bus has one software owner.

### Generic KMS with a userspace handoff

A boot service could initialize the ST7701, release the pins, and apply an I²C
overlay at runtime. This is less kernel code, but display suspend, shutdown,
restart, and error recovery become a systemd choreography problem. The first
prototype also used a `config.txt` line longer than Raspberry Pi's documented
98-character limit, so it was not a fair test of generic KMS. Even with that
corrected, the lifecycle remains fragile.

### Pimoroni's legacy stack

This is proven to support touch, including on the Zero family. It disables
KMS/V3D and therefore fails the graphics requirement.

## Kernel Architecture

The repository will add a GPL-2.0-only kernel package:

```text
kernel/
  Makefile
  dkms.conf
  planeradar_hyperpixel2r.c
  planeradar-hyperpixel2r-overlay.dts
```

`planeradar_hyperpixel2r.ko` will bind to a private
`planeradar,hyperpixel2r` device-tree node. It will exclusively request:

| Function | BCM GPIO |
| --- | ---: |
| Shared MOSI / SDA | 10 |
| Shared clock / SCL | 11 |
| Panel chip select | 18 |
| Backlight | 19 |
| Touch interrupt | 27 |

The module has three responsibilities and no fourth:

1. register a fixed-mode DRM panel for the 480×480 DPI output;
2. serialize and perform the ST7701 9-bit SPI command phase; and
3. register an `i2c-algo-bit` adapter for the steady-state touch bus.

The custom overlay connects the panel endpoint to Raspberry Pi's DPI output
using the same 18-bit color mapping and timings as the working stock driver.
It contains an I²C child node for `edt,edt-ft5406` at address `0x15`, with a
falling-edge interrupt on GPIO27 and 480×480 coordinate bounds. Once the
adapter is registered, the existing kernel `edt-ft5x06` driver probes that
child and creates a normal Linux input device.

VC4 remains responsible for scanout and V3D remains responsible for rendering.
The custom module touches control pins, not pixels.

## Shared-bus Lifecycle

The module starts the shared lines in open-drain I²C mode. A mutex protects its
mode state, and the root I²C adapter lock prevents a touch transfer from
starting during a panel transition.

Panel preparation follows this sequence:

1. lock the I²C adapter and the module state;
2. switch GPIO10 and GPIO11 to push-pull SPI outputs;
3. assert GPIO18 and send the exact ST7701 initialization table;
4. exit sleep mode and enable the panel;
5. restore GPIO10 and GPIO11 to released, open-drain I²C lines;
6. enable the backlight; and
7. release the locks.

Panel unprepare performs the inverse operation:

1. lock and quiesce I²C transfers;
2. switch the shared lines to SPI;
3. send display-off and sleep commands;
4. turn off the backlight;
5. restore the shared lines to safe I²C inputs; and
6. release the locks.

The SPI phase is rare. Touch is the steady state. Making SPI the temporary mode
keeps the common path simple and prevents two kernel subsystems from quietly
fighting over the same pins.

## Application Input Path

The FT5x06 kernel driver emits multitouch events through `/dev/input/event*`.
SDL consumes those events and the existing normalization layer turns them into
Plane Radar pointer events. The existing gesture recognizer continues to own
tap, movement cancellation, and long-press semantics.

The Rust `i2cdev` dependency, `HyperpixelTouch`, `/dev/i2c-11` default, and
display-loop polling will be removed. A stalled electrical transaction must
not be able to put the render thread into uninterruptible sleep again.

If SDL does not expose the event device on this minimal KMS installation, the
fallback is a nonblocking evdev worker feeding the existing input-event
channel. That fallback still consumes the kernel input device; raw I²C does not
return to the application.

## Failure Behavior

Panel and touch failures have different blast radii:

- Failure to acquire the shared GPIOs or register the DRM panel fails the
  module probe and leaves the backlight off.
- Failure during panel preparation returns an error to DRM, restores safe GPIO
  directions, and leaves the backlight off.
- Failure of the FT5x06 child to probe is logged by the input driver but does
  not remove the DRM panel.
- An I²C timeout is bounded by the bit-banged adapter timeout and cannot block
  the Rust renderer.
- Panel transitions wait for an in-flight touch transfer before changing pin
  direction.
- The module cannot be unloaded while DRM holds it in active use. Driver
  upgrades happen through a reboot, where the lifecycle is controlled.
- The Plane Radar app remains reachable over HTTP when touch is absent and
  reports the degraded input state in logs.

## Build and Packaging

Mise remains the entry point for every developer action. New tasks will:

- export or obtain the exact header tree and `Module.symvers` matching the Pi;
- build ARM64 artifacts in OrbStack on the Mac;
- compile the overlay with `dtc`;
- validate the overlay against a Raspberry Pi base device tree;
- verify the module's `vermagic` against `uname -r`;
- stage a fail-safe test boot; and
- install or roll back the accepted artifacts.

The build must refuse to deploy a module whose kernel release differs from the
live Pi. The output manifest records the source revision, kernel release,
module checksum, overlay checksum, and build command.

The module will also include DKMS metadata. Cross-building on the Mac remains
the normal development path; DKMS is the slower safety net that rebuilds this
small module after a Raspberry Pi OS kernel upgrade when matching headers are
available.

The installer places versioned artifacts without overwriting Raspberry Pi's
stock HyperPixel overlay. Rollback switches `config.txt` back to
`vc4-kms-dpi-hyperpixel2r`, removes the custom module from the active boot, and
runs `depmod`.

## Fail-safe Deployment

The first boot uses Raspberry Pi's one-shot `tryboot` mechanism:

1. preserve the current working `config.txt`;
2. write `tryboot.txt` with the same configuration except for the custom
   overlay;
3. verify every generated `config.txt` line is at most 98 characters;
4. sync the boot filesystem;
5. run `sudo reboot '0 tryboot'`; and
6. leave the normal `config.txt` untouched.

The tryboot flag clears before Linux starts. If the candidate boot hangs, one
power cycle returns to the stock overlay automatically. This is the control
plane we were missing during the first experiment.

The candidate becomes permanent only after automated and physical acceptance.
The accepted tryboot configuration is then applied to `config.txt`, synced,
normally rebooted, and verified a second time.

Reference: [Raspberry Pi fail-safe tryboot documentation](https://www.raspberrypi.com/documentation/hardware/raspberrypi/videocore/config_txt.html#fail-safe-os-updates-tryboot).

## Verification

### Build-time checks

- Build the module with the exact target headers, `modpost`, and `W=1`.
- Compile the device-tree overlay without warnings.
- Apply the overlay to a representative Pi Zero 2 W base tree with
  `fdtoverlay`.
- Check module metadata, dependencies, license, architecture, `vermagic`, and
  artifact checksums.
- Run the full Rust `mise run verify` suite after removing direct I²C input.
- Add a regression test proving the display loop and shutdown path cannot wait
  on a touch read.

### Tryboot checks on the Pi

- SSH accepts and opens commands without abnormal delay.
- The custom module is loaded with no probe warning or failed systemd unit.
- DRM reports one connected 480×480 DPI connector.
- VC4 and V3D are loaded, and SDL reports `kmsdrm` plus `opengles2`.
- The FT5x06 device appears under `/dev/input` with 0–479 coordinate bounds.
- `evtest` reports press, motion, and release at plausible physical positions.
- Plane Radar serves `/healthz` and renders its 480×480 debug frame.
- CPU and memory use remain appropriate for the Zero 2 W.
- Stopping the app and shutting down do not hang.

### Physical acceptance

The user will verify:

- the QR setup screen remains edge-to-edge and sharp;
- a tap performs the intended state action;
- a long press performs the intended alternate action;
- touch coordinates match the visible screen orientation;
- the radar screen remains visually identical to the approved accelerated
  build; and
- a cold boot returns to a working display, touch device, HTTP service, and
  SSH session.

Task 16 remains incomplete until those physical checks pass. Only then can the
project proceed to permanent service installation.

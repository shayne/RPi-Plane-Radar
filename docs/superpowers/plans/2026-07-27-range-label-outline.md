# Range Label Outline Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add a crisp one-pixel black glyph outline only to the green radar range readout without restoring a text plate or changing any other rendering or interaction.

**Architecture:** Keep the retained static radar background and current paint order. Draw the range string at the eight surrounding one-pixel integer offsets in black, then draw its unchanged green foreground at the original anchor; dynamic aircraft rendering remains later and therefore topmost.

**Tech Stack:** Rust 1.97.1, `tiny-skia`, `fontdue`, PNG golden tests, cargo-nextest, mise, GitButler, Raspberry Pi OS on AArch64, KMSDRM/OpenGLES2, and the custom HyperPixel 2.1 Round kernel driver.

## Global Constraints

- Only the range readout receives an outline.
- Use exactly the eight combinations of `-1`, `0`, and `1` pixel offsets, excluding `(0, 0)`.
- The outline color is `theme::BACKGROUND`, `[0, 0, 0, 255]`.
- The range string, `theme::SCALE_CAP_HEIGHT`, anchor, position, and `theme::GRID` foreground remain unchanged.
- Do not add a rectangle, filled backplate, blur, translucent shadow, padding, mask buffer, compositor, or public text-rendering API.
- Cardinal, runway, airport, aircraft-tag, and stale-data text remain unoutlined and transparent.
- Preserve the existing paint order so dynamic aircraft graphics win direct overlaps.
- The setup/settings QR screen and its checked-in goldens remain byte-for-byte unchanged.
- Touch gestures and range selection behavior remain unchanged.
- Run repository-defined tool and build commands through mise.
- Use GitButler for all version-control writes on `rpi-port`; do not use `git commit`.
- Do not push or promote the candidate into permanent normal boot in this plan.
- The Pi target is `pi@raspberrypi.local`, AArch64 release `6.18.34+rpt-rpi-v8`.
- `/boot/firmware/config.txt` must remain byte-identical to `/boot/firmware/config.txt.task6-baseline.20260727T003128Z.bak` at SHA-256 `d237a211ad67b941f2c36e08917984143d256793f1aaf348cf7ee4249df7dbeb`.

---

### Task 1: Render the Range Label With a One-Pixel Glyph Outline

**Files:**
- Modify: `src/render/radar.rs:19-22`
- Modify: `src/render/radar.rs:205-220`
- Modify: `tests/render_radar.rs:1-20`
- Modify: `tests/render_radar.rs:273-290`
- Modify: `tests/goldens/radar-empty.png`
- Modify: `tests/goldens/radar-traffic.png`
- Modify: `tests/goldens/radar-stale.png`

**Interfaces:**
- Consumes: `TextRasterizer::draw(&self, &mut Pixmap, &str, f32, f32, TextStyle)`.
- Consumes: `RadarRenderer::render(&mut self, &RadarSnapshot, &RadarSettings, &[Airport], Duration) -> Result<Frame, RenderError>`.
- Produces: the same public renderer API, frame dimensions, typography, and layout; only the range glyph contour gains black pixels.

- [ ] **Step 1: Replace the obsolete transparent-range regression with an exact contour regression**

Add these imports to `tests/render_radar.rs`:

```rust
use fontdue::{Font, FontSettings};
use planeradar::range::{format_range_label, range_preset};
use planeradar::render::text::{
    HorizontalAnchor, TextRasterizer, TextStyle, VerticalAnchor,
};
use tiny_skia::Pixmap;
```

Replace the existing `use planeradar::range::range_preset;` import with the
combined range import above, and add `LABEL` to the existing
`planeradar::render::theme` import list. Add these test helpers after
`test_renderer`:

```rust
fn range_glyph_mask(settings: &RadarSettings) -> Frame {
    let font = Font::from_bytes(
        include_bytes!("../src/assets/DejaVuSans-Bold.ttf") as &[u8],
        FontSettings {
            collection_index: 0,
            scale: 40.0,
            load_substitutions: true,
        },
    )
    .expect("embedded DejaVu font");
    let preset = range_preset(settings.range_index).expect("configured range");
    let label = format_range_label(preset, settings.units);
    let anchor_x = CENTER.0 + GRID_OUTER_RADIUS - 12.0;
    let mut mask = Pixmap::new(SIZE, SIZE).expect("range mask");
    TextRasterizer::new(&font).draw(
        &mut mask,
        &label,
        anchor_x,
        CENTER.1,
        TextStyle {
            cap_height: SCALE_CAP_HEIGHT,
            color: LABEL,
            horizontal: HorizontalAnchor::Right,
            vertical: VerticalAnchor::Middle,
        },
    );
    Frame::new(SIZE, SIZE, mask.take()).expect("range mask frame")
}

fn mask_covers(mask: &Frame, x: u32, y: u32) -> bool {
    mask.pixel(x, y)[3] != 0
}

fn mask_within_one_pixel(mask: &Frame, x: u32, y: u32) -> bool {
    let left = x.saturating_sub(1);
    let top = y.saturating_sub(1);
    let right = x.saturating_add(1).min(SIZE - 1);
    let bottom = y.saturating_add(1).min(SIZE - 1);
    (top..=bottom).any(|mask_y| {
        (left..=right).any(|mask_x| mask_covers(mask, mask_x, mask_y))
    })
}
```

Replace `transparent_range_label_preserves_the_east_scope_line` with:

```rust
#[test]
fn range_label_has_a_one_pixel_shape_only_outline() {
    let settings = configured_settings();
    let frame = test_renderer()
        .render(
            &empty_snapshot(Some(Duration::ZERO)),
            &settings,
            &[],
            Duration::ZERO,
        )
        .expect("render");
    let mask = range_glyph_mask(&settings);
    let center_y = CENTER.1 as u32;
    let scope_x = 380..448;
    let black_pixels = scope_x
        .clone()
        .filter(|&x| frame.pixel(x, center_y) == BACKGROUND)
        .collect::<Vec<_>>();

    assert!(
        !black_pixels.is_empty(),
        "the black contour must separate the range label from the east scope line"
    );
    assert!(
        black_pixels
            .iter()
            .all(|&x| mask_within_one_pixel(&mask, x, center_y)),
        "the range outline must not extend beyond one pixel of glyph coverage"
    );
    assert!(
        black_pixels
            .iter()
            .any(|&x| !mask_covers(&mask, x, center_y)),
        "the black contour must extend outside the green glyph coverage"
    );
    for x in scope_x {
        if !mask_within_one_pixel(&mask, x, center_y) {
            assert_ne!(
                frame.pixel(x, center_y),
                BACKGROUND,
                "a black backplate appeared outside the one-pixel glyph contour at x={x}"
            );
        }
    }

    let mut opaque_glyph_pixels = 0;
    for y in 220..260 {
        for x in 370..450 {
            if mask.pixel(x, y) == LABEL {
                opaque_glyph_pixels += 1;
                assert_eq!(
                    frame.pixel(x, y),
                    GRID,
                    "the green range fill moved or changed at ({x}, {y})"
                );
            }
        }
    }
    assert!(opaque_glyph_pixels > 0, "range mask must contain glyph pixels");
}
```

- [ ] **Step 2: Run the focused regression and capture strict RED**

Run:

```bash
mise exec -- cargo nextest run --all-features -E \
  'test(range_label_has_a_one_pixel_shape_only_outline)'
```

Expected: FAIL at `the black contour must separate the range label from the
east scope line`. The accepted transparent renderer contains no black pixels
on that continuous scope-line segment.

- [ ] **Step 3: Add only the eight black offset passes**

Add this constant beside the other radar-renderer limits in
`src/render/radar.rs`:

```rust
const RANGE_LABEL_OUTLINE_OFFSETS: [(f32, f32); 8] = [
    (-1.0, -1.0),
    (0.0, -1.0),
    (1.0, -1.0),
    (-1.0, 0.0),
    (1.0, 0.0),
    (-1.0, 1.0),
    (0.0, 1.0),
    (1.0, 1.0),
];
```

Replace only the range-label draw block in `draw_grid_labels` with:

```rust
let style = TextStyle {
    cap_height: theme::SCALE_CAP_HEIGHT,
    color: theme::GRID,
    horizontal: HorizontalAnchor::Right,
    vertical: VerticalAnchor::Middle,
};
for (offset_x, offset_y) in RANGE_LABEL_OUTLINE_OFFSETS {
    text.draw(
        pixmap,
        &range_label,
        anchor_x + offset_x,
        theme::CENTER.1 + offset_y,
        TextStyle {
            color: theme::BACKGROUND,
            ..style
        },
    );
}
text.draw(
    pixmap,
    &range_label,
    anchor_x,
    theme::CENTER.1,
    style,
);
```

Do not modify `src/render/text.rs` or any other label draw call.

- [ ] **Step 4: Verify GREEN and preserve the other transparent-label contracts**

Run:

```bash
mise exec -- cargo nextest run --all-features -E \
  'test(range_label_has_a_one_pixel_shape_only_outline) \
  or test(transparent_aircraft_tag_preserves_static_pixels_and_draws_text_last) \
  or test(whitespace_runway_label_does_not_mask_radar_geometry)'
```

Expected: all three tests PASS. This proves the range exception without
restoring aircraft-tag or runway-label plates.

- [ ] **Step 5: Confirm that only the three radar goldens need regeneration**

Record the setup golden checksums:

```bash
shasum -a 256 \
  tests/goldens/settings.png \
  tests/goldens/setup-required.png
```

Expected:

```text
aaf52ee26ab2ba1e351ceb18246f10669e9c2d37a5eb873ff8ef7267460371e7  tests/goldens/settings.png
9e56198bfe65c51bc46e957c9cf0915ed9713e954625468885c548e93ddadb75  tests/goldens/setup-required.png
```

Then run:

```bash
mise exec -- cargo nextest run --all-features -E 'binary(render_radar)'
```

Expected: the behavioral tests pass and exactly
`empty_fixture_matches_golden`, `traffic_fixture_matches_golden`, and
`stale_fixture_matches_golden` fail because the range contour changed. The
actual PNGs are written to `target/golden-failures/`.

- [ ] **Step 6: Regenerate and inspect only the radar fixtures**

Run:

```bash
fixture_dir="$(mktemp -d target/range-outline-fixtures.XXXXXX)"
mise exec -- cargo run --locked -- render-fixtures --output "$fixture_dir"
cp "$fixture_dir"/radar-empty.png tests/goldens/radar-empty.png
cp "$fixture_dir"/radar-traffic.png tests/goldens/radar-traffic.png
cp "$fixture_dir"/radar-stale.png tests/goldens/radar-stale.png
shasum -a 256 \
  "$fixture_dir"/settings.png \
  "$fixture_dir"/setup-required.png \
  tests/goldens/settings.png \
  tests/goldens/setup-required.png
```

Expected: generated and checked-in setup hashes match the two fixed values
from Step 5. Visually inspect all three regenerated radar PNGs at original
resolution and confirm the only new treatment is the black range-glyph
contour.

- [ ] **Step 7: Run the full repository verification**

Run:

```bash
mise run verify
git diff --check
git diff --stat
```

Expected: formatting, Clippy with warnings denied, all tests, and cargo-deny
pass. The tracked diff contains exactly:

- `src/render/radar.rs`;
- `tests/render_radar.rs`; and
- the three radar golden PNGs.

The two setup golden PNGs remain unchanged.

- [ ] **Step 8: Commit with GitButler and stop for independent review**

Run:

```bash
but diff
but commit rpi-port -m "style: outline radar range label"
git show --check
```

Expected: one focused commit above the range-outline design and plan commits,
no uncommitted changes, and no push. Obtain independent spec-compliance and
code-quality approval for the exact committed revision before touching the
Pi.

---

### Task 2: Cross-Build and Validate on the Physical HyperPixel

**Files:**
- Read: `dist/hyperpixel/6.18.34+rpt-rpi-v8/manifest.txt`
- Read: `/boot/firmware/config.txt` on `planeradar.local`
- Read: `/boot/firmware/config.txt.task6-baseline.20260727T003128Z.bak` on `planeradar.local`
- Create ignored evidence: `.superpowers/sdd/2026-07-26-hyperpixel-kms-touch-driver/live-radar-range-outline.png`
- Update ignored evidence only: `.superpowers/sdd/2026-07-26-hyperpixel-kms-touch-driver/task-6-report.md`

**Interfaces:**
- Consumes: the independently reviewed, clean source revision from Task 1.
- Consumes: `mise run build-pi`, `mise run build-hyperpixel-driver`, `mise run check-hyperpixel-artifacts`, `mise run stage-hyperpixel-tryboot`, and `scripts/verify-hyperpixel-boot.sh --expect-tryboot`.
- Produces: a visually accepted one-shot tryboot candidate; permanent normal boot remains unchanged.

- [ ] **Step 1: Stop the current transient app and verify recovery state**

Run:

```bash
ssh -o BatchMode=yes -o ConnectTimeout=8 pi@raspberrypi.local '
  set -eu
  sudo systemctl stop planeradar-gesture-acceptance.service 2>/dev/null || true
  test "$(sudo sha256sum /boot/firmware/config.txt | cut -d" " -f1)" = \
    d237a211ad67b941f2c36e08917984143d256793f1aaf348cf7ee4249df7dbeb
  sudo cmp -s \
    /boot/firmware/config.txt \
    /boot/firmware/config.txt.task6-baseline.20260727T003128Z.bak
  test "$(sudo ss -H -ltn sport = :80 | wc -l)" -eq 0
  test "$(systemctl --failed --no-legend --plain | wc -l)" -eq 0
'
git status --short
but status
```

Expected: normal config matches the recovery baseline, port 80 is free, zero
failed units, and the repository is clean.

- [ ] **Step 2: Cross-build the exact reviewed revision**

Run:

```bash
mise run build-pi
mise run build-hyperpixel-driver
mise run check-hyperpixel-artifacts
revision="$(git rev-parse HEAD)"
tree="$(git rev-parse HEAD^{tree})"
manifest=dist/hyperpixel/6.18.34+rpt-rpi-v8/manifest.txt
awk -F '	' -v wanted="$revision" '
  $1 == "source_revision" && $2 == wanted { found = 1 }
  END { exit !found }
' "$manifest"
awk -F '	' -v wanted="$tree" '
  $1 == "source_tree" && $2 == wanted { found = 1 }
  END { exit !found }
' "$manifest"
awk -F '	' '
  $1 == "source_dirty" && $2 == "false" { found = 1 }
  END { exit !found }
' "$manifest"
```

Expected: the AArch64 app and matching kernel bundle build successfully; the
manifest revision and tree exactly match the clean reviewed workspace.

- [ ] **Step 3: Stage one reversible transaction**

Run exactly once:

```bash
mise run stage-hyperpixel-tryboot
```

Then verify:

```bash
revision="$(git rev-parse HEAD)"
prefix="${revision:0:12}"
ssh -o BatchMode=yes -o ConnectTimeout=8 pi@raspberrypi.local "
  set -eu
  sudo cmp -s \
    /boot/firmware/config.txt \
    /boot/firmware/config.txt.task6-baseline.20260727T003128Z.bak
  sudo grep -Fx 'dtoverlay=planeradar-hyperpixel2r-$prefix' \
    /boot/firmware/tryboot.txt
  test \"\$(cat \
    /usr/lib/planeradar/hyperpixel/$revision/6.18.34+rpt-rpi-v8/planeradar.revision)\" = \
    '$revision'
  test \"\$(systemctl --failed --no-legend --plain | wc -l)\" -eq 0
"
```

Do not reboot if any assertion fails.

- [ ] **Step 4: Reboot once through tryboot and verify the exact candidate**

Record the current boot ID, run `sudo reboot '0 tryboot'`, poll SSH with a
three-second connection timeout until the boot ID changes, then run:

```bash
./scripts/verify-hyperpixel-boot.sh --expect-tryboot
```

Expected: `Verified HyperPixel tryboot boot for revision` followed by the
current `git rev-parse HEAD` value.

- [ ] **Step 5: Launch the full hardware-accelerated application**

Run:

```bash
revision="$(git rev-parse HEAD)"
ssh -o BatchMode=yes -o ConnectTimeout=8 pi@raspberrypi.local "
  set -eu
  sudo rm -f /var/lib/planeradar/debug.png
  sudo systemd-run \
    --unit=planeradar-gesture-acceptance \
    --collect \
    --uid=pi \
    --property=StateDirectory=planeradar \
    --property=StateDirectoryMode=0750 \
    --property=AmbientCapabilities=CAP_NET_BIND_SERVICE \
    --setenv=SDL_VIDEODRIVER=kmsdrm \
    --setenv=SDL_RENDER_DRIVER=opengles2 \
    --setenv=RUST_LOG=info \
    /usr/lib/planeradar/hyperpixel/$revision/6.18.34+rpt-rpi-v8/planeradar run \
    --settings /var/lib/planeradar/settings.json \
    --geocode-cache /var/lib/planeradar/geocode-cache.json \
    --debug-frame /var/lib/planeradar/debug.png \
    --http 0.0.0.0:80
"
```

Poll `/healthz` with `Host: planeradar.local` until it reports the exact
revision, `"state":"RADAR"`, and `"data_stale":false`.

- [ ] **Step 6: Capture and physically accept the range outline**

Signal the app with `SIGUSR1`, wait for `/var/lib/planeradar/debug.png`, and
copy it to:

```text
.superpowers/sdd/2026-07-26-hyperpixel-kms-touch-driver/live-radar-range-outline.png
```

Inspect the 480-by-480 PNG at original resolution and obtain explicit user
acceptance on the physical display that:

- the range readout has a crisp one-pixel black glyph outline;
- the range readout has no black rectangle, blur, or shadow;
- its green fill, position, size, and sharpness are unchanged;
- every other radar label remains transparent and unoutlined;
- aircraft symbols and tags remain topmost on direct overlap;
- the overall colors, geometry, and full-screen bounds remain unchanged; and
- one short tap advances range exactly once without flashing the QR screen.

Record the exact revision, boot ID, verifier result, screenshot SHA-256, and
user acceptance in the ignored Task 6 report. Leave the transient service
running for inspection and leave permanent boot promotion for the existing
Task 7 gate.

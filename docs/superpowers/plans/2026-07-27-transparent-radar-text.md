# Transparent Radar Text Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Remove every opaque radar-text backplate while preserving the existing typography, geometry, paint order, touch behavior, and hardware-accelerated display path.

**Architecture:** Keep the current retained radar background and dynamic traffic passes. Remove only the three masking operations behind range, runway/airport, and aircraft-tag text; ordinary glyph alpha compositing will make later aircraft glyph pixels win while earlier radar graphics remain visible through glyph gaps.

**Tech Stack:** Rust 1.97.1, `tiny-skia`, `fontdue`, PNG golden tests, cargo-nextest, mise, GitButler, Raspberry Pi OS on AArch64, KMSDRM/OpenGLES2, and the custom HyperPixel 2.1 Round kernel driver.

## Global Constraints

- No radar label may paint a rectangle, translucent plate, outline, halo, or shadow behind its text.
- The setup/settings QR view and its functional white QR tile are unchanged.
- Text content, anchors, positions, sizes, colors, clipping, and integer sizing remain unchanged.
- Aircraft projection, traffic ordering, vectors, symbols, runway geometry, and range behavior remain unchanged.
- Preserve the existing paint order: grid and axes, runways, static labels, aircraft vectors and symbols, aircraft tags, then stale notice.
- All tool and build commands run through mise where the repository defines a mise task or tool.
- All version-control writes use GitButler on `rpi-port`; do not use `git commit`.
- Do not push, commit the permanent boot configuration, or weaken the recovery path in this plan.
- The Pi target is `shayne@planeradar.local`, AArch64 release `6.18.34+rpt-rpi-v8`.
- `/boot/firmware/config.txt` must remain byte-identical to `/boot/firmware/config.txt.task6-baseline.20260727T003128Z.bak` at SHA-256 `d237a211ad67b941f2c36e08917984143d256793f1aaf348cf7ee4249df7dbeb`.

---

### Task 1: Render All Radar Text Without Backplates

**Files:**
- Modify: `src/render/radar.rs:22`
- Modify: `src/render/radar.rs:206-229`
- Modify: `src/render/radar.rs:288-333`
- Modify: `src/render/radar.rs:440-485`
- Modify: `src/render/radar.rs:791-836`
- Modify: `tests/render_radar.rs:201-265`
- Modify: `tests/render_radar.rs:287-313`
- Modify: `tests/goldens/radar-empty.png`
- Modify: `tests/goldens/radar-traffic.png`
- Modify: `tests/goldens/radar-stale.png`

**Interfaces:**
- Consumes: `RadarRenderer::render(&mut self, &RadarSnapshot, &RadarSettings, &[Airport], Duration) -> Result<Frame, RenderError>`.
- Consumes: existing `FrameAssertions`, `configured_settings`, `empty_snapshot`, `aircraft`, and `runway_airport` test helpers.
- Produces: the same `RadarRenderer` API and frame dimensions; only the pixels formerly occupied by text backplates change.

- [ ] **Step 1: Replace the opaque-overlap regression and add transparent-label regressions**

Replace `aircraft_tag_masks_the_range_label_where_they_overlap` with:

```rust
#[test]
fn transparent_aircraft_tag_preserves_static_pixels_and_draws_text_last() {
    let settings = configured_settings();
    let plane = aircraft(11.6, 0.0);
    let snapshot = RadarSnapshot {
        aircraft: Arc::from([plane.clone()]),
        fetched_at: Some(Duration::ZERO),
        last_error_at: None,
    };
    let mut renderer = test_renderer();
    let empty = renderer
        .render(
            &empty_snapshot(Some(Duration::ZERO)),
            &settings,
            &[],
            Duration::ZERO,
        )
        .expect("empty render");
    let traffic = renderer
        .render(&snapshot, &settings, &[], Duration::ZERO)
        .expect("traffic render");

    let location = settings.location.as_ref().expect("configured location");
    let preset = range_preset(settings.range_index).expect("configured range");
    let east = offset_km(location, plane.latitude, plane.longitude).east;
    let aircraft_x =
        CENTER.0 + (east * f64::from(GRID_OUTER_RADIUS) / preset.outer_km) as f32;
    let tag_anchor_x =
        aircraft_x - AIRCRAFT_NOSE_LENGTH - AIRCRAFT_TAIL_HALF_WIDTH - AIRCRAFT_LABEL_GAP;
    let overlap_width = 24;
    let overlap_left = tag_anchor_x.floor() as u32 - overlap_width;
    let overlap_top = (CENTER.1 - AIRCRAFT_TAG_CAP_HEIGHT / 2.0).floor() as u32;
    let overlap_height = AIRCRAFT_TAG_CAP_HEIGHT.ceil() as u32;
    let empty_grid = empty.color_count(
        GRID,
        overlap_left,
        overlap_top,
        overlap_width,
        overlap_height,
    );
    let traffic_grid = traffic.color_count(
        GRID,
        overlap_left,
        overlap_top,
        overlap_width,
        overlap_height,
    );

    assert!(empty_grid > 0, "overlap precondition needs range-label pixels");
    assert!(
        traffic_grid > 0,
        "static range pixels must remain visible through transparent glyph gaps"
    );
    assert!(
        traffic_grid < empty_grid,
        "later aircraft glyphs must replace directly overlapped static pixels"
    );
    assert!(
        traffic.color_count(
            TAG_TYPE,
            overlap_left,
            overlap_top,
            overlap_width,
            overlap_height,
        ) > 0,
        "aircraft type glyphs must remain visible"
    );
}
```

Add the range-label regression:

```rust
#[test]
fn transparent_range_label_preserves_the_east_scope_line() {
    let frame = test_renderer()
        .render(
            &empty_snapshot(Some(Duration::ZERO)),
            &configured_settings(),
            &[],
            Duration::ZERO,
        )
        .expect("render");

    for x in 380..448 {
        assert_ne!(
            frame.pixel(x, CENTER.1 as u32),
            BACKGROUND,
            "range-label padding or glyph gap masked the scope line at x={x}"
        );
    }
}
```

Add the runway-label regression:

```rust
#[test]
fn whitespace_runway_label_does_not_mask_radar_geometry() {
    let mut empty_ident = runway_airport();
    empty_ident.ident.clear();
    let mut whitespace_ident = empty_ident.clone();
    whitespace_ident.ident = "   ".to_owned();
    let settings = configured_settings();
    let snapshot = empty_snapshot(Some(Duration::ZERO));

    let empty_frame = test_renderer()
        .render(
            &snapshot,
            &settings,
            std::slice::from_ref(&empty_ident),
            Duration::ZERO,
        )
        .expect("empty-label render");
    let whitespace_frame = test_renderer()
        .render(
            &snapshot,
            &settings,
            std::slice::from_ref(&whitespace_ident),
            Duration::ZERO,
        )
        .expect("whitespace-label render");

    assert_eq!(
        whitespace_frame.pixels(),
        empty_frame.pixels(),
        "a label with no glyph coverage must not paint a background plate"
    );
}
```

- [ ] **Step 2: Run the focused tests and capture strict RED**

Run:

```bash
mise exec -- cargo nextest run --all-features -E \
  'test(transparent_aircraft_tag_preserves_static_pixels_and_draws_text_last) \
  or test(transparent_range_label_preserves_the_east_scope_line) \
  or test(whitespace_runway_label_does_not_mask_radar_geometry)'
```

Expected: all three tests fail against the current renderer:

- the aircraft-tag overlap contains zero `GRID` pixels because of its plate;
- the east axis contains `BACKGROUND` pixels inside the range-label plate; and
- empty versus whitespace runway labels produce different frames because the plate widths differ.

- [ ] **Step 3: Remove only the three radar text masking paths**

In `src/render/radar.rs`:

1. Delete `AIRCRAFT_TAG_BACKPLATE_PADDING`.
2. In `draw_grid_labels`, retain `range_label`, `anchor_x`, and the existing `text.draw`, but delete the `text.measure` call and `fill_rectangle`.
3. In `draw_runways`, retain label placement and the existing `text.draw`, but delete the `text.measure` call and `fill_rectangle`.
4. In `draw_aircraft_tag`, retain all existing measurements, placement, and the three-line `text.draw` loop, but delete the `aircraft_tag_backplate` call and `fill_rectangle`.
5. Delete the now-unused private `aircraft_tag_backplate` and `fill_rectangle` helpers.
6. Remove `Rect` from the `tiny_skia` imports.

The resulting range-label path must have this shape:

```rust
let preset = range_preset(settings.range_index)?;
let range_label = format_range_label(preset, settings.units);
let anchor_x = theme::CENTER.0 + theme::GRID_OUTER_RADIUS - 12.0;
text.draw(
    pixmap,
    &range_label,
    anchor_x,
    theme::CENTER.1,
    TextStyle {
        cap_height: theme::SCALE_CAP_HEIGHT,
        color: theme::GRID,
        horizontal: HorizontalAnchor::Right,
        vertical: VerticalAnchor::Middle,
    },
);
```

The aircraft-tag path must go directly from `top` calculation into the
unchanged text loop:

```rust
let top = (y - block_height / 2.0)
    .clamp(1.0, theme::SIZE as f32 - block_height - 1.0);
for (index, (line, color)) in lines.into_iter().enumerate() {
    text.draw(
        pixmap,
        line,
        anchor_x,
        top + line_height * index as f32,
        TextStyle {
            cap_height: theme::AIRCRAFT_TAG_CAP_HEIGHT,
            color,
            horizontal,
            vertical: VerticalAnchor::Top,
        },
    );
}
```

- [ ] **Step 4: Run focused tests and the renderer suite for GREEN**

Run:

```bash
mise exec -- cargo nextest run --all-features -E \
  'test(transparent_aircraft_tag_preserves_static_pixels_and_draws_text_last) \
  or test(transparent_range_label_preserves_the_east_scope_line) \
  or test(whitespace_runway_label_does_not_mask_radar_geometry)'
mise exec -- cargo nextest run --all-features -E 'binary(render_radar)'
```

Expected: the three focused tests pass. The renderer suite may fail only on
the three radar golden comparisons until Step 5 regenerates them.

- [ ] **Step 5: Regenerate only the radar goldens**

Run:

```bash
fixture_dir="$(mktemp -d)"
mise exec -- cargo run --locked -- render-fixtures --output "$fixture_dir"
cp "$fixture_dir/radar-empty.png" tests/goldens/radar-empty.png
cp "$fixture_dir/radar-traffic.png" tests/goldens/radar-traffic.png
cp "$fixture_dir/radar-stale.png" tests/goldens/radar-stale.png
```

Do not copy `settings.png` or `setup-required.png`. Inspect all three new radar
PNGs and confirm that text geometry is unchanged, no black rectangles remain
behind radar text, and the QR fixtures are untouched.

- [ ] **Step 6: Run complete verification**

Run:

```bash
mise exec -- cargo fmt --all -- --check
mise exec -- cargo check --all-targets
mise exec -- cargo clippy --all-targets -- -D warnings
mise exec -- cargo nextest run --all-features -E 'binary(render_radar)'
mise run verify
git diff --check
```

Expected: every command exits 0; the full suite reports all executed tests
passed with only the existing intentional live test skipped.

- [ ] **Step 7: Commit the reviewed renderer change with GitButler**

Run:

```bash
but status
but diff
but commit rpi-port -m "style: make radar text transparent"
but status
git show --check
```

Expected: one new focused commit above `zrn`; exactly
`src/render/radar.rs`, `tests/render_radar.rs`, and the three radar golden
PNGs are included; GitButler reports no uncommitted changes.

Stop here for independent spec-compliance and code-quality review. Do not
build or touch the Pi until both reviews approve the exact commit.

---

### Task 2: Cross-Build and Validate on the Physical HyperPixel

**Files:**
- Read: `dist/hyperpixel/6.18.34+rpt-rpi-v8/manifest.txt`
- Read: `/boot/firmware/config.txt` on `planeradar.local`
- Read: `/boot/firmware/config.txt.task6-baseline.20260727T003128Z.bak` on `planeradar.local`
- Update evidence only: `.superpowers/sdd/2026-07-26-hyperpixel-kms-touch-driver/task-6-report.md`

**Interfaces:**
- Consumes: reviewed clean source revision from Task 1.
- Consumes: `mise run build-pi`, `mise run build-hyperpixel-driver`, `mise run check-hyperpixel-artifacts`, `mise run stage-hyperpixel-tryboot`, and `scripts/verify-hyperpixel-boot.sh --expect-tryboot`.
- Produces: a visually accepted one-shot tryboot candidate; it does not alter the permanent normal boot configuration.

- [ ] **Step 1: Stop the transient acceptance service and verify recovery state**

Run:

```bash
ssh -o BatchMode=yes -o ConnectTimeout=8 shayne@planeradar.local '
  set -eu
  sudo systemctl stop planeradar-gesture-acceptance.service 2>/dev/null || true
  test "$(sha256sum /boot/firmware/config.txt | cut -d" " -f1)" = \
    d237a211ad67b941f2c36e08917984143d256793f1aaf348cf7ee4249df7dbeb
  cmp -s \
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

Expected: AArch64 app and kernel bundle build successfully; manifest revision
and tree exactly match the clean GitButler workspace revision.

- [ ] **Step 3: Stage one transaction and confirm normal boot remains unchanged**

Run once:

```bash
mise run stage-hyperpixel-tryboot
```

Then run:

```bash
revision="$(git rev-parse HEAD)"
prefix="${revision:0:12}"
ssh -o BatchMode=yes -o ConnectTimeout=8 shayne@planeradar.local "
  set -eu
  cmp -s \
    /boot/firmware/config.txt \
    /boot/firmware/config.txt.task6-baseline.20260727T003128Z.bak
  grep -Fx 'dtoverlay=planeradar-hyperpixel2r-$prefix' \
    /boot/firmware/tryboot.txt
  test \"\$(cat \
    /usr/lib/planeradar/hyperpixel/$revision/6.18.34+rpt-rpi-v8/planeradar.revision)\" = \
    '$revision'
  test \"\$(systemctl --failed --no-legend --plain | wc -l)\" -eq 0
"
```

Expected: tryboot selects the new revision and normal boot still matches the
baseline. Do not reboot if any assertion fails.

- [ ] **Step 4: Reboot through one-shot tryboot and run automated verification**

Run:

```bash
ssh -o BatchMode=yes -o ConnectTimeout=8 \
  shayne@planeradar.local "sudo reboot '0 tryboot'"
```

Poll in the same terminal until SSH returns:

```bash
for attempt in $(seq 1 80); do
  if ssh -o BatchMode=yes -o ConnectTimeout=3 -o ConnectionAttempts=1 \
    shayne@planeradar.local 'cat /proc/sys/kernel/random/boot_id'
  then
    break
  fi
  sleep 3
done
./scripts/verify-hyperpixel-boot.sh --expect-tryboot
```

Expected: a new boot ID and
`Verified HyperPixel tryboot boot for revision`, followed by the current
`git rev-parse HEAD` value.

- [ ] **Step 5: Launch the full application with preserved settings**

Run:

```bash
revision="$(git rev-parse HEAD)"
ssh -o BatchMode=yes -o ConnectTimeout=8 shayne@planeradar.local "
  set -eu
  sudo rm -f /var/lib/planeradar/debug.png
  sudo systemd-run \
    --unit=planeradar-gesture-acceptance \
    --collect \
    --uid=shayne \
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

Poll `/healthz` until it reports the exact revision and `RADAR`.

- [ ] **Step 6: Capture and visually accept the transparent rendering**

Run:

```bash
ssh -o BatchMode=yes -o ConnectTimeout=8 shayne@planeradar.local '
  sudo systemctl kill \
    --kill-whom=main \
    --signal=SIGUSR1 \
    planeradar-gesture-acceptance.service
  for attempt in $(seq 1 40); do
    sudo test -s /var/lib/planeradar/debug.png && exit 0
    sleep 0.25
  done
  exit 1
'
scp -q -o BatchMode=yes -o ConnectTimeout=8 \
  shayne@planeradar.local:/var/lib/planeradar/debug.png \
  .superpowers/sdd/2026-07-26-hyperpixel-kms-touch-driver/live-radar-transparent-text.png
```

Inspect the captured PNG and obtain explicit user acceptance on the physical
display that:

- no radar label has a block, halo, outline, or shadow;
- rings, axes, runways, and earlier graphics show through glyph gaps;
- dynamic aircraft glyph pixels appear above directly overlapped static
  pixels;
- typography, colors, geometry, sharpness, and full-screen bounds are
  unchanged; and
- one short tap advances range exactly once without flashing the QR screen.

Record the exact revision, boot ID, verifier invocation, screenshot checksum,
and user acceptance in the Task 6 report. Leave permanent boot promotion for
the existing Task 7 gate.

# Radar Visual Weight Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make the radar and setup backgrounds true black and reduce radar visual weight with native whole-pixel sizes while preserving the edge-to-edge spatial layout.

**Architecture:** Keep the existing 480×480 renderer, projection, cache, and SDL presentation path. The setup renderer composes a white QR tile (full quiet zone and light modules) with black dark modules over the shared black canvas, and uses light surrounding text; regenerate deterministic golden PNGs rather than adding a scaling layer.

**Tech Stack:** Rust 1.97.1, tiny-skia, fontdue, PNG golden tests, mise, Docker Buildx/OrbStack, SDL2 KMSDRM with accelerated OpenGL ES 2, GitButler.

## Global Constraints

- Canvas remains 480×480 with center `(240, 240)`.
- Outer grid radius remains 214 pixels, aircraft safe radius remains 188 pixels, and rim radius remains 238 pixels.
- Ring positions, projected aircraft/airport/runway positions, track-vector scale, touch coordinates, and gesture regions do not move.
- Shared background becomes opaque true black `[0, 0, 0, 255]`.
- Setup uses that black canvas, a native white QR tile including the full quiet zone and light modules, black QR dark modules, and light surrounding text.
- Every adjusted size is a whole number of native pixels; do not add a scene or frame scaling transform.
- Curves, diagonals, and glyph edges retain normal antialiasing.
- Production display remains `kmsdrm` plus hardware-accelerated `opengles2`; there is no software-renderer fallback.
- Cross-build on the Mac and verify the exact checksummed artifact on the Raspberry Pi Zero 2 W.

---

### Task 1: Refine Native Radar Visual Weight

**Files:**
- Modify: `src/render/theme.rs`
- Modify: `src/render/setup.rs`
- Modify: `tests/render_radar.rs`
- Modify: `tests/render_setup.rs`
- Regenerate: `tests/goldens/radar-empty.png`
- Regenerate: `tests/goldens/radar-traffic.png`
- Regenerate: `tests/goldens/radar-stale.png`
- Regenerate: `tests/goldens/setup-required.png`
- Regenerate: `tests/goldens/settings.png`

**Interfaces:**
- Consumes: existing `render::theme` constants, `RadarRenderer`, `SetupRenderer`, and `render-fixtures --output tests/goldens`.
- Produces: the same public constant names and types with refined values; renderer callers and SDL display code do not change.

- [ ] **Step 1: Write failing contract tests**

Extend the `planeradar::render::theme` import in `tests/render_radar.rs` to include every fixed and adjusted metric:

```rust
use planeradar::render::theme::{
    AIRCRAFT, AIRCRAFT_LABEL_GAP, AIRCRAFT_NOSE_LENGTH, AIRCRAFT_SAFE_RADIUS,
    AIRCRAFT_TAG_CAP_HEIGHT, AIRCRAFT_TAIL_HALF_WIDTH, AIRCRAFT_TAIL_LENGTH, BACKGROUND,
    CARDINAL_CAP_HEIGHT, CENTER, CENTER_DOT_RADIUS, GRID, GRID_OUTER_RADIUS, GRID_STROKE_WIDTH,
    RIM_DOT_RADIUS, RIM_RADIUS, RUNWAY, RUNWAY_LABEL_CAP_HEIGHT, RUNWAY_LABEL_GAP,
    RUNWAY_STROKE_WIDTH, SCALE_CAP_HEIGHT, SIZE, STALE, STALE_CAP_HEIGHT, TRACK,
    TRACK_MIN_LENGTH, TRACK_STROKE_WIDTH,
};
```

Add this test:

```rust
#[test]
fn visual_metrics_use_whole_pixels_without_moving_the_radar() {
    assert_eq!(BACKGROUND, [0, 0, 0, 255]);
    assert_eq!(SIZE, 480);
    assert_eq!(CENTER, (240.0, 240.0));
    assert_eq!(GRID_OUTER_RADIUS, 214.0);
    assert_eq!(AIRCRAFT_SAFE_RADIUS, 188.0);
    assert_eq!(RIM_RADIUS, 238.0);

    let refined = [
        (GRID_STROKE_WIDTH, 3.0),
        (CENTER_DOT_RADIUS, 3.0),
        (AIRCRAFT_NOSE_LENGTH, 13.0),
        (AIRCRAFT_TAIL_LENGTH, 5.0),
        (AIRCRAFT_TAIL_HALF_WIDTH, 6.0),
        (AIRCRAFT_LABEL_GAP, 2.0),
        (TRACK_MIN_LENGTH, 3.0),
        (TRACK_STROKE_WIDTH, 3.0),
        (RIM_DOT_RADIUS, 6.0),
        (RUNWAY_STROKE_WIDTH, 3.0),
        (RUNWAY_LABEL_GAP, 5.0),
        (CARDINAL_CAP_HEIGHT, 22.0),
        (SCALE_CAP_HEIGHT, 18.0),
        (AIRCRAFT_TAG_CAP_HEIGHT, 21.0),
        (RUNWAY_LABEL_CAP_HEIGHT, 22.0),
        (STALE_CAP_HEIGHT, 18.0),
    ];
    for (actual, expected) in refined {
        assert_eq!(actual, expected);
        assert_eq!(actual.fract(), 0.0);
    }
}
```

Add setup rendering contracts that prove representative canvas pixels are black, the full QR tile including its quiet zone and light modules is white except for black dark modules, surrounding text is light, and opacity/circular-safe bounds remain intact. Change the setup test's expected QR ink constant:

```rust
const INK: [u8; 4] = [0, 0, 0, 255];
```

- [ ] **Step 2: Run the focused tests and verify RED**

Run:

```bash
mise exec -- cargo test --test render_radar visual_metrics_use_whole_pixels_without_moving_the_radar
mise exec -- cargo test --test render_setup setup_frame_encodes_only_the_stable_medium_ec_local_url
```

Expected: both commands fail against the current dark-blue, heavier theme.

- [ ] **Step 3: Apply the minimal theme change**

Set these constants in `src/render/theme.rs`:

```rust
pub const GRID_STROKE_WIDTH: f32 = 3.0;
pub const CENTER_DOT_RADIUS: f32 = 3.0;

pub const AIRCRAFT_NOSE_LENGTH: f32 = 13.0;
pub const AIRCRAFT_TAIL_LENGTH: f32 = 5.0;
pub const AIRCRAFT_TAIL_HALF_WIDTH: f32 = 6.0;
pub const AIRCRAFT_SAFE_RADIUS: f32 = 188.0;
pub const AIRCRAFT_LABEL_GAP: f32 = 2.0;
pub const TRACK_MIN_LENGTH: f32 = 3.0;
pub const TRACK_STROKE_WIDTH: f32 = 3.0;

pub const RIM_RADIUS: f64 = 238.0;
pub const RIM_DOT_RADIUS: f32 = 6.0;
pub const RUNWAY_STROKE_WIDTH: f32 = 3.0;
pub const RUNWAY_LABEL_GAP: f32 = 5.0;

pub const CARDINAL_CAP_HEIGHT: f32 = 22.0;
pub const SCALE_CAP_HEIGHT: f32 = 18.0;
pub const AIRCRAFT_TAG_CAP_HEIGHT: f32 = 21.0;
pub const RUNWAY_LABEL_CAP_HEIGHT: f32 = 22.0;
pub const STALE_CAP_HEIGHT: f32 = 18.0;

pub const BACKGROUND: [u8; 4] = [0, 0, 0, 255];
```

Do not change `SIZE`, `CENTER`, `GRID_OUTER_RADIUS`, `GRID_RING_COUNT`,
`AIRCRAFT_SAFE_RADIUS`, `TRACK_LENGTH_SCALE`, or `RIM_RADIUS`.

- [ ] **Step 4: Run the focused tests and verify GREEN**

Run:

```bash
mise exec -- cargo test --test render_radar visual_metrics_use_whole_pixels_without_moving_the_radar
mise exec -- cargo test --test render_setup setup_frame_encodes_only_the_stable_medium_ec_local_url
```

Expected: both tests pass.

- [ ] **Step 5: Regenerate deterministic golden images**

Run:

```bash
mise exec -- cargo run --locked -- render-fixtures --output tests/goldens
```

Expected: all five 480×480 PNG fixtures are rewritten by the renderer.

- [ ] **Step 6: Verify both renderer suites**

Run:

```bash
mise exec -- cargo test --test render_radar --test render_setup
```

Expected: every radar and setup renderer test passes, including all five golden comparisons.

- [ ] **Step 7: Run the complete local verification**

Run:

```bash
mise run verify
```

Expected: formatting, clippy with warnings denied, all tests, and dependency policy checks pass.

- [ ] **Step 8: Commit the focused implementation with GitButler**

Run:

```bash
but diff
but commit rpi-port -m "style: lighten radar visuals"
```

Expected: one commit containing only the theme, renderer-test, and golden-image changes; the returned workspace state reports no uncommitted changes.

- [ ] **Step 9: Cross-build and deploy the exact committed ARM64 artifact**

Run:

```bash
mise run deploy-pi
cat dist/planeradar.revision
cat dist/planeradar.sha256
cat dist/last-stage-path
```

Expected: Docker reports an ARM aarch64 executable, the Pi verifies the checksum, and the revision equals the new GitButler commit SHA.

- [ ] **Step 10: Run the accelerated physical visual checkpoint**

Using the stage path recorded in Step 9, run:

```bash
stage="$(cat dist/last-stage-path)"
ssh pi@raspberrypi.local "'${stage}/planeradar' demo radar --seconds 60"
```

Expected on the HyperPixel: true black edge-to-edge background, unchanged radar diameter and projected positions, visibly lighter whole-pixel text/symbols/strokes, and no blank frame. Acceptance requires the user's physical visual review.

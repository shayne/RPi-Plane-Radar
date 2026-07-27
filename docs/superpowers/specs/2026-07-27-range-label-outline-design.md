# Range Label Outline Design

## Goal

Separate the green range readout, such as `5km`, from the green radar rings
and east-west axis without restoring an opaque text plate. All other radar
text remains fully transparent as specified by the transparent-radar-text
design.

## Scope

This is a single exception to the earlier prohibition on radar-text outlines:
only the range readout receives a black outline.

The following remain unchanged:

- the range text, cap height, anchor, position, and green fill color;
- cardinal, runway, airport, aircraft-tag, and stale-data text;
- radar geometry, colors, layer order, and aircraft projection;
- the setup/settings QR screen; and
- touch gestures and range selection behavior.

## Rendering Contract

Render the existing range string at the same anchor and size in this order:

1. eight black copies at the one-pixel integer offsets surrounding the
   original glyph position; then
2. the original green range glyphs at the unchanged position.

The offsets are the eight combinations of `-1`, `0`, and `1` on the x and y
axes, excluding `(0, 0)`. This produces a continuous one-pixel black contour
around glyph coverage, including diagonal corners.

The outline must follow glyph shapes. It must not introduce a rectangle,
filled backplate, blur, translucent shadow, or additional padding. The green
fill remains topmost within the range glyphs. Dynamic aircraft graphics still
paint after the cached static background and therefore continue to win direct
overlaps with the outlined range readout.

## Implementation

Keep the change local to radar rendering. Reuse the existing text rasterizer
for the eight integer-offset black passes and the unchanged green foreground
pass. Do not add a new mask buffer, compositor, or public text-rendering API.

The range label is part of the cached static radar background, so the extra
passes occur only when that background is regenerated rather than on every
display frame.

## Verification

Renderer regressions must prove:

- black outline pixels appear immediately adjacent to the range glyphs;
- the outline extends no more than one pixel beyond glyph coverage;
- the green range fill, content, size, and anchor remain unchanged;
- no rectangular black region appears behind the range readout;
- other radar labels remain unoutlined and transparent;
- dynamic aircraft pixels retain their existing topmost paint order; and
- the setup/settings QR rendering is byte-for-byte unchanged.

The focused renderer suite and full `mise run verify` must pass. The reviewed
revision must then be cross-built, staged through the reversible one-shot
tryboot path, boot-verified on the Raspberry Pi Zero 2 W, and visually
accepted on the physical HyperPixel display. A quick tap must continue to
advance the range without flashing the QR screen.

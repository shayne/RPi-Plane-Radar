# Transparent Radar Text Design

## Goal

Keep every label on the radar scope visually transparent so radar lines,
runways, symbols, and other earlier graphics remain visible between the
painted pixels of each glyph. Dynamic aircraft text remains the last radar
text layer and therefore wins wherever its glyph pixels directly overlap an
earlier layer.

## Scope

This change applies only to the radar view:

- aircraft callsign, type, and altitude tags;
- the range label, such as `10km`;
- runway and airport labels; and
- the existing cardinal and stale-status text, which are already transparent.

The setup/settings QR view is unchanged. Its white QR tile is functional
content, not a text backplate.

## Rendering Contract

The radar renderer keeps the existing black base and paint order:

1. grid rings and axes;
2. runways;
3. static radar labels;
4. aircraft vectors and symbols;
5. aircraft tag glyphs; and
6. the stale-data notice when applicable.

No radar label may paint a rectangle, translucent plate, outline, halo, or
shadow behind its text. Overlap is ordinary alpha compositing: a later glyph
pixel replaces the earlier pixel beneath it, while the earlier radar graphics
remain visible through the glyph's transparent gaps.

Text content, anchors, positions, sizes, colors, clipping, and integer sizing
remain unchanged. Aircraft projection, traffic ordering, vectors, symbols,
runway geometry, and range behavior also remain unchanged.

## Implementation

Remove the newly added aircraft-tag backplate and the pre-existing background
rectangles behind range and runway/airport labels. Do not introduce a
replacement masking effect.

Keep the existing layer order rather than adding a new compositor or public
rendering API. Update only radar rendering tests and golden images whose
pixels legitimately change.

## Verification

Renderer regressions must prove:

- the range label no longer interrupts the horizontal scope line in its
  transparent padding and glyph gaps;
- runway geometry remains visible through runway/airport label gaps;
- static grid/range pixels remain present inside an overlapping aircraft-tag
  footprint wherever no aircraft glyph is painted;
- aircraft tag glyph pixels still render above the earlier layers; and
- text metrics, anchors, colors, and the empty-tag behavior remain unchanged.

The focused renderer suite and full `mise run verify` must pass. The reviewed
revision must then be cross-built, staged through the existing one-shot
tryboot path, boot-verified on the Raspberry Pi Zero 2 W, and visually
accepted on the physical HyperPixel display. The previously accepted
single-tap range behavior must remain free of a QR flash.

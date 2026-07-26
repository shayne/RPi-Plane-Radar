# Radar Visual Weight Refinement

## Status

The direction was approved during physical review of the hardware-accelerated
demo on the Raspberry Pi Zero 2 W. This document fixes the numbers before the
renderer changes.

## Goal

Keep the current edge-to-edge 480×480 radar layout while making its visual
weight lighter. “80%” describes the size of the ink, not a smaller viewport.
The display should still feel full-size on the round HyperPixel.

## Fixed Spatial Geometry

These values do not change:

- canvas: 480×480 pixels;
- center: `(240, 240)`;
- outer grid radius: 214 pixels;
- aircraft safe radius: 188 pixels;
- rim radius: 238 pixels;
- ring count and ring positions;
- projected aircraft, airport, and runway positions;
- track-vector distance scale; and
- touch coordinates and gesture regions.

Keeping those values fixed preserves the current diameter and edge coverage.
Scaling the whole scene would create a decorative black bezel. That is not the
requested result.

## Palette

The shared display background changes from dark blue to opaque true black:
`[0, 0, 0, 255]`. Radar and setup screens use the same background so the
physical edge of the round panel disappears into the enclosure. The setup
screen composes a native white QR tile, including every light module and the
full four-module quiet zone, over that black canvas; dark QR modules are
opaque black. Its surrounding URL, instruction, and control text is light so
it remains readable on black. This is not an inverted full-screen QR canvas.

All foreground colors remain unchanged.

## Visual Weight and Pixel Alignment

There is no fractional scene transform. The first Pi prototype's typography,
symbols, dots, strokes, and local spacing shrink to the nearest useful whole
pixel around 80% of their current values:

| Element | Current | Refined |
| --- | ---: | ---: |
| Grid stroke | 4 px | 3 px |
| Center dot radius | 4 px | 3 px |
| Aircraft nose | 16 px | 13 px |
| Aircraft tail | 6 px | 5 px |
| Aircraft half-width | 8 px | 6 px |
| Aircraft-label gap | 2 px | 2 px |
| Minimum track length | 4 px | 3 px |
| Track stroke | 4 px | 3 px |
| Rim-dot radius | 8 px | 6 px |
| Runway stroke | 4 px | 3 px |
| Runway-label gap | 6 px | 5 px |
| Cardinal cap height | 28 px | 22 px |
| Range cap height | 22 px | 18 px |
| Aircraft-tag cap height | 26 px | 21 px |
| Runway-label cap height | 28 px | 22 px |
| Stale-message cap height | 22 px | 18 px |

Text anchors remain where they are. Background masks behind labels continue to
use measured glyph bounds, so reducing text does not leave oversized black
patches. Every configured size is a whole number of native pixels. Curves,
diagonals, and glyph edges keep normal antialiasing, but the renderer never
resamples a completed frame through a fractional scale.

## Verification

The renderer tests will prove that the canvas, center, outer ring, rim, and
projection positions did not move. Palette and representative stroke, symbol,
and text measurements will cover the lighter visual constants. Golden radar
and setup images will be regenerated intentionally.

After native formatting, linting, tests, and dependency checks pass, the exact
committed revision will be cross-built for ARM64 on the Mac, checksummed,
deployed to `planeradar`, and run with the hardware-accelerated `opengles2`
renderer. The user will judge the result on the physical HyperPixel before the
visual checkpoint is accepted.

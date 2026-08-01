# README Tone and Live GIF Design

**Status:** Approved design

**Date:** 2026-08-01

**Repositories:** `shayne/RPi-Plane-Radar`, `shayne/hyperpixel2r-kms`

## Purpose

Both repositories contain complete technical documentation, but parts of the
README copy use jokes and conversational asides that distract from the setup
instructions. The Plane Radar README also uses a single static device capture
as its hero image.

This change makes both READMEs factual, compact, and command-forward. It also
replaces the Plane Radar hero image with a short animation captured from the
running reference device.

## README changes

The existing README structures, commands, URLs, warnings, support boundaries,
and technical explanations remain in place. The edit is a focused tone pass,
not a wholesale rewrite.

For both READMEs:

- begin with a direct description of the software and supported hardware;
- remove jokes, rhetorical framing, and conversational asides;
- use short factual transitions where context is still needed;
- preserve installation order and operational warnings;
- keep the existing AI-assistance disclosure and attribution.

The Plane Radar opening will identify the application as a Rust ADS-B display
for the Raspberry Pi Zero 2 W and Pimoroni HyperPixel 2.1 Round, tested with
64-bit Raspberry Pi OS Lite Trixie. It will state the narrow support boundary
without editorial commentary.

The HyperPixel driver opening will identify the standalone DRM/KMS driver, the
supported display, and the tested operating-system and kernel context. Its
human-readable stable-release text will reflect the published stable release.
The machine-readable release marker and release workflow state will not change
as part of this documentation task.

## Animated device capture

The Plane Radar README will reference a 480 by 480 animated GIF in place of the
current static hero image.

The source material will be captured from the logical framebuffer of the
running reference device for one real minute:

- request one fresh renderer screenshot per second;
- retain 60 ordered PNG frames;
- record enough timing information to confirm the capture spans one minute;
- encode the frames at four frames per second;
- loop the resulting 15-second GIF indefinitely;
- use a generated palette so the radar colors and text remain legible.

No spatial scaling is required. The output stays at the display's native
480 by 480 resolution, preserving integer pixel placement and sharp text.

The existing static PNG remains in the repository as a stable reference and
test fixture. The README hero changes to the GIF.

## Verification

Before landing the change:

- confirm the source capture contains 60 valid 480 by 480 PNG frames;
- confirm the timestamps span approximately one minute;
- confirm the GIF is 480 by 480, loops, and lasts approximately 15 seconds;
- inspect representative frames from the beginning, middle, and end;
- verify both READMEs retain their commands, links, warnings, and attribution;
- run the relevant documentation contracts and repository verification tasks;
- review the final GitButler diffs before committing and landing each repo.

This is a documentation and media update. It does not change the installed
application, display driver, release assets, or device configuration.

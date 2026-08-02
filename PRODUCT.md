# Product

## Register

product

## Users

Plane Radar is used by the owner of a dedicated Raspberry Pi Zero 2 W and
HyperPixel 2.1 Round radar display. The local web interface is opened most
often from a phone during first setup and occasionally from a desktop or
laptop when the owner wants to adjust the location or radar preferences.

The owner is focused on getting the physical appliance configured quickly.
They should not need to understand internal range indices, Raspberry Pi
service details, or the implementation of the ADS-B data source.

## Product Purpose

Plane Radar turns a small round Raspberry Pi display into an always-on,
single-purpose view of nearby aircraft. Its web interface configures the
radar's location, distance units, visible range, and runway overlay from the
same local network.

The web interface succeeds when first-time setup is obvious on a phone,
repeat adjustments are fast on larger screens, and the owner can trust that a
submitted change was accepted without exposing private location data beyond
the Pi.

## Brand Personality

Calm, precise, and instrument-like. The product should feel related to a
civilian aviation instrument while remaining approachable to a hobbyist
setting up a home appliance.

## Anti-references

The interface should not resemble a generic SaaS dashboard, military command
software, neon hacker tooling, or a novelty retro terminal. It should also
avoid decorative cards, excessive instrumentation, and visual effects that
compete with the configuration task.

## Design Principles

1. Make the next setup action obvious.
2. Show radar concepts in the owner's language, not internal model values.
3. Preserve the directness and reliability of a local appliance.
4. Let visual identity support the task without turning configuration into a
   simulation.
5. Keep first-run guidance and repeat-use efficiency in the same surface.

## Accessibility & Inclusion

Target WCAG AA contrast. Provide visible keyboard focus, semantic labels and
groups, 44-pixel minimum touch targets, status cues that do not rely on color
alone, and reduced-motion support. The layout must remain usable on narrow
mobile screens and wide desktop screens without horizontal scrolling.

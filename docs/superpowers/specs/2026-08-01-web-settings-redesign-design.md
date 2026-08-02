# Plane Radar Web Settings Redesign

Status: approved in conversation  
Date: 2026-08-01  
Target repository: `shayne/RPi-Plane-Radar`

## Feature summary

Redesign the local Plane Radar settings page as a responsive appliance
control surface. It serves one owner who most often configures the radar from
a phone during first boot and later makes occasional adjustments from a phone
or desktop.

The page remains a dependency-free, server-rendered Rust surface. It must
improve hierarchy, language, feedback, accessibility, and responsive behavior
without changing the local-only product boundary or weakening the existing
HTTP security controls.

## Primary user action

The owner chooses or enters the radar location, confirms the display
preferences, and applies a valid configuration.

Location is the primary task. Units, visible range, and runway visibility are
secondary but remain available on the same page.

## Design direction

Use a restrained product color strategy. Deep graphite surfaces visually
connect the browser to the physical black radar display. Radar green marks
selection and the primary action; amber is reserved for setup and warning
states. Warm off-white carries primary text, with muted blue-gray supporting
copy. All authored colors use OKLCH values and meet WCAG AA contrast for their
intended text and control roles.

The physical scene is an owner standing near a small always-on radar appliance
in a home office or workshop, opening its local URL on a phone in mixed ambient
light and wanting to finish setup without reading a manual.

Visual anchors are a precise civilian cockpit instrument, the physical Plane
Radar screen, and the quiet control density of Linear. The result must not look
like military command software, neon hacker tooling, a novelty retro terminal,
or a generic SaaS card dashboard.

The visual direction was confirmed in text. Visual probes and north-star image
comps are intentionally omitted because the user requested that the design
stay in chat and the approved direction contains no image-native content.

## Scope

- Fidelity: production-ready.
- Breadth: the single local settings page and all of its server-rendered states.
- Interactivity: shipped semantic HTML and CSS using the existing POST and
  redirect flow.
- Responsive target: narrow mobile through wide desktop.
- Runtime additions: none. No JavaScript framework, external font, icon set,
  image request, or CSS asset is added.

## Information architecture

### Masthead and configuration status

The page begins with a compact masthead containing:

- the Plane Radar name;
- the context label `Local control`;
- the canonical local URL; and
- a status line derived from saved settings.

A configured radar identifies the current saved place, falling back to a
compact latitude and longitude when the place label is empty. An unconfigured
radar says `Setup required` and tells the owner to choose the radar's home
location. The status uses an icon or shape plus text, never color alone.

### Location

Location occupies the dominant column on desktop and appears first on mobile.
It contains:

1. a place search field and `Search` action;
2. an OpenStreetMap privacy disclosure and attribution;
3. zero or more search results; and
4. manual coordinates as a native expandable fallback.

Search results are distinct selectable rows with the complete returned place
name and a `Use location` action. Selecting a result saves that location while
preserving the currently stored radar preferences.

Manual entry includes latitude, longitude, and an optional place name. The
latitude and longitude controls declare decimal input modes, useful bounds,
and step values while retaining server-side validation as authority.

### Radar preferences

Radar preferences occupy the secondary desktop column and follow location on
mobile.

- Units are an accessible two-option radio group styled as a segmented control.
- Range is a four-option radio group. Visible labels use real radar values,
  `5 km`, `10 km`, `15 km`, and `25 km` when kilometres are selected, and the
  same rounded conversion used by the renderer, `3 mi`, `6 mi`, `9 mi`, and
  `16 mi`, when miles are selected. Submitted values remain indices `0` through
  `3` for compatibility with `RadarSettings`.
- Runway visibility is a checkbox styled as a switch with concise explanatory
  text.
- One `Apply settings` button submits manual location and preferences together.

The initial server render uses the selected unit for range labels. Changing
the units control does not rewrite labels before submission because the page
adds no JavaScript; the updated labels appear after the settings are applied.

## Layout and responsive behavior

The page uses an asymmetric desktop grid above 52rem. Location receives the
larger track and preferences receive the smaller track. The masthead and status
span both tracks.

Below 52rem, content becomes one ordered column. Controls fill the available
width where that improves touch use. Coordinate fields may share a row only
when the viewport can preserve their minimum readable width. No viewport may
introduce horizontal page scrolling.

Spacing has three deliberate rhythms: compact label-to-control spacing,
moderate spacing within a task group, and generous separation between the
location and preference regions. Borders and subtle surface shifts establish
structure without wrapping every element in a card. The masthead may use a
low-contrast CSS radar-ring motif that never intersects primary text.

## Typography and controls

Use the system sans-serif stack with no font download. Type hierarchy relies
on size and weight contrast, not a display face. Coordinates and range values
use tabular numerals.

Every interactive control provides default, hover, focus-visible, active, and
disabled treatment where the native element supports the state. Minimum touch
target height is 44 pixels. Focus rings remain visible against every surface.
Transitions last 150 to 200 milliseconds, convey state only, and are removed
when `prefers-reduced-motion: reduce` is active.

## Key states

### Setup required

The status treatment uses amber plus the text `Setup required`. The location
section is visually primary. Empty coordinate inputs do not imply that zero is
a valid default.

### Configured

The status treatment uses radar green plus `Radar configured`, followed by the
saved place label or coordinates. Existing values populate all controls.

### Search results

The page labels the result region with the number of matches and presents each
result as a separate form. Long place names wrap without changing button width
or overflowing the viewport. Submitted search text is not echoed into the
result page.

### Empty search results

The result region says `No matching places found` and points to manual
coordinates. It does not imply a network failure.

### Search unavailable

An alert says `Search unavailable. Enter coordinates manually.` The page keeps
the saved configuration visible and opens or emphasizes the manual fallback.
No provider error or submitted search text is disclosed.

### Invalid settings

The settings response returns the page with HTTP 400 and a concise alert:
`Those settings could not be applied. Check the coordinates and try again.`
The currently saved configuration remains visible. Native input constraints
catch common mistakes before submission, but the server remains authoritative.

### Save failure

The settings response returns the page with HTTP 500 and an alert:
`Plane Radar could not save those settings. Try again.` It reveals no path,
coordinate, or internal error detail.

### Success

A successful update keeps the POST-redirect-GET behavior and redirects to the
fixed, non-sensitive query `/?saved=1`. The resulting page announces
`Radar settings applied` with `role="status"`. Refreshing or directly visiting
the canonical root omits the message.

## Data flow and security

The implementation preserves:

- the HttpOnly, SameSite session cookie;
- per-session CSRF validation;
- exact allowed-host checks;
- Origin or Referer validation for mutations;
- the 16 KiB form limit;
- the bounded request worker count;
- HTML escaping for every dynamic value;
- no-store responses for session-bearing HTML;
- no browser geolocation; and
- no sensitive values in `/healthz` or error copy.

The GET router may accept only the exact success query used by the redirect.
Other unknown paths or query strings continue to return 404. Search and
settings stay as separate forms. Search result selection keeps hidden values
for CSRF, coordinates, place label, and stored preferences.

## Implementation boundaries

`src/web.rs` remains the owner of HTTP routing and page rendering. The page is
small enough that a new frontend build system or static-asset pipeline would
add more complexity than value. Rendering should be decomposed into focused
private helpers for status, messages, search results, range options, and the
final document so the formatter does not become one untestable block.

`src/range.rs` remains the authority for range presets and formatted values.
Web labels must use `range_preset` and `format_range_label` rather than copying
conversion constants.

`tests/web.rs` remains the primary end-to-end contract suite for response
status, HTML, privacy, security, mutation behavior, and responsive CSS. Pure
range behavior remains covered by the existing range tests.

## Verification

Automated verification covers:

- setup-required and configured status copy;
- semantic landmarks, labels, fieldsets, legends, alerts, and status regions;
- meaningful range labels in both unit systems;
- selectable and safely escaped search results;
- empty, unavailable, invalid, save-failure, and success states;
- preservation of stored preferences when a search result is selected;
- mobile viewport metadata, 44-pixel targets, the desktop breakpoint, focus
  styling, and reduced-motion CSS;
- absence of browser geolocation, Wi-Fi configuration, remote assets, and
  sensitive error details; and
- all existing host, CSRF, body-size, concurrency, health, and persistence
  contracts.

Browser verification inspects at least 375×812, 768×1024, and 1440×900. It
must cover setup-required, configured, search-result, search-error, validation,
and success presentations. After the first pass, perform one critique-and-fix
iteration and recapture the affected viewports.

## Open questions

None. The user approved the responsive instrument-console direction, content
hierarchy, restrained visual system, basic accessibility target, behavior,
error handling, and verification scope.

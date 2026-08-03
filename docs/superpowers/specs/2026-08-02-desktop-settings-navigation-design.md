# Plane Radar Desktop Settings Navigation Redesign

Status: approved in conversation  
Date: 2026-08-02  
Target repository: `shayne/RPi-Plane-Radar`

## Feature summary

Replace the settings page's two-column desktop layout with a sticky left
section rail and one continuous content flow. The redesign preserves every
working control, HTTP behavior, default, and progressive-disclosure rule while
making the fully configured and fully expanded page usable on desktop.

The current desktop grid gives location and preferences independent columns,
then spans preferences across the location and manual-coordinate rows. At a
1440 by 900 viewport with all optional settings enabled, the preferences column
is 2,077 pixels tall and forces manual coordinates down to 1,380 pixels even
though the location content ends at 548 pixels. The redesign removes this
cross-column row coupling instead of attempting to hide it with collapsed
sections or smaller spacing.

## Approved design direction

Use a quiet, sticky left rail for orientation and a single vertical editing
flow for content. The rail and content are siblings; the rail never
participates in the content's row sizing. Each content section may use a small
internal grid to spend available width, but no top-level content sections sit
in competing columns.

This direction was selected from three browser mockups. The approved detailed
mockup uses the existing dark appliance palette, restrained radar-green
accent, native controls, and divider-led structure. It does not add decorative
cards, a dashboard treatment, or denser typography.

## Goals

- Keep the settings page coherent when every optional disclosure is open.
- Make section navigation persistent and predictable on desktop.
- Preserve one top-to-bottom reading and keyboard order.
- Use desktop width inside sections without creating masonry or coupled rows.
- Preserve the current simple mobile experience.
- Preserve the working settings controls and all existing defaults.

## Non-goals

- No settings schema, provider, radar-rendering, or persistence changes.
- No changes to which optional sections open automatically.
- No client-side state, scroll spy, animation system, or JavaScript.
- No external assets, web fonts, icon packages, or frontend build system.
- No global visual redesign beyond the layout and section organization needed
  for this page.

## Information architecture

### Desktop control rail

At viewports of 64rem and wider, a 13.5rem rail occupies the left side of the
existing bounded shell. It uses `position: sticky` with a top offset matching
the shell padding and remains independently visible while the settings content
scrolls.

The rail contains, in order:

1. the existing Plane Radar identity and `Local control` context;
2. the existing device URL and configured or setup-required status;
3. a `Settings` navigation label;
4. anchor links to Location, Radar basics, Aircraft labels, Footer, and Traffic
   filter; and
5. the desktop `Apply settings` action.

Each link may include a concise server-rendered saved-value summary when it is
useful: location label or coordinates, radar text percentage, number of enabled
aircraft-label options, number of enabled footer fields, and compact altitude
bounds. Summaries are secondary text, are not required to understand the link,
and are never updated in the browser before form submission.

The rail has no automatic scroll tracking. Following a link moves focus or
scroll position to the corresponding semantic section. CSS `:target` may give
the destination a restrained highlight. A link to a collapsed native
`details` element lands on its summary and does not open it automatically.

The rail Apply button submits the existing settings form through the HTML
`form` attribute. It does not submit the separate place-search form. The rail
must fit a 768-pixel-tall desktop viewport; on shorter viewports it may scroll
internally rather than covering content or placing the action off screen.

### Main settings flow

The main content column is capped near 54rem and follows this visual order:

1. notices and confirmation feedback;
2. Location search;
3. Manual coordinates;
4. Radar basics;
5. Aircraft labels;
6. Footer; and
7. Traffic filter.

The place-search and settings forms remain separate HTTP forms. “One content
flow” describes their shared visual reading order, not a change to form
ownership or server behavior.

Sections use a heading, one concise explanatory line where necessary, and
subtle top dividers. Borders establish structure without wrapping every
section in a card. The existing semantic fieldsets, legends, labels, and native
`details` elements remain authoritative.

### Section-local layouts

- Location search uses a fluid input and fixed-width Search action when space
  permits. Manual latitude and longitude share a row, with place name spanning
  the section.
- Radar basics groups units, range, radar text size, and runway visibility in
  the same section. Controls may use two columns where their minimum readable
  widths are preserved.
- Aircraft labels arranges its three existing switches in a compact internal
  grid at wide widths and stacks them on narrow widths.
- Footer separates “Show in footer” switches from formatting controls. Footer
  switches use a compact internal grid, while temperature unit, time zone, and
  clock format retain their existing fieldsets and segmented controls.
- Traffic filter keeps minimum and maximum altitude as the existing paired
  fields.

No section-local grid changes DOM order. Keyboard navigation remains the same
as the visible top-to-bottom, left-to-right order.

## Progressive disclosure

The optional Aircraft labels, Footer, and Traffic filter sections remain native
`details` elements. Default settings keep these sections collapsed. A section
with any non-default saved setting opens on the initial server render, as it
does today. Manual coordinates retain their existing setup-dependent open
behavior.

The fully expanded state is a first-class layout requirement, not an edge case
to solve by forcing disclosures closed.

## Responsive behavior

### Desktop, 64rem and wider

The shell uses two columns: the 13.5rem sticky control rail and one
`minmax(0, 1fr)` content track. The gap is no larger than the existing
`--space-2xl` token. Content sections occupy only the content track and follow
normal document flow.

The desktop rail Apply action is visible and the content-bottom Apply action is
visually hidden. Both target the same settings form, so there is only one
visible primary action.

### Below 64rem

The control rail becomes a static top region. Identity, device URL, and status
remain readable. Section links become a horizontally scrollable strip with
44-pixel touch targets. The rail Apply action is hidden and the existing
content-bottom Apply action is visible.

The main flow uses one column. Section-local grids collapse when they cannot
preserve readable controls. The page must not introduce horizontal document
scrolling at the 320-pixel minimum body width.

## Visual and interaction details

- Reuse the existing OKLCH palette, spacing tokens, system font stack, focus
  ring, control states, and reduced-motion behavior.
- Preserve the current 44-pixel minimum target size for controls, summaries,
  and navigation links.
- Use radar green for the primary action and selected or targeted state only.
- Keep navigation summaries muted and subordinate to link labels.
- Apply `scroll-margin` to anchored sections so destinations are not flush
  against the viewport edge.
- Do not use sticky overlays, floating cards, nested scroll regions in the main
  content, or viewport-height content clipping.

## Accessibility

The section rail is a labelled `nav` landmark. Anchor labels use the visible
section names. Saved-value summaries are either part of concise accessible link
text or hidden from assistive technology when they would create noisy
repetition.

The primary content remains in semantic source order. Focus-visible treatment
must remain apparent on rail links, the rail Apply button, summaries, and every
existing form control. Color is not the only status indicator. Horizontal
navigation scrolling must not suppress keyboard access or visible focus.

The desktop and mobile Apply actions are the same form action presented at
different breakpoints. Hidden actions must not remain in the focus order.

## Data flow, errors, and security

All existing request and response behavior remains unchanged:

- place search uses its current POST action and OpenStreetMap disclosure;
- settings use the current settings POST and POST-redirect-GET success flow;
- invalid settings and save failures render their existing safe notices;
- configured values and search results remain escaped server-side;
- CSRF, host, Origin or Referer, body-size, session, worker-bound, and no-store
  protections remain intact; and
- no location or provider data is added to client-side code because no
  client-side code is added.

Notices appear at the top of the main content so they remain adjacent to the
editing task and are not trapped inside the sticky rail.

## Implementation boundaries

`src/web.rs` remains the owner of server-rendered HTML and CSS. The change may
add small private formatting helpers for rail summaries when that keeps the
document template readable. It must not move settings ownership out of the
existing web module or introduce a static-asset pipeline.

`tests/web.rs` remains the contract suite for generated semantics, responsive
CSS, form association, preserved progressive disclosure, and absence of
JavaScript. No radar renderer or provider implementation should change.

## Verification and acceptance criteria

Automated tests must prove:

- the labelled section navigation and all five anchor destinations render;
- the settings form has a stable identifier and the desktop Apply action is
  explicitly associated with it;
- the desktop layout has one content track beside the 13.5rem rail and no
  location/preferences grid areas;
- the rail becomes sticky only at the 64rem desktop breakpoint;
- the mobile section strip and content-bottom Apply fallback are present;
- optional disclosures retain their current collapsed-default and
  open-when-configured behavior;
- the fully enabled page still renders all controls and settings values;
- the page remains JavaScript-free and dependency-free; and
- all existing web security, persistence, and round-trip tests pass.

Browser verification must cover:

- 1440 by 900 with every optional disclosure expanded;
- 1024 by 768 with every optional disclosure expanded;
- 768 by 1024 with every optional disclosure expanded;
- 390 by 844 in both default-collapsed and fully expanded states; and
- keyboard traversal through the section strip, disclosures, form controls,
  and the visible Apply action at desktop and mobile widths.

At 1440 by 900 and 1024 by 768, Manual coordinates must follow Location in
normal flow with no blank region created by Footer or any other expanded
section. Every section begins after the preceding section with no gap larger
than the intended section spacing. The page has no horizontal document
overflow at any acceptance viewport.

After automated and browser verification, install the resulting local
prerelease on `user@radar.local` through the existing supported local
release-directory application-only path. Confirm the service, health endpoint,
mDNS page, settings preservation, and fully expanded desktop layout before any
release is cut.

## Open questions

None. The user approved direction A: a sticky left navigator with a single
continuous content flow, section-local grids, and a simple mobile fallback.

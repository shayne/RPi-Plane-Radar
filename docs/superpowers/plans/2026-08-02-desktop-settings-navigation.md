# Desktop Settings Navigation Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace the coupled desktop settings columns with a sticky 13.5rem section rail and one responsive content flow that remains usable with every disclosure expanded.

**Architecture:** Keep the page dependency-free and server-rendered in `src/web.rs`. Add a focused navigation renderer, restructure the existing HTML into a control rail plus one content column, and use section-local CSS grids without changing form ownership or settings behavior. Extend `tests/web.rs` with generated-HTML and responsive-CSS contracts, then validate the exact result in desktop and mobile browsers before installing a local prerelease on the physical radar.

**Tech Stack:** Rust 1.97.1, server-rendered HTML and CSS, native HTML forms and `details`, Cargo nextest, Mise, GitButler, Chrome browser inspection, Raspberry Pi systemd deployment.

## Global Constraints

- Preserve every working control, HTTP behavior, default, and progressive-disclosure rule.
- The place-search and settings forms remain separate HTTP forms.
- At viewports of 64rem and wider, use a 13.5rem sticky rail beside one `minmax(0, 1fr)` content track.
- Below 64rem, use a static top region and horizontally scrollable section links with 44-pixel touch targets.
- Keep default optional sections collapsed and open sections with non-default saved settings on initial render.
- Add no client-side state, scroll spy, animation system, JavaScript, external asset, web font, icon package, frontend build system, or runtime dependency.
- Reuse the existing OKLCH palette, spacing tokens, system font stack, focus ring, control states, and reduced-motion behavior.
- Preserve CSRF, host, Origin or Referer, body-size, session, worker-bound, no-store, escaping, and POST-redirect-GET behavior.
- The fully expanded page must have no cross-column displacement or horizontal document overflow at 1440 by 900, 1024 by 768, 768 by 1024, and 390 by 844.
- Install the verified local prerelease on `user@radar.local` through the supported local release-directory application-only path before any release is cut.
- Use GitButler branch `codex/optional-radar-features`; do not create a worktree or commit generated visual-companion state.

---

## File map

| File | Responsibility |
|---|---|
| `src/web.rs` | Render saved-value summaries, labelled section navigation, form association, semantic section anchors, and all responsive page CSS |
| `tests/web.rs` | Contract-test navigation semantics, escaped summaries, form association, breakpoint behavior, top-level flow, internal grids, disclosures, and the JavaScript-free boundary |
| `docs/superpowers/specs/2026-08-02-desktop-settings-navigation-design.md` | Approved behavior and acceptance criteria; read-only during implementation unless a discovered contradiction requires user review |
| `dist/release/` | Generated local prerelease artifacts used for the physical Pi upgrade; never commit |
| `dist/smoke-radar.png` | Generated physical-display smoke capture; never commit |

## Interfaces

The implementation introduces one private rendering boundary in `src/web.rs`:

```rust
fn render_settings_navigation(settings: &RadarSettings) -> String
```

It returns the complete labelled section `nav`, including escaped
server-rendered summaries and links to these stable identifiers:

```text
#location
#radar-basics
#aircraft-labels
#footer
#traffic-filter
```

The existing settings POST form gains `id="settings-form"`. Both primary
actions submit that form:

```html
<button class="button-primary button-rail" type="submit" form="settings-form">Apply settings</button>
<button class="button-primary button-content" type="submit">Apply settings</button>
```

CSS makes exactly one of those actions visible and focusable at each
breakpoint. No server route or public Rust type changes.

---

### Task 1: Render the semantic section rail and responsive content flow

**Files:**
- Modify: `tests/web.rs:476-616`
- Modify: `src/web.rs:914-1178`
- Modify: `src/web.rs:1725-1925`

**Interfaces:**
- Consumes: `RadarSettings`, `Location`, `FooterSettings`, `escape_html`, `render_status`, and the existing `render_page` format arguments.
- Produces: `render_settings_navigation(settings: &RadarSettings) -> String`, stable section IDs, `id="settings-form"`, and the two associated Apply actions.

- [ ] **Step 1: Move generated visual-companion state outside the repository**

Stop the visual-companion server if it is still running. Preserve its files
outside the repository so release packaging can later prove a clean source
tree:

```bash
companion_archive="$(mktemp -d "${TMPDIR:-/tmp}/planeradar-companion.XXXXXX")"
mv /Users/shayne/code/RPi-Plane-Radar/.superpowers/brainstorm "$companion_archive/"
git status --short
```

Expected: `.superpowers/brainstorm` is absent from `git status`, while the
git-ignored `.superpowers/sdd` execution ledger remains in place. The approved
design spec and plan remain committed on `codex/optional-radar-features`.

- [ ] **Step 2: Write failing navigation and form-association tests**

Add these tests beside the current page semantics tests in `tests/web.rs`:

```rust
#[test]
fn settings_page_renders_labelled_section_navigation_and_associated_actions() {
    let response = TestServer::new(optional_settings_enabled(), Vec::new()).get("/");

    assert_eq!(response.status, 200);
    assert!(
        response
            .body
            .contains("<nav class=\"settings-navigation\" aria-label=\"Settings sections\">")
    );
    for (target, label) in [
        ("location", "Location"),
        ("radar-basics", "Radar basics"),
        ("aircraft-labels", "Aircraft labels"),
        ("footer", "Footer"),
        ("traffic-filter", "Traffic filter"),
    ] {
        assert!(
            response.body.contains(&format!("href=\"#{target}\"")),
            "missing navigation target {target:?}"
        );
        assert!(
            response.body.contains(&format!("id=\"{target}\"")),
            "missing section id for {label:?}"
        );
    }
    assert!(
        response
            .body
            .contains("<form id=\"settings-form\" class=\"settings-form\"")
    );
    assert!(response.body.contains(
        "class=\"button-primary button-rail\" type=\"submit\" form=\"settings-form\""
    ));
    assert!(response.body.contains(
        "class=\"button-primary button-content\" type=\"submit\""
    ));
    assert!(!response.body.contains("<script"));
}

#[test]
fn settings_navigation_escapes_saved_location_summary() {
    let mut settings = optional_settings_enabled();
    settings.location.as_mut().unwrap().label = "<script>alert('rail')</script>".to_owned();

    let response = TestServer::new(settings, Vec::new()).get("/");

    assert!(!response.body.contains("<script>alert"));
    assert!(
        response
            .body
            .contains("&lt;script&gt;alert(&#39;rail&#39;)&lt;/script&gt;")
    );
}
```

In `page_exposes_local_settings_without_wifi_or_browser_geolocation`, replace
the obsolete CSS contract strings `@media (min-width: 52rem)`,
`grid-template-areas:`, and `align-content: start` with:

```rust
"@media (min-width: 64rem)",
"grid-template-columns: 13.5rem minmax(0, 1fr);",
".settings-navigation { min-width: 0; overflow-x: auto; }",
```

- [ ] **Step 3: Run the new tests and verify the expected failure**

Run:

```bash
cargo nextest run --test web -E 'test(page_exposes_local_settings_without_wifi_or_browser_geolocation) | test(settings_page_renders_labelled_section_navigation_and_associated_actions) | test(settings_navigation_escapes_saved_location_summary)'
```

Expected: both new tests and the updated CSS contract fail because the
navigation, stable section IDs, associated rail action, and 64rem layout do not
exist.

- [ ] **Step 4: Add the focused navigation renderer**

Insert this helper after `footer_summary` in `src/web.rs`:

```rust
fn render_settings_navigation(settings: &RadarSettings) -> String {
    let location = settings.location.as_ref().map_or_else(
        || "Not set".to_owned(),
        |location| {
            if location.label.trim().is_empty() {
                format!("{:.3}, {:.3}", location.latitude, location.longitude)
            } else {
                location.label.clone()
            }
        },
    );
    let aircraft_count = usize::from(settings.show_callsign)
        + usize::from(settings.show_route)
        + usize::from(settings.show_expanded_model);
    let footer_count = [
        settings.footer.show_condition,
        settings.footer.show_temperature,
        settings.footer.show_humidity,
        settings.footer.show_time,
        settings.footer.show_date,
    ]
    .into_iter()
    .filter(|enabled| *enabled)
    .count();
    let traffic = match (
        settings.minimum_altitude_feet,
        settings.maximum_altitude_feet,
    ) {
        (None, None) => "All altitudes".to_owned(),
        (Some(minimum), None) => format!("{minimum}+ ft"),
        (None, Some(maximum)) => format!("Up to {maximum} ft"),
        (Some(minimum), Some(maximum)) => format!("{minimum}-{maximum} ft"),
    };
    let location = escape_html(&location);
    let traffic = escape_html(&traffic);

    format!(
        r##"<nav class="settings-navigation" aria-label="Settings sections">
<p class="rail-label">Settings</p>
<ul>
<li><a href="#location"><span>Location</span><small>{location}</small></a></li>
<li><a href="#radar-basics"><span>Radar basics</span><small>{}%</small></a></li>
<li><a href="#aircraft-labels"><span>Aircraft labels</span><small>{aircraft_count} on</small></a></li>
<li><a href="#footer"><span>Footer</span><small>{footer_count} on</small></a></li>
<li><a href="#traffic-filter"><span>Traffic filter</span><small>{traffic}</small></a></li>
</ul>
</nav>"##,
        settings.radar_text_scale_percent
    )
}
```

In `render_page`, compute it beside the existing rendered fragments:

```rust
let settings_navigation = render_settings_navigation(settings);
```

- [ ] **Step 5: Restructure the document into rail and content siblings**

Replace the top-level masthead, status, notice, and `console-grid` wrapper with
this exact hierarchy while retaining the existing control bodies unchanged:

```html
<main class="shell">
<div class="control-layout">
<aside class="control-rail">
<header class="masthead">
<span class="radar-mark" aria-hidden="true"><i></i></span>
<div class="brand-lockup">
<p class="eyebrow">Local control</p>
<h1>Plane Radar</h1>
</div>
<a class="device-url" href="{local_url}"><span>Device</span>{local_url}</a>
</header>
{status}
{settings_navigation}
<button class="button-primary button-rail" type="submit" form="settings-form">Apply settings</button>
</aside>
<div class="settings-content">
{notice}
<h2 class="settings-title">Settings</h2>
<section class="location" id="location" aria-labelledby="location-title">
```

Close `section.location` before the settings form as it is today. Change the
settings form opening tag to:

```html
<form id="settings-form" class="settings-form" action="/settings" method="post">
```

Give the existing Radar display section and native disclosures these IDs:

```html
<section class="preferences" id="radar-basics" aria-labelledby="preferences-title">
<details class="option-group" id="aircraft-labels" data-section="aircraft"{aircraft_open}>
<details class="option-group" id="footer" data-section="footer"{footer_open}>
<details class="option-group" id="traffic-filter" data-section="traffic"{traffic_open}>
```

Rename the existing content-bottom button class without changing its action:

```html
<button class="button-primary button-content" type="submit">Apply settings</button>
```

Close `settings-content`, `control-layout`, and `main` after the settings form.
Do not nest the search form in the settings form and do not move any existing
hidden presence sentinel to a different form.

- [ ] **Step 6: Add the minimum flow CSS needed by the new hierarchy**

Remove `.console-grid` grid areas, every `grid-area` declaration, and the
52rem location/preferences column template. Replace `.settings-form {{
display: contents; }}` with a normal one-column flow. Add:

```css
.control-layout {{ display: grid; }}

.control-rail {{
  display: grid;
  gap: var(--space-lg);
  min-width: 0;
}}

.settings-content {{
  width: min(100%, 54rem);
  min-width: 0;
  margin: 0 auto;
}}

.settings-title {{ padding: var(--space-xl) 0 0; }}
.settings-form {{ display: grid; min-width: 0; }}
.button-rail {{ display: none; }}
.button-content {{ display: block; }}

.settings-navigation {{ min-width: 0; overflow-x: auto; }}
.settings-navigation ul {{
  display: flex;
  width: max-content;
  min-width: 100%;
  margin: 0;
  padding: 0;
  list-style: none;
}}
.settings-navigation a {{
  display: grid;
  min-height: 44px;
  align-content: center;
  padding: var(--space-sm) var(--space-md);
  border-radius: var(--radius-sm);
  color: var(--text-muted);
  text-decoration: none;
}}
.settings-navigation a span {{ color: inherit; font-size: 0.875rem; font-weight: 700; }}
.settings-navigation a small {{ color: var(--text-faint); font-size: 0.6875rem; }}
.rail-label {{
  color: var(--text-faint);
  font-size: 0.6875rem;
  font-weight: 800;
  letter-spacing: 0.1em;
  text-transform: uppercase;
}}

#location, #radar-basics, #aircraft-labels, #footer, #traffic-filter {{
  scroll-margin-top: var(--space-xl);
}}

@media (min-width: 64rem) {{
  .shell {{ padding-top: var(--space-xl); }}
  .control-layout {{
    grid-template-columns: 13.5rem minmax(0, 1fr);
    gap: var(--space-2xl);
    align-items: start;
  }}
  .control-rail {{
    position: sticky;
    top: var(--space-xl);
    max-height: calc(100svh - (var(--space-xl) * 2));
    overflow-y: auto;
    padding-right: var(--space-lg);
    border-right: 1px solid var(--border);
  }}
  .control-rail .masthead {{
    grid-template-columns: auto minmax(0, 1fr);
    gap: var(--space-md);
    padding-top: 0;
  }}
  .control-rail .device-url {{ grid-column: 1 / -1; }}
  .settings-navigation {{ overflow: visible; }}
  .settings-navigation ul {{ display: grid; width: auto; }}
  .settings-navigation a {{
    grid-template-columns: minmax(0, 1fr) auto;
    gap: var(--space-sm);
  }}
  .settings-navigation a small {{ text-align: right; }}
  .button-rail {{ display: block; }}
  .button-content {{ display: none; }}
}}
```

- [ ] **Step 7: Run the focused and existing web tests**

Run:

```bash
cargo nextest run --test web
```

Expected: all web tests pass, including current disclosure, security,
round-trip, error, and persistence contracts.

- [ ] **Step 8: Commit Task 1 with GitButler**

Run `but status` and confirm `src/web.rs` and `tests/web.rs` are the only
uncommitted files. Commit them to `co`:

```bash
but commit co -m "feat: add settings section navigation"
```

Confirm `but status` shows no uncommitted product-code change.

---

### Task 2: Spend width inside expanded sections

**Files:**
- Modify: `tests/web.rs:476-616`
- Modify: `src/web.rs:1419-1710`
- Modify: `src/web.rs:1764-1915`

**Interfaces:**
- Consumes: Task 1's `.control-layout`, `.control-rail`, `.settings-content`, `.settings-navigation`, stable IDs, and associated Apply actions.
- Produces: `.radar-basics-grid`, `.switch-grid`, `.footer-switch-grid`, and `.footer-format-grid` section-local layouts inside Task 1's sticky desktop rail.

- [ ] **Step 1: Write the failing responsive-layout contract test**

Add this test beside the Task 1 navigation test:

```rust
#[test]
fn settings_layout_uses_one_sticky_desktop_rail_and_section_local_grids() {
    let response = TestServer::new(optional_settings_enabled(), Vec::new()).get("/");

    for expected in [
        "class=\"radar-basics-grid\"",
        "class=\"switch-grid\"",
        "class=\"footer-switch-grid\"",
        "class=\"footer-format-grid\"",
        ".switch-grid, .footer-switch-grid {",
        "grid-template-columns: repeat(3, minmax(0, 1fr));",
    ] {
        assert!(
            response.body.contains(expected),
            "page omitted scalable layout contract {expected:?}"
        );
    }
    for forbidden in [
        "grid-area: location",
        "grid-area: manual",
        "grid-area: preferences",
        "\"location preferences\"",
        "\"manual preferences\"",
    ] {
        assert!(
            !response.body.contains(forbidden),
            "page retained coupled layout contract {forbidden:?}"
        );
    }
}
```

- [ ] **Step 2: Run the new contract and verify the expected failure**

Run:

```bash
cargo nextest run --test web settings_layout_uses_one_sticky_desktop_rail_and_section_local_grids
```

Expected: failure reports the missing section-local wrapper classes and grid
rules.

- [ ] **Step 3: Group existing controls without changing form semantics**

In `src/web.rs`:

- Wrap the existing Units, Range, Radar text size, and Show runways blocks in
  `<div class="radar-basics-grid">`.
- Inside Aircraft labels, keep the provider disclosure copy first and wrap the
  three hidden presence sentinels plus their corresponding switch labels in
  `<div class="switch-grid">`.
- Inside Footer, keep the Open-Meteo disclosure copy first. Wrap the five hidden
  presence sentinels plus their switch labels in
  `<div class="footer-switch-grid">`. Wrap the Temperature unit, Time zone,
  and Clock format fieldsets in `<div class="footer-format-grid">`.
- Leave Traffic filter's existing `.paired-fields` wrapper unchanged.

The hidden presence sentinel immediately precedes its current switch inside
the new wrapper. Input names, values, labels, help copy, checked fragments,
details summaries, and source order remain unchanged.

- [ ] **Step 4: Add section-local responsive grids**

Add these base styles near the current preferences and option-group styles:

```css
.radar-basics-grid, .switch-grid, .footer-switch-grid, .footer-format-grid {{
  display: grid;
  gap: var(--space-lg);
  min-width: 0;
}}

.radar-basics-grid {{ gap: var(--space-xl); }}

@media (min-width: 34rem) {{
  .switch-grid, .footer-switch-grid {{
    grid-template-columns: repeat(2, minmax(0, 1fr));
  }}
  .footer-format-grid {{
    grid-template-columns: repeat(2, minmax(0, 1fr));
  }}
  .footer-format-grid > :last-child {{ grid-column: 1 / -1; }}
}}
```

At the desktop breakpoint, change only internal section density:

```css
@media (min-width: 64rem) {{
  .radar-basics-grid {{ grid-template-columns: repeat(2, minmax(0, 1fr)); }}
  .radar-basics-grid > :nth-child(2) {{ grid-column: 1 / -1; }}
  .switch-grid {{ grid-template-columns: repeat(3, minmax(0, 1fr)); }}
  .footer-format-grid {{ grid-template-columns: repeat(3, minmax(0, 1fr)); }}
  .footer-format-grid > :last-child {{ grid-column: auto; }}
}}
```

Preserve the existing 34rem field rules, hover states, focus-visible rules, and
reduced-motion block. Confirm Task 1 removed the obsolete 52rem top-level
two-column rules instead of overriding them later.

- [ ] **Step 5: Keep switch targets readable inside the compact grids**

Change `.switch` only enough to allow its copy to shrink without clipping:

```css
.switch {{
  min-height: 44px;
  display: grid;
  grid-template-columns: auto minmax(0, 1fr);
  gap: var(--space-md);
  align-items: center;
  min-width: 0;
  cursor: pointer;
}}

.switch-copy {{ display: grid; gap: 0.125rem; min-width: 0; }}
.switch-copy strong, .switch-copy small {{ overflow-wrap: anywhere; }}
```

Do not reduce type sizes, the 44-pixel target, or control padding to make the
grid fit.

- [ ] **Step 6: Run formatting and the complete web suite**

Run:

```bash
cargo fmt --all --check
cargo nextest run --test web
```

Expected: formatting succeeds and every web test passes.

- [ ] **Step 7: Commit Task 2 with GitButler**

Run `but status` and confirm `src/web.rs` and `tests/web.rs` are the only
uncommitted files. Commit them to `co`:

```bash
but commit co -m "fix: scale fully expanded desktop settings"
```

Confirm the feature head advances and no product-code change remains
uncommitted.

---

### Task 3: Verify the exact feature head in automated and browser tests

**Files:**
- Modify only if a failing acceptance check requires a regression test and minimal fix: `tests/web.rs`, `src/web.rs`

**Interfaces:**
- Consumes: Task 2's committed feature head and the repository's `mise run verify` quality gate.
- Produces: passing full-suite evidence and viewport measurements for the fully expanded layout.

- [ ] **Step 1: Run the full repository quality gate**

Run:

```bash
mise run verify
```

Expected: rustfmt, strict all-target/all-feature Clippy, cargo-deny, and the
complete nextest workspace suite pass on the exact feature head.

- [ ] **Step 2: Confirm the GitButler source state**

Run:

```bash
but status
git status --short
git rev-parse refs/heads/codex/optional-radar-features
git rev-parse 'refs/heads/codex/optional-radar-features^{tree}'
```

Expected: no uncommitted product files; only ignored build output may exist.
Record the feature commit and tree for the physical-install evidence.

- [ ] **Step 3: Build the local prerelease from the clean GitButler workspace**

Confirm `git status --porcelain --untracked-files=all` is empty, then run:

```bash
mise run package-release -- 0.1.1-physical.20260802.2
```

Expected: `dist/release/` contains the exact validated release asset set and
the manifest source tree matches the committed GitButler workspace tree. This
is a local physical-test prerelease only; do not tag, push, draft, or publish a
release.

- [ ] **Step 4: Record settings and service state before mutation**

Run:

```bash
ssh user@radar.local 'sudo sha256sum /var/lib/planeradar/settings.json; systemctl show planeradar --property=ActiveState,SubState,NRestarts --no-pager'
mise run status -- user@radar.local
```

Expected: settings hash is recorded, the service is active and running, and
the currently accepted release is reported.

- [ ] **Step 5: Install through the supported application-only upgrade path**

Run:

```bash
mise run upgrade -- user@radar.local --release-dir /Users/shayne/code/RPi-Plane-Radar/dist/release
```

Expected: the controller accepts the local manifest, recognizes the unchanged
driver, atomically installs the application payload, and does not require a
reboot.

- [ ] **Step 6: Verify service, settings preservation, health, and smoke evidence**

Run:

```bash
mise run status -- user@radar.local
mise run doctor -- user@radar.local
mise run smoke-pi -- user@radar.local
ssh user@radar.local 'sudo sha256sum /var/lib/planeradar/settings.json; systemctl show planeradar --property=ActiveState,SubState,NRestarts --no-pager; journalctl -u planeradar -n 80 --no-pager'
curl --fail --silent --show-error http://planeradar.local/healthz
```

Expected: status and doctor pass, smoke verification accepts the release and
fresh physical screenshot, the settings hash is byte-identical to Step 4, the
service is active/running with zero restarts, recent logs contain no new
warning or error, and mDNS health returns success.

- [ ] **Step 7: Inspect the fully expanded live page at desktop widths**

Use the `agent-browser` skill. Resolve the current Pi address without assuming
the prior DHCP lease:

```bash
ssh user@radar.local 'hostname -I'
```

Save the first address in a task-specific shell variable and open it in Chrome:

```bash
planeradar_test_ip="$(ssh user@radar.local 'hostname -I' | awk '{print $1}')"
printf '%s\n' "http://${planeradar_test_ip}/"
```

At both 1440 by 900 and 1024 by 768, set every native `details` element open
for the visual stress test and
evaluate this measurement object:

```javascript
const locationBox = document.querySelector('#location').getBoundingClientRect();
const manualBox = document.querySelector('.manual').getBoundingClientRect();
const railBox = document.querySelector('.control-rail').getBoundingClientRect();
({
  documentOverflow: document.documentElement.scrollWidth - window.innerWidth,
  locationToManualGap: manualBox.top - locationBox.bottom,
  railTop: railBox.top,
  railHeight: railBox.height,
  viewportWidth: window.innerWidth,
  viewportHeight: window.innerHeight,
});
```

Expected before scrolling: `documentOverflow` is `0`,
`locationToManualGap` is between `0` and `48`, and the rail fits or scrolls
within the viewport rather than covering content. Scroll the page at least
1,000 pixels and measure `.control-rail` again. Its top remains at the declared
sticky offset while Location, Manual coordinates, Radar basics, Aircraft
labels, Footer, and Traffic filter remain one uninterrupted vertical sequence.

- [ ] **Step 8: Inspect tablet and mobile behavior**

At 768 by 1024 and 390 by 844, verify both the saved initial disclosure state
and a stress state with all `details` open.

Expected:

- the identity and status appear above the content;
- the settings links form a horizontally scrollable 44-pixel section strip;
- the rail Apply action is hidden and absent from keyboard focus;
- the content-bottom Apply action is visible;
- section-local grids stack without clipped labels or controls;
- `document.documentElement.scrollWidth - window.innerWidth` is `0`; and
- Tab traversal follows visible document order with a visible focus ring.

- [ ] **Step 9: Perform the critique-and-fix loop if any acceptance check fails**

For each failed measurement, first add a focused assertion to
`settings_layout_uses_one_sticky_desktop_rail_and_section_local_grids`, run it
to observe failure, make the smallest CSS or markup correction in `src/web.rs`,
rerun `cargo nextest run --test web`, then repeat the affected browser
measurement. Commit any correction to `co` with GitButler as:

```bash
but commit co -m "fix: refine responsive settings layout"
```

Do not accept a visual correction that forces configured sections closed,
reduces touch targets, changes form fields, or adds JavaScript.

- [ ] **Step 10: Re-run the final exact-head gate**

After the last browser correction, run:

```bash
mise run verify
but status
git status --short
```

Expected: all quality gates pass and no required product work remains
uncommitted. If Task 3 changed code after the first physical install, package
`0.1.1-physical.20260802.3`, repeat Steps 4 through 8, and report only evidence
from the final installed head.

---

## Completion evidence

Report:

- final `codex/optional-radar-features` commit and tree;
- `mise run verify` result and nextest count;
- the installed local prerelease version, source revision, and binary hash;
- before-and-after settings hashes;
- status, doctor, health, service restart, and log results;
- browser measurements at all four acceptance viewports;
- the physical radar screenshot path; and
- explicit confirmation that no branch was pushed, tag created, or release cut.

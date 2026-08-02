# Plane Radar Web Settings Redesign Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace the bare local settings form with a production-ready, responsive instrument-console interface that is obvious during first setup and efficient for repeat changes.

**Architecture:** Keep the page server-rendered inside `src/web.rs`, preserve the current session and mutation boundaries, and split presentation into private helpers for page state, status, search results, and controls. Reuse `src/range.rs` as the only authority for visible range labels, and exercise the complete HTTP surface through `tests/web.rs` before browser inspection.

**Tech Stack:** Rust 2024, `tiny_http`, semantic HTML, inline CSS, existing integration-test TCP client, local headless Plane Radar runtime, browser automation.

## Global Constraints

- No JavaScript framework, external font, icon set, image request, CSS asset, or new runtime dependency.
- Preserve HttpOnly SameSite sessions, CSRF checks, allowed-host checks, mutation provenance checks, the 16 KiB form limit, bounded workers, escaping, and no-store HTML.
- Use `range_preset` and `format_range_label` for all visible range values.
- Target WCAG AA, visible keyboard focus, semantic labels and groups, 44-pixel minimum touch targets, color-independent status cues, and reduced-motion support.
- Unknown paths and query strings remain 404 except the exact success route `/?saved=1`.
- Do not echo search text, provider errors, storage errors, file paths, cookies, CSRF values, or other sensitive data.

---

### Task 1: Semantic page states and human-readable controls

**Files:**
- Modify: `src/web.rs:610`
- Test: `tests/web.rs:422`

**Interfaces:**
- Consumes: `crate::range::{format_range_label, range_preset}` and existing `RadarSettings`, `Units`, and `GeocodeResult` values.
- Produces: private `SearchState<'a>`, `PageNotice`, `render_page`, `render_status`, `render_search_results`, and `render_range_options` helpers used by the route handlers and Task 2.

- [ ] **Step 1: Write failing semantic and range-label tests**

Add focused HTTP assertions covering setup-required and configured status,
semantic groups, manual fallback, selected controls, and real range labels:

```rust
#[test]
fn unconfigured_page_prioritizes_setup_and_exposes_semantic_controls() {
    let server = TestServer::new(RadarSettings::default(), Vec::new());
    let response = server.get("/");

    assert_eq!(response.status, 200);
    for expected in [
        "Local control",
        "Setup required",
        "Choose the radar's home location",
        "<main",
        "<fieldset",
        "<legend>Units</legend>",
        "<legend>Range</legend>",
        "<details",
        "Manual coordinates",
        "Apply settings",
    ] {
        assert!(response.body.contains(expected), "missing {expected:?}");
    }
}

#[test]
fn range_choices_use_display_values_in_the_selected_units() {
    let kilometres = TestServer::new(RadarSettings::default(), Vec::new()).get("/");
    for label in ["5 km", "10 km", "15 km", "25 km"] {
        assert!(kilometres.body.contains(label), "missing {label:?}");
    }

    let miles = TestServer::new(configured_settings(), Vec::new()).get("/");
    for label in ["3 mi", "6 mi", "9 mi", "16 mi"] {
        assert!(miles.body.contains(label), "missing {label:?}");
    }
    assert!(miles.body.contains("Radar configured"));
    assert!(miles.body.contains("Old location"));
}
```

- [ ] **Step 2: Run the new tests and verify they fail**

Run:

```bash
cargo test --locked --test web unconfigured_page_prioritizes_setup_and_exposes_semantic_controls
cargo test --locked --test web range_choices_use_display_values_in_the_selected_units
```

Expected: both tests fail because the old page has no status treatment, semantic fieldsets, or visible range values.

- [ ] **Step 3: Introduce explicit render state and focused helpers**

Replace the `results` plus free-form message arguments with typed render state:

```rust
enum SearchState<'a> {
    Idle,
    Results(&'a [GeocodeResult]),
    Empty,
    Unavailable,
}

#[derive(Clone, Copy)]
enum PageNotice {
    Saved,
    InvalidSettings,
    SaveFailed,
}

fn render_page(
    settings: &RadarSettings,
    local_url: &str,
    csrf_token: &str,
    search: SearchState<'_>,
    notice: Option<PageNotice>,
) -> String
```

Add `render_status`, `render_notice`, `render_search_results`, and
`render_range_options`. Generate each range label from the existing range
module:

```rust
fn render_range_options(settings: &RadarSettings) -> String {
    (0_u8..=3)
        .map(|index| {
            let label = range_preset(index)
                .map(|preset| format_range_label(preset, settings.units))
                .expect("the web form only renders supported range indices");
            let checked = (settings.range_index == index).then_some(" checked").unwrap_or("");
            format!(
                r#"<label class="segment"><input type="radio" name="range_index" value="{index}"{checked}><span>{label}</span></label>"#
            )
        })
        .collect()
}
```

Render a status glyph plus text, place search and results first, put manual
coordinates in `<details>`, and use `<fieldset>` plus `<legend>` for units and
range. Keep each search result as its own settings form with the existing
hidden preservation fields.

- [ ] **Step 4: Run the focused web tests**

Run:

```bash
cargo test --locked --test web unconfigured_page_prioritizes_setup_and_exposes_semantic_controls
cargo test --locked --test web range_choices_use_display_values_in_the_selected_units
cargo test --locked --test web page_exposes_local_settings_without_wifi_or_browser_geolocation
cargo test --locked --test web search_results_are_selectable_escaped_and_never_persist
```

Expected: all four pass.

- [ ] **Step 5: Commit the semantic rendering change**

```bash
git add src/web.rs tests/web.rs
git commit -m "feat: redesign radar settings structure" -m "Co-authored-by: Codex <noreply@openai.com>"
```

---

### Task 2: Inline search, validation, failure, and success feedback

**Files:**
- Modify: `src/web.rs:128-255`
- Modify: `tests/web.rs:16-170`
- Test: `tests/web.rs:844-1008`

**Interfaces:**
- Consumes: `SearchState<'a>`, `PageNotice`, and `render_page` from Task 1.
- Produces: exact GET routes `/` and `/?saved=1`, HTML error responses for settings failures, and a fixed success redirect for all accepted settings mutations.

- [ ] **Step 1: Write failing state-feedback tests**

Add tests with these exact contracts:

```rust
#[test]
fn empty_search_has_a_distinct_manual_fallback_state() {
    let server = TestServer::new(configured_settings(), Vec::new());
    let session = server.session();
    let response = server.post_form(
        "/search",
        &[("query", "no match")],
        &session,
        Some(&server.current_ip_origin()),
        None,
    );
    assert_eq!(response.status, 200);
    assert!(response.body.contains("No matching places found"));
    assert!(!response.body.contains("Search unavailable"));
}

#[test]
fn invalid_settings_return_the_page_with_safe_guidance() {
    let server = TestServer::new(configured_settings(), Vec::new());
    let session = server.session();
    let response = server.post_form(
        "/settings",
        &[("latitude", "91"), ("longitude", "-74.0")],
        &session,
        Some(&server.current_ip_origin()),
        None,
    );
    assert_eq!(response.status, 400);
    assert!(response.body.contains("Those settings could not be applied"));
    assert!(response.body.contains("Old location"));
}

#[test]
fn successful_settings_redirect_to_a_fixed_confirmation_page() {
    let server = TestServer::new(RadarSettings::default(), Vec::new());
    let session = server.session();
    let response = server.post_form(
        "/settings",
        &[("latitude", "40.7"), ("longitude", "-74.0")],
        &session,
        Some(&server.current_ip_origin()),
        None,
    );
    assert_eq!(response.status, 303);
    assert_eq!(response.header("location"), Some("/?saved=1"));
    let confirmed = server.get("/?saved=1");
    assert_eq!(confirmed.status, 200);
    assert!(confirmed.body.contains("Radar settings applied"));
    assert!(confirmed.body.contains("role=\"status\""));
}
```

Extend the test settings double with a `fail_replacements: bool` field and a
constructor used by `TestServer::with_failing_settings`. Assert that a failed
replacement returns HTTP 500, shows `Plane Radar could not save those
settings`, retains the old location, and omits internal error text.

- [ ] **Step 2: Run the feedback tests and verify they fail**

Run:

```bash
cargo test --locked --test web empty_search_has_a_distinct_manual_fallback_state
cargo test --locked --test web invalid_settings_return_the_page_with_safe_guidance
cargo test --locked --test web successful_settings_redirect_to_a_fixed_confirmation_page
cargo test --locked --test web failed_settings_write_returns_safe_page
```

Expected: the old implementation omits the empty and validation states,
redirects to `/`, and returns plain text for storage failure.

- [ ] **Step 3: Route typed feedback through the server-rendered page**

Change the route enum and exact route matching:

```rust
enum Route {
    Page { saved: bool },
    Health,
    Search,
    Settings,
}

let route = match (request.method(), request.url()) {
    (&Method::Get, "/") => Route::Page { saved: false },
    (&Method::Get, "/?saved=1") => Route::Page { saved: true },
    (&Method::Get, "/healthz") => Route::Health,
    (&Method::Post, "/search") => Route::Search,
    (&Method::Post, "/settings") => Route::Settings,
    _ => return Ok(Outgoing::text(404, "Not found")),
};
```

Render `SearchState::Empty` for an empty successful provider response and
`SearchState::Unavailable` for provider failure. Pass the submitted CSRF token
into settings rendering on invalid or failed saves. Return HTML with status 400
or 500 and `Cache-Control: no-store`. Redirect accepted settings to
`/?saved=1`, which renders `PageNotice::Saved` without reflecting user input.

- [ ] **Step 4: Run all web integration tests**

Run:

```bash
cargo test --locked --test web
```

Expected: every web integration test passes, including the original security,
privacy, body-size, concurrency, and route-allowlist cases.

- [ ] **Step 5: Commit the feedback behavior**

```bash
git add src/web.rs tests/web.rs
git commit -m "feat: add web settings feedback" -m "Co-authored-by: Codex <noreply@openai.com>"
```

---

### Task 3: Responsive visual system and browser critique loop

**Files:**
- Modify: `src/web.rs:render_page`
- Test: `tests/web.rs:page_exposes_local_settings_without_wifi_or_browser_geolocation`

**Interfaces:**
- Consumes: the semantic HTML and page states from Tasks 1 and 2.
- Produces: inline OKLCH design tokens, responsive console layout, complete control-state styling, a low-contrast CSS radar motif, and browser evidence at all required viewports.

- [ ] **Step 1: Write failing CSS-contract assertions**

Replace the old minimal CSS assertions with durable requirements:

```rust
for expected in [
    "--surface: oklch(",
    "min-height: 44px",
    ":focus-visible",
    "@media (min-width: 52rem)",
    "grid-template-areas:",
    "prefers-reduced-motion: reduce",
] {
    assert!(response.body.contains(expected), "page omitted CSS contract {expected:?}");
}
for forbidden in ["<script", "https://fonts.", "backdrop-filter", "background-clip: text"] {
    assert!(!response.body.contains(forbidden), "page included {forbidden:?}");
}
```

- [ ] **Step 2: Run the CSS contract test and verify it fails**

Run:

```bash
cargo test --locked --test web page_exposes_local_settings_without_wifi_or_browser_geolocation
```

Expected: failure on the new OKLCH, focus, responsive-grid, and reduced-motion contracts.

- [ ] **Step 3: Implement the responsive instrument-console CSS**

Add inline tokens for graphite surfaces, warm off-white text, muted blue-gray,
radar green, amber, borders, focus, and error states. Implement:

- a full-page tinted graphite base;
- a bounded shell with a compact masthead and CSS radar-ring motif;
- named grid areas `location`, `manual`, and `preferences`;
- a one-column mobile flow and asymmetric desktop grid above 52rem;
- segmented native radio controls with visible checked and focus states;
- a native checkbox switch with adjacent explanatory copy;
- wrapping search-result rows and full-width mobile actions;
- 44-pixel minimum controls;
- tabular numeric values;
- 150 to 200ms opacity, color, border-color, and transform transitions; and
- a reduced-motion override that removes transitions.

Do not add side-stripe accents, gradient text, glass blur, nested card grids,
custom scrollbars, or decorative page-load animation.

- [ ] **Step 4: Run automated formatting and verification**

Run:

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --locked --workspace --all-features
cargo deny check
```

Expected: all commands exit 0.

- [ ] **Step 5: Launch the real local headless settings server**

Create a temporary directory with `mktemp -d`, choose a free loopback port,
and launch:

```bash
cargo run --locked -- run --headless \
  --settings "$RADAR_UI_TMP/settings.json" \
  --geocode-cache "$RADAR_UI_TMP/geocode-cache.json" \
  --http "127.0.0.1:$RADAR_UI_PORT" \
  --local-url "http://127.0.0.1:$RADAR_UI_PORT"
```

Use the browser against that exact loopback URL. The settings path begins
absent to exercise first-run. Submit a valid configuration through the browser
to exercise configured and success states.

- [ ] **Step 6: Inspect and capture required viewports**

Inspect the first-run and configured page at:

- 375×812 mobile;
- 768×1024 tablet; and
- 1440×900 desktop.

Verify no overlap, clipping, horizontal scroll, cramped targets, weak focus,
awkward whitespace, or long-label overflow. Exercise keyboard focus, manual
details, unit and range groups, runway switch, successful save, and a search
failure. Save screenshots outside tracked source paths.

- [ ] **Step 7: Perform one critique-and-fix pass**

Compare the screenshots with the approved design spec. Record the concrete
defects, patch `src/web.rs` and its tests, rerun the affected automated tests,
and recapture every viewport affected by the patch. Exit only when the page is
intentional in setup-required, configured, success, and error states.

- [ ] **Step 8: Commit the final visual implementation**

```bash
git add src/web.rs tests/web.rs
git commit -m "feat: polish responsive radar settings" -m "Co-authored-by: Codex <noreply@openai.com>"
```

---

### Task 4: Final repository verification

**Files:**
- Verify: `src/web.rs`
- Verify: `tests/web.rs`
- Verify: `PRODUCT.md`
- Verify: `docs/superpowers/specs/2026-08-01-web-settings-redesign-design.md`
- Verify: `docs/superpowers/plans/2026-08-01-web-settings-redesign.md`

**Interfaces:**
- Consumes: the completed responsive settings UI and all repository contracts.
- Produces: clean verification evidence and an exact final workspace status.

- [ ] **Step 1: Run the repository verification commands**

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --locked --workspace --all-features
cargo deny check
cargo test --locked --test docs_contract
git diff --check HEAD~3..HEAD
```

Expected: all commands exit 0 and `git diff --check` prints nothing.

- [ ] **Step 2: Confirm final repository state**

```bash
git status --short --branch
git log -4 --oneline --decorate
```

Expected: the branch contains the design, structure, feedback, and polish
commits with no uncommitted files.

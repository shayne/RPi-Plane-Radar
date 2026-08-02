---
name: Plane Radar
description: A calm local control surface for a dedicated home aircraft radar.
colors:
  canvas: "oklch(14% 0.012 155)"
  surface: "oklch(18% 0.014 155)"
  surface-raised: "oklch(22% 0.016 155)"
  surface-active: "oklch(28% 0.025 155)"
  text: "oklch(94% 0.012 100)"
  text-muted: "oklch(72% 0.022 165)"
  text-faint: "oklch(60% 0.018 165)"
  border: "oklch(35% 0.022 155)"
  border-strong: "oklch(48% 0.04 150)"
  accent: "oklch(78% 0.14 145)"
  accent-hover: "oklch(84% 0.15 145)"
  accent-ink: "oklch(18% 0.035 145)"
  warning: "oklch(80% 0.13 80)"
  warning-surface: "oklch(23% 0.035 80)"
  danger: "oklch(74% 0.17 28)"
  danger-surface: "oklch(22% 0.045 28)"
  success-surface: "oklch(23% 0.04 145)"
  radar-line: "oklch(31% 0.045 150)"
  focus: "oklch(88% 0.14 145)"
typography:
  display:
    fontFamily: '-apple-system, BlinkMacSystemFont, "Segoe UI", system-ui, sans-serif'
    fontSize: "2rem"
    fontWeight: 750
    lineHeight: 1.05
    letterSpacing: "-0.035em"
  headline:
    fontFamily: '-apple-system, BlinkMacSystemFont, "Segoe UI", system-ui, sans-serif'
    fontSize: "1.25rem"
    fontWeight: 750
    lineHeight: 1.2
    letterSpacing: "-0.015em"
  title:
    fontFamily: '-apple-system, BlinkMacSystemFont, "Segoe UI", system-ui, sans-serif'
    fontSize: "1rem"
    fontWeight: 700
    lineHeight: 1.25
  body:
    fontFamily: '-apple-system, BlinkMacSystemFont, "Segoe UI", system-ui, sans-serif'
    fontSize: "1rem"
    fontWeight: 400
    lineHeight: 1.55
    letterSpacing: "0.01em"
  label:
    fontFamily: '-apple-system, BlinkMacSystemFont, "Segoe UI", system-ui, sans-serif'
    fontSize: "0.875rem"
    fontWeight: 700
    lineHeight: 1.2
  eyebrow:
    fontFamily: '-apple-system, BlinkMacSystemFont, "Segoe UI", system-ui, sans-serif'
    fontSize: "0.75rem"
    fontWeight: 800
    lineHeight: 1.2
    letterSpacing: "0.12em"
rounded:
  sm: "0.5rem"
  md: "0.75rem"
spacing:
  xs: "0.25rem"
  sm: "0.5rem"
  md: "0.75rem"
  lg: "1.5rem"
  xl: "2rem"
  2xl: "3rem"
components:
  button-primary:
    backgroundColor: "{colors.accent}"
    textColor: "{colors.accent-ink}"
    typography: "{typography.label}"
    rounded: "{rounded.sm}"
    padding: "0.75rem 1rem"
    height: "44px"
  button-primary-hover:
    backgroundColor: "{colors.accent-hover}"
    textColor: "{colors.accent-ink}"
    rounded: "{rounded.sm}"
  button-secondary:
    backgroundColor: "{colors.surface-raised}"
    textColor: "{colors.text}"
    typography: "{typography.label}"
    rounded: "{rounded.sm}"
    padding: "0.75rem 1rem"
    height: "44px"
  input:
    backgroundColor: "{colors.surface}"
    textColor: "{colors.text}"
    typography: "{typography.body}"
    rounded: "{rounded.sm}"
    padding: "0.65rem 0.75rem"
    height: "44px"
  segmented-selected:
    backgroundColor: "{colors.accent}"
    textColor: "{colors.accent-ink}"
    typography: "{typography.label}"
    height: "44px"
---

# Design System: Plane Radar

## Overview

**Creative North Star: "The Quiet Radar Console"**

Plane Radar borrows the clarity of a civilian aviation instrument, then softens it for a dedicated appliance at home. The surface is dark because it belongs beside an always-on radar display, but the restrained green guidance color and generous touch targets keep configuration approachable rather than theatrical.

Information is arranged as a calm sequence of structural rules, native controls, and progressive disclosure. Identity appears in the radar mark and compact masthead; everything below serves location and display configuration. The system explicitly rejects generic SaaS dashboards, military command software, neon hacker tooling, and novelty retro terminals.

**Key Characteristics:**

- Calm, precise, and instrument-like.
- Dark tonal layers with one rare guidance-green accent.
- Flat structural separation instead of decorative cards.
- Familiar native controls with strong focus and touch affordances.
- Mobile-first flow that becomes a two-column console at desktop widths.

## Colors

Near-black green neutrals create the console, while a clear guidance green marks actions, selections, readiness, and focus.

### Primary

- **Guidance Green** (`accent`): Primary actions, selected values, ready state, and the radar center point.
- **Bright Guidance Green** (`accent-hover`): Hover feedback for the primary action and links only.
- **Deep Green Ink** (`accent-ink`): Legible text and switch knobs placed on Guidance Green.

### Secondary

- **Amber Advisory** (`warning`): Setup-required status and other non-destructive attention states.
- **Coral Alert** (`danger`): Validation and failure messages that must be noticed without dominating the page.

### Neutral

- **Flight Deck Black-Green** (`canvas`): The page background and deepest neutral.
- **Radar Console Surface** (`surface`): Input fields and unselected segmented controls.
- **Raised Instrument Surface** (`surface-raised`): Secondary buttons and inactive switch tracks.
- **Active Instrument Surface** (`surface-active`): Hovered secondary controls.
- **Warm Chart White** (`text`): Primary text and headings.
- **Muted Sage Gray** (`text-muted`): Supporting copy and inactive labels.
- **Faint Sage Gray** (`text-faint`): Attribution, metadata, and low-priority hints.
- **Quiet Radar Rule** (`border`): Structural dividers and grouped-control seams.
- **Instrument Outline** (`border-strong`): Interactive control outlines.
- **Deep Amber Surface** (`warning-surface`): Advisory message background.
- **Deep Alert Surface** (`danger-surface`): Error message background.
- **Confirmed Green Surface** (`success-surface`): Success message background.
- **Radar Grid Green** (`radar-line`): The masthead radar-mark geometry.
- **High-Visibility Focus Green** (`focus`): The 3px keyboard-focus outline.

**The One Guidance Color Rule.** Guidance Green marks an action, selection, readiness, or focus state. It is not decoration.

## Typography

**Display Font:** system sans (with `-apple-system`, BlinkMacSystemFont, `Segoe UI`, `system-ui`)
**Body Font:** system sans (with `-apple-system`, BlinkMacSystemFont, `Segoe UI`, `system-ui`)
**Label Font:** system sans (with `-apple-system`, BlinkMacSystemFont, `Segoe UI`, `system-ui`)

**Character:** One native sans-serif stack keeps the local appliance fast and familiar. Weight, compact fixed sizes, and deliberate letter spacing create hierarchy without display-font ornament.

### Hierarchy

- **Display** (750, 2rem, 1.05): Product name in the masthead, with tight negative tracking.
- **Headline** (750, 1.25rem, 1.2): Major configuration regions such as Radar location and Radar display.
- **Title** (700, 1rem, 1.25): Local subsections and compact status titles.
- **Body** (400, 1rem, 1.55): Instructions and general copy, capped at 68ch.
- **Label** (700, 0.875rem): Controls, buttons, and field names.
- **Eyebrow** (800, 0.75rem, 0.12em tracking): Sparse uppercase appliance metadata.

**The Native Instrument Rule.** Use the system stack everywhere; hierarchy comes from weight, size, and spacing, never a decorative display face.

## Elevation

The system is flat by default. Depth comes from small tonal shifts between canvas, controls, active states, and message surfaces, plus 1px structural borders. There are no ambient card shadows; the only inset shadow is the status dot's canvas-colored inner ring, which preserves its instrument shape.

**The Structural Depth Rule.** Separate regions with tone, spacing, or a 1px rule. Do not float configuration in decorative cards.

## Components

Components feel restrained and tactile: familiar forms, exact states, and no decorative chrome.

### Buttons

- **Shape:** Compact rounded rectangle (`0.5rem`) with a 1px outline and at least 44px height.
- **Primary:** Guidance Green with Deep Green Ink, bold label text, and `0.75rem 1rem` padding.
- **Secondary:** Raised Instrument Surface with Warm Chart White and Instrument Outline.
- **Hover / Focus:** Hover changes tone or outline; keyboard focus uses a 3px High-Visibility Focus Green outline offset by 3px.
- **Active / Disabled:** Active presses down by 1px; disabled controls retain shape at 50% opacity.

### Cards / Containers

- **Corner Style:** Containers stay square and structural; rounded corners belong to controls and notices.
- **Background:** Most regions share the Flight Deck Black-Green canvas.
- **Shadow Strategy:** No ambient shadows.
- **Border:** Use Quiet Radar Rule at 1px for section boundaries and disclosure separators.
- **Internal Padding:** Follow the extracted spacing scale, varying from `0.75rem` for controls to `3rem` between desktop regions.

### Inputs / Fields

- **Style:** Radar Console Surface, Instrument Outline, `0.5rem` radius, tabular numerals where values are numeric, and at least 44px height.
- **Focus:** 3px High-Visibility Focus Green outline with a 3px offset.
- **Error / Disabled:** Errors are reported in a bordered Coral Alert notice; invalid data never replaces stored settings.

### Segmented Controls

- **Style:** Adjacent 44px options share one rounded 1px border with 1px seams.
- **State:** Unselected options use Radar Console Surface and Muted Sage Gray; the selected option uses Guidance Green and Deep Green Ink.

### Switches

- **Style:** A 3rem by 1.75rem pill track sits beside a plain-language label and optional supporting copy.
- **State:** Inactive uses Raised Instrument Surface; active uses Guidance Green with a Deep Green Ink knob. The complete label remains a 44px touch target.

### Disclosures

- **Style:** Native `details` and `summary` rows separated by 1px Quiet Radar Rules, without nested cards.
- **State:** Muted at rest, Warm Chart White while open, Guidance Green on hover, and the shared focus outline for keyboard use.
- **Behavior:** Optional groups start collapsed and open when they contain non-default values or own a validation error.

### Notices and Status

- **Notices:** A full 1px semantic outline, matching tinted surface, compact leading symbol, and `0.5rem` radius communicate success or error without color alone.
- **Status:** A ringed dot plus strong title and muted detail conveys setup or ready state in one compact row.

### Navigation

- **Style:** The device URL is the only utility navigation. It stays a standard underlined Guidance Green link with visible focus, wraps safely, and moves from the masthead's own row to right alignment on wide screens.

## Do's and Don'ts

### Do:

- **Do** keep primary touch targets at least 44px high and keyboard focus visible with the 3px `focus` outline.
- **Do** use native form semantics, labels, fieldsets, and `details` disclosure before inventing a custom interaction.
- **Do** use Guidance Green only for actions, selections, readiness, and focus.
- **Do** preserve the single-column phone flow, paired fields above 34rem, and the location/preferences split above 52rem.
- **Do** use spacing, tonal surfaces, and 1px rules to make hierarchy obvious.
- **Do** keep prose to 68ch or less and wrap provider/privacy explanations naturally.

### Don't:

- **Don't** make the interface resemble a generic SaaS dashboard.
- **Don't** make it feel like military command software.
- **Don't** use neon hacker tooling or novelty retro-terminal styling.
- **Don't** introduce decorative cards, nested cards, or identical card grids.
- **Don't** add excessive instrumentation or visual effects that compete with configuration.
- **Don't** use colored side-stripe borders, gradient text, glassmorphism, or decorative motion.
- **Don't** use full-saturation accents on inactive states or introduce a second decorative accent color.
- **Don't** replace familiar native controls with invented affordances.

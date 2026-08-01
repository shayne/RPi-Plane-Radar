# README Tone and Live GIF Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make both public READMEs concise and factual, and replace the Plane Radar hero screenshot with a native-resolution GIF made from one minute of live device captures played at four times normal speed.

**Architecture:** Keep the existing README structure and operational contracts while replacing conversational copy in place. Capture 60 logical framebuffer PNGs from the installed Plane Radar service over one SSH session, encode them into a 15-second looping GIF with a generated palette, and add a small Rust documentation contract for the public media reference and GIF container properties. The static accepted PNG remains unchanged as a stable fixture.

**Tech Stack:** Markdown, Rust documentation-contract tests, Bash, OpenSSH, FFmpeg 8.1.2 through mise, GitButler

## Global Constraints

- Preserve all commands, URLs, setup order, warnings, support boundaries, AI disclosure, project credit, and license facts.
- Use a direct README register: compact, factual, and command-forward, without jokes or rhetorical asides.
- Keep the supported application target exactly Raspberry Pi Zero 2 W, Pimoroni HyperPixel 2.1 Round, and 64-bit Raspberry Pi OS Lite Trixie.
- Capture exactly 60 valid 480 by 480 PNG frames across approximately one real minute.
- Encode at four frames per second into an approximately 15-second infinitely looping 480 by 480 GIF with no spatial scaling.
- Replace the README hero reference with the GIF while retaining `docs/images/planeradar-radar.png` unchanged.
- Do not change the installed application, display driver, release artifacts, target configuration, or `release/current-release.txt`.
- Keep every authored commit co-authored exactly once with `Co-authored-by: Codex <noreply@openai.com>`.
- Use `apply_patch` for text edits, mise for the FFmpeg execution environment, and GitButler for every version-control write.

---

### Task 1: Capture the GIF and update the Plane Radar README

**Files:**
- Create: `docs/images/planeradar-radar.gif`
- Modify: `README.md:1-201`
- Modify: `tests/docs_contract.rs:6-8,119-151,388-406`
- Create temporarily: `target/readme-capture.*/frame-00.png` through `frame-59.png`
- Create temporarily: `target/readme-capture.*/timestamps.tsv`
- Test: `tests/docs_contract.rs`

**Interfaces:**
- Consumes: ignored `.env` key `PLANERADAR_PI_TARGET`, the running `planeradar.service`, `/var/lib/planeradar/debug.png`, the existing SIGUSR1 screenshot path, and the static PNG documentation contract
- Produces: a native 480 by 480, 60-frame, 15-second looping GIF, a compact app README, and passing animated-media documentation contracts

- [ ] **Step 1: Confirm the target is healthy without exposing its private address**

Load the ignored maintainer environment and run the public controller diagnostic:

```sh
set -a
source .env
set +a
mise run doctor -- "$PLANERADAR_PI_TARGET"
```

Expected: `Plane Radar doctor: healthy`.

- [ ] **Step 2: Capture 60 fresh logical frames over one SSH session**

Run from the application repository:

```sh
set -euo pipefail
set -a
source .env
set +a
capture_dir="$(mktemp -d target/readme-capture.XXXXXX)"
export capture_dir

ssh "$PLANERADAR_PI_TARGET" 'bash -s' <<'REMOTE' | tar -xf - -C "$capture_dir"
set -euo pipefail
work="$(mktemp -d /var/tmp/planeradar-readme.XXXXXX)"
case "$work" in
  /var/tmp/planeradar-readme.*) ;;
  *) exit 1 ;;
esac
trap 'rm -rf -- "$work"' EXIT

for frame in $(seq -w 0 59); do
  before="$(sudo -n stat -c '%Y:%s:%i' /var/lib/planeradar/debug.png 2>/dev/null || true)"
  sudo -n systemctl kill --signal=SIGUSR1 planeradar.service
  fresh=false
  for _attempt in $(seq 1 200); do
    after="$(sudo -n stat -c '%Y:%s:%i' /var/lib/planeradar/debug.png 2>/dev/null || true)"
    if test -n "$after" && test "$after" != "$before"; then
      fresh=true
      break
    fi
    sleep 0.05
  done
  test "$fresh" = true
  sudo -n cat /var/lib/planeradar/debug.png >"$work/frame-$frame.png"
  printf '%s\t%s\n' "$frame" "$(date +%s.%N)" >>"$work/timestamps.tsv"
  if test "$frame" != 59; then
    sleep 1
  fi
done

tar -C "$work" -cf - .
REMOTE
```

Expected: the temporary local directory contains `frame-00.png` through `frame-59.png` and `timestamps.tsv`. The remote temporary directory is removed by its validated trap.

- [ ] **Step 3: Validate the source frame count, dimensions, and elapsed time**

Run:

```sh
test "$(find "$capture_dir" -type f -name 'frame-*.png' | wc -l | tr -d ' ')" = 60
mise x ffmpeg@8.1.2 -- ffprobe -v error \
  -select_streams v:0 \
  -show_entries stream=width,height \
  -of csv=p=0 "$capture_dir/frame-00.png" | grep -qx '480,480'
mise x ffmpeg@8.1.2 -- ffprobe -v error \
  -select_streams v:0 \
  -show_entries stream=width,height \
  -of csv=p=0 "$capture_dir/frame-59.png" | grep -qx '480,480'
awk 'NR == 1 { first=$2 } { last=$2 } END { elapsed=last-first; exit !(elapsed >= 59 && elapsed <= 75) }' \
  "$capture_dir/timestamps.tsv"
```

Expected: all commands exit zero; the first and last captures are 480 by 480 and span 59 to 75 seconds.

- [ ] **Step 4: Encode the GIF at four times normal speed**

Run:

```sh
mise x ffmpeg@8.1.2 -- ffmpeg -y \
  -framerate 4 \
  -i "$capture_dir/frame-%02d.png" \
  -filter_complex \
  '[0:v]split[original][palette_source];[palette_source]palettegen=max_colors=256:stats_mode=diff[palette];[original][palette]paletteuse=dither=bayer:bayer_scale=3:diff_mode=rectangle' \
  -loop 0 \
  docs/images/planeradar-radar.gif
```

Expected: FFmpeg reads 60 PNG frames and writes `docs/images/planeradar-radar.gif` without resizing.

- [ ] **Step 5: Verify the encoded media**

Run:

```sh
mise x ffmpeg@8.1.2 -- ffprobe -v error \
  -select_streams v:0 \
  -count_frames \
  -show_entries stream=width,height,nb_read_frames:format=duration \
  -of default=noprint_wrappers=1 \
  docs/images/planeradar-radar.gif
```

Expected: `width=480`, `height=480`, `nb_read_frames=60`, and `duration=15.000000` or an equivalent 15-second duration reported by the installed FFprobe build.

- [ ] **Step 6: Generate a temporary contact sheet for visual review**

Run:

```sh
mise x ffmpeg@8.1.2 -- ffmpeg -y \
  -i docs/images/planeradar-radar.gif \
  -vf 'select=eq(n\,0)+eq(n\,29)+eq(n\,59),scale=480:480:flags=neighbor,tile=3x1' \
  -frames:v 1 \
  "$capture_dir/contact-sheet.png"
```

Inspect the contact sheet and the GIF. Confirm the radar remains edge-to-edge, text is sharp, colors match the accepted renderer, and no frame is blank or corrupted.

- [ ] **Step 7: Change the README contract to require the GIF hero**

In `readme_states_support_maturity_credit_and_disclosure`, replace the two static-hero requirements with:

```rust
"docs/images/planeradar-radar.gif",
"![Plane Radar running on a Raspberry Pi Zero 2 W](docs/images/planeradar-radar.gif)",
```

Keep `accepted_device_capture_is_exact_rgba_480_square` unchanged so the accepted PNG remains hash-pinned.

- [ ] **Step 8: Add a GIF container contract**

Add this test after the existing PNG test:

```rust
#[test]
fn readme_animation_is_native_480_square_and_loops() {
    let path = repository_root().join("docs/images/planeradar-radar.gif");
    let bytes = fs::read(&path).unwrap_or_else(|error| panic!("read {}: {error}", path.display()));
    assert!(bytes.len() >= 13, "GIF is shorter than its logical screen descriptor");
    assert!(
        &bytes[..6] == b"GIF87a" || &bytes[..6] == b"GIF89a",
        "README animation is not a GIF"
    );
    assert_eq!(u16::from_le_bytes([bytes[6], bytes[7]]), 480);
    assert_eq!(u16::from_le_bytes([bytes[8], bytes[9]]), 480);
    assert!(
        bytes.windows(b"NETSCAPE2.0".len()).any(|window| window == b"NETSCAPE2.0"),
        "README animation does not contain an infinite-loop extension"
    );
}
```

- [ ] **Step 9: Run the contract and confirm the README-only failure**

Run:

```sh
mise run docs-check
```

Expected: FAIL because the README still names the PNG. The GIF container test itself must pass.

- [ ] **Step 10: Replace the opening and hero reference**

Use this opening after the CI badge:

```markdown
Plane Radar is a Rust ADS-B display for the Raspberry Pi Zero 2 W and
Pimoroni HyperPixel 2.1 Round. It is tested with 64-bit Raspberry Pi OS Lite
Trixie on the physical display.

The supported configuration is intentionally narrow. Other Raspberry Pi
models, displays, and operating-system releases are not currently supported.
[Version 0.1.0](https://github.com/shayne/RPi-Plane-Radar/releases/tag/v0.1.0)
is the current immutable stable release.

![Plane Radar running on a Raspberry Pi Zero 2 W](docs/images/planeradar-radar.gif)
```

Remove the duplicate AI-assistance paragraph from the opening. Keep the full disclosure in `Credit, licenses, and AI disclosure`.

- [ ] **Step 11: Replace the remaining conversational asides**

Make these exact copy changes while preserving their surrounding sections:

```markdown
The transaction records every verified phase. If the Mac exits or a reboot
outlasts the connection window, rerun the same command to resume safely.
```

```markdown
The frame may contain live callsigns and should be treated as operational
data.
```

```markdown
Plane Radar retains the current and previous two accepted application and
driver pairs for rollback.
```

```markdown
The project does not provide a curl-to-shell path because that would bypass
these verification steps.
```

```markdown
Application and driver releases are versioned separately because driver
changes affect the boot path.
```

Do not change fenced commands, documented paths, links, or operational warnings.

- [ ] **Step 12: Run the documentation contract**

Run:

```sh
mise run docs-check
```

Expected: PASS, including the static PNG hash, GIF dimensions/loop extension, exact install block, public links, support statement, and AI disclosure.

- [ ] **Step 13: Review and commit the application documentation**

Run:

```sh
but diff
but commit readme-live-gif -m $'docs: add live radar preview and tighten README copy\n\nCo-authored-by: Codex <noreply@openai.com>'
```

Expected: the commit contains `README.md`, `tests/docs_contract.rs`, and `docs/images/planeradar-radar.gif`; the temporary capture directory remains ignored and uncommitted.

---

### Task 2: Rewrite the HyperPixel driver README in the same register

**Files:**
- Modify: `/Users/shayne/code/hyperpixel2r-kms/README.md:1-142`
- Test: `/Users/shayne/code/hyperpixel2r-kms/tests/release-contract.sh`

**Interfaces:**
- Consumes: the published stable `v0.1.1` release and canonical source marker `v0.1.1-rc.1`
- Produces: a factual driver README without changing release state

- [ ] **Step 1: Establish the current release-contract baseline**

Run from `/Users/shayne/code/hyperpixel2r-kms`:

```sh
mise run test-release-contract
```

Expected: PASS before the copy edit.

- [ ] **Step 2: Replace the opening and stable-release summary**

Use this opening while preserving the machine-readable HTML marker exactly:

```markdown
This repository provides a standalone DRM/KMS driver for the Pimoroni
HyperPixel 2.1 Round on a Raspberry Pi Zero 2 W. It is tested with 64-bit
Raspberry Pi OS Lite Trixie and the exact kernel checks described below.

<!-- HP2R_CURRENT_RELEASE=v0.1.1-rc.1 -->

Stable release: [`v0.1.1`](https://github.com/shayne/hyperpixel2r-kms/releases/tag/v0.1.1).
This source revision retains `v0.1.1-rc.1` as its canonical candidate marker
for the release contract.
```

- [ ] **Step 3: Replace the remaining conversational asides**

Use these factual replacements in their current sections:

```markdown
For manual operation, use the normal command above.
```

```markdown
If a candidate cannot boot, the firmware clears tryboot and the next power
cycle returns to the previous boot path. A successful build is not sufficient
for promotion; verify the display and touch hardware first.
```

```markdown
A prebuilt module is rejected when any of these values differ.
```

```markdown
The source archive supports local exact-kernel builds. A module archive is
valid only when its kernel facts match the target Pi.
```

```markdown
The release packager performs two clean builds in CI and rejects
nondeterministic archives.
```

Keep the supported-shape table, commands, tryboot warnings, provenance, GPL license, and AI disclosure.

- [ ] **Step 4: Confirm the release marker and commands remain valid**

Run:

```sh
mise run test-release-contract
rg -n 'oddly specific|heroic compatibility|deliberately boring|very polite|close enough|cute property' README.md && exit 1 || true
```

Expected: the release contract passes and the phrase scan prints no matches.

- [ ] **Step 5: Review and commit the driver documentation**

Run:

```sh
but diff
but commit readme-tone -c -m $'docs: tighten README copy\n\nCo-authored-by: Codex <noreply@openai.com>'
```

Expected: the commit changes only `README.md`.

---

### Task 3: Verify, land, and confirm both repositories

**Files:**
- Verify: all files changed by Tasks 1 and 2
- Verify unchanged: `docs/images/planeradar-radar.png`, `release/current-release.txt`

**Interfaces:**
- Consumes: both completed documentation commits
- Produces: clean `main` branches and passing public CI runs

- [ ] **Step 1: Run the complete application verification suite**

Run from `/Users/shayne/code/RPi-Plane-Radar`:

```sh
mise run verify
mise run docs-check
```

Expected: all formatting, Clippy, nextest, dependency-policy, and documentation checks pass.

- [ ] **Step 2: Run the complete driver verification suite**

Run from `/Users/shayne/code/hyperpixel2r-kms`:

```sh
mise run verify
```

Expected: all protocol, GPIO, build, boot lifecycle, and release-contract tests pass.

- [ ] **Step 3: Confirm immutable inputs did not change**

Run:

```sh
git -C /Users/shayne/code/RPi-Plane-Radar diff 208178dc083c26a0aa50d076b1238a38dec04e3c -- docs/images/planeradar-radar.png
git -C /Users/shayne/code/hyperpixel2r-kms diff 261a29f45963ef3fcaf1a23e8e444b4e68d4c370 -- release/current-release.txt
```

Expected: both commands print no diff.

- [ ] **Step 4: Land the application branch through GitButler**

Run from `/Users/shayne/code/RPi-Plane-Radar`:

```sh
but status
but land readme-live-gif --yes
```

Expected: `readme-live-gif` lands on `main` and the returned workspace has no uncommitted changes.

- [ ] **Step 5: Land the driver branch through GitButler**

Run from `/Users/shayne/code/hyperpixel2r-kms`:

```sh
but status
but land readme-tone --yes
```

Expected: `readme-tone` lands on `main` and the returned workspace has no uncommitted changes.

- [ ] **Step 6: Verify remote state and CI**

Run:

```sh
gh run list --repo shayne/RPi-Plane-Radar --branch main --limit 3
gh run list --repo shayne/hyperpixel2r-kms --branch main --limit 3
```

Wait for the runs created by the landed commits and inspect them with `gh run watch` until both conclude successfully. Confirm each remote `main` head matches its local landed commit.

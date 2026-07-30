#!/usr/bin/env bash
set -euo pipefail

readonly REPOSITORY="shayne/RPi-Plane-Radar"
readonly SIGNER_WORKFLOW="shayne/RPi-Plane-Radar/.github/workflows/release.yml"
readonly CONTROL_BOOTSTRAP_ARG="--__planeradar-bootstrap-v1"
readonly CONTROL_FOREGROUND_TTY_ARG="--__planeradar-foreground-tty-v1"
readonly CONTROL_RESTORE_TTY_ARG="--__planeradar-restore-tty-v1"
readonly CONTROL_BOOTSTRAP_MARKER="control-bootstrap.ready"
readonly CONTROL_CONTINUE_MARKER="control-bootstrap.continue"
readonly MAX_INSTALLER_METADATA_BYTES=$((64 * 1024))
readonly MAX_MANIFEST_METADATA_BYTES=$((64 * 1024))
readonly MAX_CHECKSUMS_METADATA_BYTES=$((16 * 1024))
readonly MAX_SBOM_METADATA_BYTES=$((1024 * 1024))
readonly MAX_CONTROL_ARCHIVE_BYTES=$((16 * 1024 * 1024))
readonly MAX_CONTROL_MEMBER_BYTES=$((16 * 1024 * 1024))
readonly MAX_EXPANDED_ARCHIVE_BYTES=$((32 * 1024 * 1024))
readonly MAX_PRIVATE_TEMP_BYTES=$((
  MAX_INSTALLER_METADATA_BYTES +
    MAX_MANIFEST_METADATA_BYTES +
    MAX_CHECKSUMS_METADATA_BYTES +
    MAX_SBOM_METADATA_BYTES +
    MAX_CONTROL_ARCHIVE_BYTES +
    MAX_CONTROL_MEMBER_BYTES +
    MAX_EXPANDED_ARCHIVE_BYTES
))

die() {
  printf 'Plane Radar installer: %s\n' "$*" >&2
  exit 1
}

darwin_file_blocks() {
  local bytes=$1
  printf '%s\n' "$(((bytes + 511) / 512))"
}

usage() {
  cat >&2 <<'EOF'
usage: install.sh [--version VERSION] [--hostname HOSTNAME] [--non-interactive] TARGET
EOF
  exit 64
}

version=""
hostname=""
non_interactive=0
target=""
while [[ $# -gt 0 ]]; do
  case "$1" in
    --version)
      [[ $# -ge 2 && -z "$version" ]] || usage
      version=$2
      shift 2
      ;;
    --hostname)
      [[ $# -ge 2 && -z "$hostname" ]] || usage
      hostname=$2
      shift 2
      ;;
    --non-interactive)
      [[ $non_interactive -eq 0 ]] || usage
      non_interactive=1
      shift
      ;;
    --)
      shift
      [[ $# -eq 1 ]] || usage
      target=$1
      shift
      ;;
    -*)
      usage
      ;;
    *)
      [[ -z "$target" ]] || usage
      target=$1
      shift
      ;;
  esac
done

[[ "$target" =~ ^[A-Za-z_][A-Za-z0-9_.-]*@([A-Za-z0-9]([A-Za-z0-9.-]*[A-Za-z0-9])?|\[[0-9A-Fa-f:]+\])$ ]] ||
  die "target must be one safe OpenSSH user-at-host argument"
if [[ -n "$hostname" ]]; then
  [[ "$hostname" =~ ^[a-z0-9]([a-z0-9-]{0,61}[a-z0-9])?$ ]] ||
    die "hostname must be a lowercase RFC 1123 label"
fi
if [[ -n "$version" ]]; then
  [[ "$version" =~ ^(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)(-([0-9A-Za-z-]+)(\.[0-9A-Za-z-]+)*)?(\+([0-9A-Za-z-]+)(\.[0-9A-Za-z-]+)*)?$ ]] ||
    die "version must be canonical semantic version text"
fi

[[ "$(uname -s)" == "Darwin" ]] || die "the release bootstrap requires macOS"
host_arch="$(uname -m)"
case "$host_arch" in
  arm64)
    control_archive="planeradarctl-aarch64-apple-darwin.tar.zst"
    manifest_arch="aarch64"
    ;;
  x86_64)
    control_archive="planeradarctl-x86_64-apple-darwin.tar.zst"
    manifest_arch="x86_64"
    ;;
  *)
    die "unsupported Mac architecture: $host_arch"
    ;;
esac
command -v gh >/dev/null 2>&1 ||
  die "required command is unavailable: gh"
for command in shasum tar zstd lipo mktemp chmod awk grep wc plutil df stat dd od tr ps; do
  command -v "$command" >/dev/null 2>&1 ||
    die "required command is unavailable: $command"
done

if [[ -n "$version" ]]; then
  requested_tag="v$version"
  release_json="$(gh release view "$requested_tag" -R "$REPOSITORY" \
    --json tagName,isDraft,isPrerelease)" ||
    die "could not resolve requested Plane Radar release"
else
  release_json="$(gh release view -R "$REPOSITORY" \
    --json tagName,isDraft,isPrerelease)" ||
    die "could not resolve the latest stable Plane Radar release"
fi
# `gh` has no local jq mode. Extract only the rigid values produced by
# `gh release view --json`; reject anything outside the expected grammar.
tag="$(printf '%s' "$release_json" | sed -n 's/.*"tagName":"\([^"]*\)".*/\1/p')"
[[ "$tag" =~ ^v(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)(-([0-9A-Za-z-]+)(\.[0-9A-Za-z-]+)*)?(\+([0-9A-Za-z-]+)(\.[0-9A-Za-z-]+)*)?$ ]] ||
  die "release returned a mutable or malformed tag"
[[ "$release_json" == *'"isDraft":false'* ]] || die "draft releases cannot be installed"
if [[ -z "$version" ]]; then
  [[ "$release_json" == *'"isPrerelease":false'* ]] ||
    die "the default installer only accepts a stable release"
else
  [[ "$tag" == "v$version" ]] || die "requested version resolved to a different tag"
fi

ref_type="$(gh api "repos/$REPOSITORY/git/ref/tags/$tag" --jq .object.type)" ||
  die "could not resolve immutable release tag"
ref_sha="$(gh api "repos/$REPOSITORY/git/ref/tags/$tag" --jq .object.sha)" ||
  die "could not resolve immutable release tag"
case "$ref_type" in
  commit)
    source_commit=$ref_sha
    ;;
  tag)
    tag_type="$(gh api "repos/$REPOSITORY/git/tags/$ref_sha" --jq .object.type)" ||
      die "could not dereference annotated release tag"
    [[ "$tag_type" == "commit" ]] || die "release tag does not point to a commit"
    source_commit="$(gh api "repos/$REPOSITORY/git/tags/$ref_sha" --jq .object.sha)" ||
      die "could not dereference annotated release tag"
    ;;
  *)
    die "release ref is not an immutable commit or annotated tag"
    ;;
esac
[[ "$source_commit" =~ ^[0-9a-f]{40}$ ]] || die "release source commit is malformed"

gh release verify "$tag" -R "$REPOSITORY" >/dev/null ||
  die "GitHub release integrity verification failed"

private="$(mktemp -d "${TMPDIR:-/tmp}/planeradar-bootstrap.XXXXXX")"
control_pid=""
control_reap_pid=""
control_reap_pending=0
control_reaped_status=0
control_retire_pending=0
control_cancel_status=0
control_launch_pending=0
control_barrier="$private/$CONTROL_BOOTSTRAP_MARKER"
control_continue_barrier="$private/$CONTROL_CONTINUE_MARKER"
control_barrier_claimed=0
control_group_owned=0
control_terminal_restored=0
control_readiness=""
control=""
cleanup() {
  local status=$? abort_status=0
  if [[ -n "$control_pid" ]]; then
    abort_uncommitted_control || abort_status=$?
  fi
  if [[ $control_reap_pending -eq 1 ]]; then
    kill_and_reap_retired_control || abort_status=$?
  fi
  rm -rf -- "$private"
  [[ $abort_status -eq 0 ]] || status=$abort_status
  return "$status"
}
terminate_with_status() {
  local status=$1
  trap '' HUP INT TERM
  trap - EXIT
  rm -rf -- "$private"
  exit "$status"
}
kill_owned_control_group() {
  [[ -n "$control_pid" && $control_barrier_claimed -eq 1 ]] || return 0
  # The hash- and attestation-verified native control writes and syncs this
  # marker only after setpgid(0, 0) and getpgrp()==getpid(). Therefore the
  # retained root PID also names its owned process group even when the
  # independent ps admission check fails; no unverified member is resumed or
  # signaled individually.
  kill -STOP -- "-$control_pid" 2>/dev/null || true
  kill -KILL -- "-$control_pid" 2>/dev/null || true
}
restore_control_terminal() {
  local restore_status=0
  [[ $control_barrier_claimed -eq 1 && $control_terminal_restored -eq 0 ]] ||
    return 0
  [[ -n "$control" && -x "$control" ]] || return 1
  "$control" "$CONTROL_RESTORE_TTY_ARG" "$control_barrier" <&0 >&1 2>&2 ||
    restore_status=$?
  [[ $restore_status -eq 0 ]] || return "$restore_status"
  control_terminal_restored=1
}
foreground_control_terminal() {
  [[ $control_barrier_claimed -eq 1 && $control_group_owned -eq 1 ]] ||
    return 1
  [[ -n "$control" && -x "$control" ]] || return 1
  "$control" "$CONTROL_FOREGROUND_TTY_ARG" "$control_barrier" <&0 >&1 2>&2
}
abort_uncommitted_control() {
  local restore_status=0
  [[ -n "$control_pid" ]] || return 0
  if [[ $control_barrier_claimed -eq 1 ]]; then
    kill_owned_control_group
  else
    # `$!` is a retained, unreaped direct child, so this PID cannot be reused.
    kill -KILL "$control_pid" 2>/dev/null || true
  fi
  control_reap_pid=$control_pid control_reap_pending=1 control_retire_pending=0 control_group_owned=0 control_pid=""
  kill_and_reap_retired_control || true
  restore_control_terminal || restore_status=$?
  return "$restore_status"
}
await_control_barrier() {
  local attempt readiness early_state snapshot
  for ((attempt = 0; attempt < 200; attempt++)); do
    readiness=""
    if IFS= read -r readiness <"$control_barrier" &&
      [[ "$readiness" == "ready none" ||
         "$readiness" =~ ^ready\ tty\ [1-9][0-9]*\ [1-9][0-9]*$ ]]; then
      control_readiness=$readiness
      control_barrier_claimed=1
      control_terminal_restored=0
      break
    fi
    early_state="$(ps -o state= -p "$control_pid" 2>/dev/null | tr -d '[:space:]')" ||
      return 1
    [[ "$early_state" != Z* ]] || return 1
    /bin/sleep 0.01
  done
  [[ $control_barrier_claimed -eq 1 ]] || return 1

  # Read PPID, PGID, and state from one process-table snapshot. The retained
  # root cannot be reused before wait reaps it, and only this exact stopped
  # process group can ever be continued.
  for ((attempt = 0; attempt < 200; attempt++)); do
    snapshot="$(ps -o ppid= -o pgid= -o state= -p "$control_pid" 2>/dev/null)" ||
      return 1
    set -- $snapshot
    [[ $# -eq 3 && "$1" == "$$" && "$2" == "$control_pid" ]] || return 1
    [[ "$3" != Z* ]] || return 1
    [[ "$3" == T* ]] && {
      control_group_owned=1
      return 0
    }
    /bin/sleep 0.01
  done
  return 1
}
read_control_completion() {
  awk -v readiness="$control_readiness" '
    NR == 1 && $0 != readiness { exit 1 }
    NR == 2 { completion = $0 }
    NR > 2 { exit 1 }
    END {
      if (NR == 2) {
        print completion
      } else {
        exit 1
      }
    }
  ' "$control_barrier" 2>/dev/null
}
await_control_completion() {
  local completion completion_status snapshot
  local control_started=0 resume_attempt=0
  while :; do
    completion_status=""
    completion="$(read_control_completion)" || completion=""
    if [[ "$completion" =~ ^complete\ ([0-9]|[1-9][0-9]|1[0-9][0-9]|2[0-4][0-9]|25[0-5])$ ]]; then
      completion_status=${BASH_REMATCH[1]}
    fi
    snapshot="$(ps -o ppid= -o pgid= -o state= -p "$control_pid" 2>/dev/null)" ||
      return 1
    set -- $snapshot
    [[ $# -eq 3 && "$1" == "$$" && "$2" == "$control_pid" ]] || return 1
    [[ "$3" != Z* ]] || return 1
    if [[ "$3" == T* ]]; then
      if [[ -z "$completion_status" ]]; then
        if [[ $control_started -eq 0 && -z "$completion" ]]; then
          ((resume_attempt += 1))
          [[ $resume_attempt -lt 600 ]] || return 1
          /bin/sleep 0.05
          continue
        fi
        # The child can stop after the first record read but before the
        # process-table snapshot. Once stopped, its synced record is stable;
        # read it once more before deciding that completion is malformed.
        completion="$(read_control_completion)" || completion=""
        [[ "$completion" =~ ^complete\ ([0-9]|[1-9][0-9]|1[0-9][0-9]|2[0-4][0-9]|25[0-5])$ ]] ||
          return 1
        completion_status=${BASH_REMATCH[1]}
      fi
      [[ -n "$completion_status" ]] || return 1
      control_status=$completion_status
      return 0
    fi
    control_started=1
    /bin/sleep 0.05
  done
}
wait_retired_control() {
  local wait_status=0
  [[ $control_reap_pending -eq 1 && -n "$control_reap_pid" ]] || return 1
  if wait "$control_reap_pid" 2>/dev/null; then
    wait_status=0
  else
    wait_status=$?
  fi
  # A handled HUP/INT/TERM can interrupt Bash's first wait without reaping the
  # retained direct child. The first handler ignores repeats, so a second wait
  # is uninterruptible by those signals and returns the supervisor's status.
  if [[ $control_cancel_status -ne 0 ]]; then
    if wait "$control_reap_pid" 2>/dev/null; then
      wait_status=0
    else
      wait_status=$?
    fi
  fi
  control_reaped_status=$wait_status control_reap_pending=0 control_reap_pid=""
}
kill_and_reap_retired_control() {
  [[ $control_reap_pending -eq 1 && -n "$control_reap_pid" ]] || return 0
  # This is still an unreaped direct child, so its individual PID cannot have
  # been reused. Never signal its former process group from this phase.
  kill -CONT "$control_reap_pid" 2>/dev/null || true
  kill -KILL "$control_reap_pid" 2>/dev/null || true
  wait_retired_control
}
retire_completed_control_group() {
  local retire_status=0
  [[ $control_retire_pending -eq 1 && $control_group_owned -eq 1 &&
     -n "$control_pid" ]] || return 1
  kill -STOP -- "-$control_pid" 2>/dev/null || retire_status=1
  kill -KILL -- "-$control_pid" 2>/dev/null || retire_status=1
  return "$retire_status"
}
cancel_owned_control_group() {
  [[ $control_group_owned -eq 1 ]] || return 0
  kill_owned_control_group
}
cancel_control_with_status() {
  local status=$1
  [[ $control_cancel_status -eq 0 ]] || return
  control_cancel_status=$status
  trap '' HUP INT TERM
  [[ $control_retire_pending -eq 0 ]] || return 0
  if [[ -z "$control_pid" ]]; then
    [[ $control_launch_pending -eq 1 ||
       $control_reap_pending -eq 1 ||
       ($control_barrier_claimed -eq 1 && $control_terminal_restored -eq 0) ]] &&
      return 0
    terminate_with_status "$status"
  fi
  cancel_owned_control_group
  return 0
}
handle_hup() {
  cancel_control_with_status 129
}
handle_int() {
  cancel_control_with_status 130
}
handle_term() {
  cancel_control_with_status 143
}
trap cleanup EXIT
trap handle_hup HUP
trap handle_int INT
trap handle_term TERM
chmod 0700 "$private"
release_dir="$private/release"
mkdir -m 0700 "$release_dir"

required_free_kib=$(((MAX_PRIVATE_TEMP_BYTES + 1023) / 1024))
available_kib="$(df -Pk "$private" | awk 'NR == 2 { print $4 }')"
[[ "$available_kib" =~ ^[0-9]+$ && "$available_kib" -ge "$required_free_kib" ]] ||
  die "insufficient temporary disk space for bounded release bootstrap"

download_bounded_metadata() {
  local name=$1 maximum=$2 path actual_size
  path="$release_dir/$name"
  (
    ulimit -f "$(darwin_file_blocks "$maximum")"
    gh release download "$tag" -R "$REPOSITORY" --pattern "$name" --dir "$release_dir"
  ) || die "metadata download was incomplete or exceeded its size limit: $name"
  [[ -f "$path" && ! -L "$path" ]] ||
    die "metadata download was not one regular file: $name"
  actual_size="$(stat -f '%z' "$path")" ||
    die "metadata size could not be determined: $name"
  [[ "$actual_size" =~ ^[1-9][0-9]*$ && "$actual_size" -le "$maximum" ]] ||
    die "metadata asset was empty or exceeded its size limit: $name"
}

download_bounded_metadata install.sh "$MAX_INSTALLER_METADATA_BYTES"
download_bounded_metadata release-manifest.json "$MAX_MANIFEST_METADATA_BYTES"
download_bounded_metadata SHA256SUMS "$MAX_CHECKSUMS_METADATA_BYTES"
download_bounded_metadata SBOM.spdx.json "$MAX_SBOM_METADATA_BYTES"

checksum_names="$(awk 'NF == 2 && $1 ~ /^[0-9a-f]{64}$/ { sub(/^\\*/, "", $2); print $2 }' \
  "$release_dir/SHA256SUMS" | LC_ALL=C sort)"
expected_checksum_names="$(printf '%s\n' \
  planeradar-aarch64-linux-gnu.tar.zst \
  planeradarctl-aarch64-apple-darwin.tar.zst \
  planeradarctl-x86_64-apple-darwin.tar.zst \
  install.sh release-manifest.json SBOM.spdx.json | LC_ALL=C sort)"
[[ "$checksum_names" == "$expected_checksum_names" ]] ||
  die "checksum manifest has an incomplete or ambiguous subject set"
for name in install.sh release-manifest.json SBOM.spdx.json; do
  expected_digest="$(awk -v name="$name" '$2 == name || $2 == "*" name { print $1 }' \
    "$release_dir/SHA256SUMS")"
  [[ "$expected_digest" =~ ^[0-9a-f]{64}$ ]] ||
    die "checksum manifest has no unique digest for $name"
  [[ "$(shasum -a 256 "$release_dir/$name" | awk '{print $1}')" == "$expected_digest" ]] ||
    die "downloaded $name failed checksum verification"
done

manifest_version="$(plutil -extract version raw "$release_dir/release-manifest.json" 2>/dev/null)" ||
  die "release manifest is not valid JSON"
manifest_commit="$(plutil -extract source_commit raw "$release_dir/release-manifest.json" 2>/dev/null)" ||
  die "release manifest has no source identity"
manifest_workflow_ref="$(plutil -extract workflow.ref raw "$release_dir/release-manifest.json" 2>/dev/null)" ||
  die "release manifest has no workflow ref"
manifest_workflow_commit="$(plutil -extract workflow.commit raw "$release_dir/release-manifest.json" 2>/dev/null)" ||
  die "release manifest has no workflow commit"
[[ "$tag" == "v$manifest_version" &&
   "$manifest_commit" == "$source_commit" &&
   "$manifest_workflow_ref" =~ ^refs/(heads|tags)/[A-Za-z0-9]([A-Za-z0-9._/-]*[A-Za-z0-9])?$ &&
   "$manifest_workflow_ref" != *".."* &&
   "$manifest_workflow_ref" != *"//"* &&
   "$manifest_workflow_commit" == "$source_commit" ]] ||
  die "release tag, manifest, and source identity do not match"
artifact_key="${control_archive//./\\.}"
manifest_artifact_digest="$(plutil -extract "artifacts.$artifact_key.sha256" raw \
  "$release_dir/release-manifest.json" 2>/dev/null)" ||
  die "release manifest does not declare the selected control archive"
manifest_artifact_size="$(plutil -extract "artifacts.$artifact_key.size" raw \
  "$release_dir/release-manifest.json" 2>/dev/null)" ||
  die "release manifest does not declare the selected control size"
manifest_artifact_kind="$(plutil -extract "artifacts.$artifact_key.kind" raw \
  "$release_dir/release-manifest.json" 2>/dev/null)" ||
  die "release manifest does not declare the selected control kind"
manifest_artifact_platform="$(plutil -extract "artifacts.$artifact_key.platform" raw \
  "$release_dir/release-manifest.json" 2>/dev/null)" ||
  die "release manifest does not declare the selected control platform"
manifest_artifact_arch="$(plutil -extract "artifacts.$artifact_key.architecture" raw \
  "$release_dir/release-manifest.json" 2>/dev/null)" ||
  die "release manifest does not declare the selected control architecture"
[[ "$manifest_artifact_digest" =~ ^[0-9a-f]{64}$ &&
   "$manifest_artifact_size" =~ ^[1-9][0-9]*$ &&
   "$manifest_artifact_size" -le "$MAX_CONTROL_ARCHIVE_BYTES" &&
   "$manifest_artifact_kind" == "control" &&
   "$manifest_artifact_platform" == "apple-darwin" &&
   "$manifest_artifact_arch" == "$manifest_arch" ]] ||
  die "selected control archive does not match the release manifest"

# macOS supplies RLIMIT_FSIZE to Bash's `ulimit -f` in 512-byte blocks. This
# limits the release client itself, not merely a post-download size check.
(
  ulimit -f "$(darwin_file_blocks "$MAX_CONTROL_ARCHIVE_BYTES")"
  gh release download "$tag" -R "$REPOSITORY" \
    --pattern "$control_archive" --dir "$release_dir"
) || die "release download was incomplete or exceeded the compressed-size limit"

actual="$(find "$release_dir" -mindepth 1 -maxdepth 1 -type f -exec basename {} \; | LC_ALL=C sort)"
expected="$(printf '%s\n' "$control_archive" install.sh release-manifest.json SHA256SUMS SBOM.spdx.json | LC_ALL=C sort)"
[[ "$actual" == "$expected" ]] || die "download contained missing or unexpected files"
[[ "$(find "$release_dir" -mindepth 1 -maxdepth 1 ! -type f -print -quit)" == "" ]] ||
  die "download contained a non-regular entry"

expected_digest="$(awk -v name="$control_archive" '$2 == name || $2 == "*" name { print $1 }' \
  "$release_dir/SHA256SUMS")"
[[ "$expected_digest" =~ ^[0-9a-f]{64}$ &&
   "$(shasum -a 256 "$release_dir/$control_archive" | awk '{print $1}')" == "$expected_digest" ]] ||
  die "downloaded $control_archive failed checksum verification"
actual_archive_size="$(stat -f '%z' "$release_dir/$control_archive")"
[[ "$actual_archive_size" == "$manifest_artifact_size" &&
   "$actual_archive_size" -le "$MAX_CONTROL_ARCHIVE_BYTES" &&
   "$manifest_artifact_digest" == "$expected_digest" ]] ||
  die "selected control archive does not match the release manifest"

for subject in install.sh "$control_archive"; do
  gh attestation verify "$release_dir/$subject" \
    --repo "$REPOSITORY" \
    --signer-workflow "$SIGNER_WORKFLOW" \
    --source-ref "$manifest_workflow_ref" \
    --source-digest "$manifest_workflow_commit" \
    --deny-self-hosted-runners >/dev/null ||
    die "release attestation verification failed for $subject"
done

expanded_archive="$private/control.tar"
(
  ulimit -f "$(darwin_file_blocks "$MAX_EXPANDED_ARCHIVE_BYTES")"
  zstd -dc -- "$release_dir/$control_archive" >"$expanded_archive"
) || die "control archive decompression failed or exceeded the expanded-size limit"
expanded_size="$(stat -f '%z' "$expanded_archive")"
[[ "$expanded_size" =~ ^[1-9][0-9]*$ &&
   "$expanded_size" -le "$MAX_EXPANDED_ARCHIVE_BYTES" ]] ||
  die "control archive exceeded the expanded-size limit"

members="$(tar -tf "$expanded_archive")" ||
  die "control archive could not be read"
[[ "$members" == "planeradarctl" ]] ||
  die "control archive contains an unsafe or unexpected member"
[[ "$(tar -tvf "$expanded_archive" | wc -l | tr -d ' ')" == "1" ]] ||
  die "control archive contains duplicate members"
tar -tvf "$expanded_archive" | grep -Eq '^-rwxr-xr-x .* planeradarctl$' ||
  die "control archive member is not a normalized regular executable"

member_type="$(dd if="$expanded_archive" bs=1 skip=156 count=1 2>/dev/null |
  od -An -t u1 | tr -d '[:space:]')"
[[ "$member_type" == "0" || "$member_type" == "48" ]] ||
  die "control archive member is not a regular file"
member_size_octal="$(dd if="$expanded_archive" bs=1 skip=124 count=12 2>/dev/null |
  tr -d '\000[:space:]')"
[[ "$member_size_octal" =~ ^[0-7]+$ ]] ||
  die "control archive member size is malformed"
member_size=$((8#$member_size_octal))
[[ "$member_size" -ge 1 && "$member_size" -le "$MAX_CONTROL_MEMBER_BYTES" ]] ||
  die "control archive member exceeded the executable-size limit"

control="$private/planeradarctl"
(
  ulimit -f "$(darwin_file_blocks "$MAX_CONTROL_MEMBER_BYTES")"
  tar -xOf "$expanded_archive" planeradarctl >"$control"
) ||
  die "control executable extraction failed"
[[ "$(stat -f '%z' "$control")" == "$member_size" ]] ||
  die "control executable extraction length did not match the archive header"
chmod 0700 "$control"
[[ "$(lipo -archs "$control")" == "$host_arch" ]] ||
  die "control executable architecture does not match this Mac"

# Execute the verified equivalent of: planeradarctl install TARGET ...
argv=(install "$target" --version "$manifest_version")
[[ -z "$hostname" ]] || argv+=(--hostname "$hostname")
[[ $non_interactive -eq 0 ]] || argv+=(--non-interactive)
(umask 077 && set -o noclobber &&
  : >"$control_barrier" &&
  : >"$control_continue_barrier") ||
  die "could not create the private control bootstrap markers"
[[ -f "$control_barrier" && ! -L "$control_barrier" &&
   -f "$control_continue_barrier" && ! -L "$control_continue_barrier" ]] ||
  die "control bootstrap markers are not private regular files"
control_status=0
control_launch_pending=1
"$control" "$CONTROL_BOOTSTRAP_ARG" "$control_barrier" \
  "$control_continue_barrier" "${argv[@]}" <&0 >&1 2>&2 &
control_pid=$!
control_launch_pending=0
if ! await_control_barrier; then
  abort_status=0
  abort_uncommitted_control || abort_status=$?
  [[ $control_cancel_status -eq 0 ]] ||
    terminate_with_status "$control_cancel_status"
  [[ $abort_status -eq 0 ]] ||
    die "verified control terminal foreground could not be restored"
  die "verified control bootstrap barrier failed"
fi
if [[ $control_cancel_status -ne 0 ]]; then
  cancel_owned_control_group
elif ! kill -CONT -- "-$control_pid" 2>/dev/null; then
  abort_status=0
  abort_uncommitted_control || abort_status=$?
  [[ $control_cancel_status -eq 0 ]] ||
    terminate_with_status "$control_cancel_status"
  [[ $abort_status -eq 0 ]] ||
    die "verified control terminal foreground could not be restored"
  die "verified control bootstrap could not start"
fi
# The resumed child waits on a separate private marker and cannot expose
# inherited stdin yet. Complete the launcher-side handoff, return to Bash,
# then acknowledge it so the child performs the final verified handoff.
if [[ $control_cancel_status -eq 0 ]] &&
  ! foreground_control_terminal; then
  abort_status=0
  abort_uncommitted_control || abort_status=$?
  [[ $control_cancel_status -eq 0 ]] ||
    terminate_with_status "$control_cancel_status"
  [[ $abort_status -eq 0 ]] ||
    die "verified control terminal foreground could not be restored"
  die "verified control terminal foreground handoff failed"
fi
if [[ $control_cancel_status -eq 0 ]] &&
  ! printf 'continue\n' >"$control_continue_barrier"; then
  abort_status=0
  abort_uncommitted_control || abort_status=$?
  [[ $control_cancel_status -eq 0 ]] ||
    terminate_with_status "$control_cancel_status"
  [[ $abort_status -eq 0 ]] ||
    die "verified control terminal foreground could not be restored"
  die "verified control continue acknowledgement failed"
fi
if ! await_control_completion 2>/dev/null; then
  abort_status=0
  abort_uncommitted_control || abort_status=$?
  [[ $control_cancel_status -eq 0 ]] ||
    terminate_with_status "$control_cancel_status"
  [[ $abort_status -eq 0 ]] ||
    die "verified control terminal foreground could not be restored"
  die "verified control completion barrier failed"
fi
# Once retirement starts, signal traps only latch their conventional status.
# The retained root still anchors the stopped group while both group signals
# are sent, so no descendant can survive past the authority transition.
control_retire_pending=1
# Bash can report deliberate background-job SIGKILL asynchronously while the
# retirement function returns. Keep termination, authority transfer, and reap
# in one redirected compound command so that delayed shell notification cannot
# escape between commands. The worker's stderr has already been delivered.
retire_status=0
restore_status=0
reap_status=0
{
  retire_completed_control_group || retire_status=$?
  if [[ $retire_status -eq 0 ]]; then
    # One assignment-only command moves the killed retained root into the reap
    # phase, leaves retirement mode, and removes every authority to signal its
    # process group. DEBUG traps can run before or after, never between.
    control_reap_pid=$control_pid control_reap_pending=1 control_retire_pending=0 control_group_owned=0 control_pid=""
    restore_control_terminal || restore_status=$?
    wait_retired_control || reap_status=$?
  fi
} 2>/dev/null
if [[ $retire_status -ne 0 ]]; then
  abort_status=0
  abort_uncommitted_control || abort_status=$?
  [[ $control_cancel_status -eq 0 ]] ||
    terminate_with_status "$control_cancel_status"
  [[ $abort_status -eq 0 ]] ||
    die "verified control terminal foreground could not be restored"
  die "verified control process group could not be retired"
fi
[[ $control_cancel_status -eq 0 ]] ||
  terminate_with_status "$control_cancel_status"
[[ $restore_status -eq 0 ]] ||
  die "verified control terminal foreground could not be restored"
[[ $reap_status -eq 0 ]] ||
  die "verified control supervisor could not be reaped"
[[ "$control_reaped_status" == 137 ]] ||
  die "verified control supervisor was not killed during group retirement"
exit "$control_status"

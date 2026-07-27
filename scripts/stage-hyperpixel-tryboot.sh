#!/usr/bin/env bash
set -euo pipefail

umask 022
script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
repository="$(cd "$script_dir/.." && pwd -P)"
source "$script_dir/hyperpixel-build-common.sh"

parameters=()
while (($#)); do
  case "$1" in
    --parameter)
      test "$#" -ge 2 || {
        echo "--parameter requires a value" >&2
        exit 2
      }
      parameters+=("$2")
      shift 2
      ;;
    *)
      echo "unknown argument: $1" >&2
      exit 2
      ;;
  esac
done

cd "$repository"
but_status="$(but status)"
grep -Fq "[uncommitted] (no changes)" <<<"$but_status" || {
  echo "stage-hyperpixel-tryboot requires a clean GitButler workspace" >&2
  exit 1
}
test -z "$(git status --porcelain=v1 --untracked-files=all)" || {
  echo "stage-hyperpixel-tryboot requires a clean GitButler workspace" >&2
  exit 1
}

target="${1:-${PLANERADAR_PI_TARGET:-pi@raspberrypi.local}}"
case "$target" in
  ""|-*|*[!A-Za-z0-9._@-]*|@*|*@|*@*@*)
    echo "unsafe Plane Radar SSH target: $target" >&2
    exit 1
    ;;
esac
ssh_options=(-o BatchMode=yes -o ConnectTimeout=8 -o ConnectionAttempts=1)
release="$(ssh "${ssh_options[@]}" "$target" uname -r)"
hp2r_validate_release "$release"
driver_dir="${PLANERADAR_DRIVER_ARTIFACT_DIR:-dist/hyperpixel/$release}"
target_manifest="${PLANERADAR_KERNEL_TARGET_MANIFEST:-dist/kernel-target/$release/target.txt}"
app_dir="${PLANERADAR_APP_ARTIFACT_DIR:-dist}"

require_regular_file() {
  local path="$1"
  test ! -L "$path" && test -f "$path" || {
    echo "required artifact is missing or unsafe: $path" >&2
    exit 1
  }
}

sha256_file() {
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$1" | awk '{ print $1 }'
  else
    shasum -a 256 "$1" | awk '{ print $1 }'
  fi
}

validate_checksum_sidecar() {
  local checksum_file="$1"
  local expected_digest="$2"
  local expected_basename="$3"
  awk -v digest="$expected_digest" -v basename="$expected_basename" '
    NR == 1 && NF == 2 && $1 == digest && $2 == basename { valid = 1 }
    END { exit !(NR == 1 && valid) }
  ' "$checksum_file"
}

manifest="$driver_dir/manifest.txt"
require_regular_file "$manifest"
require_regular_file "$target_manifest"
hp2r_validate_artifact_provenance \
  "$manifest" \
  "$target_manifest" \
  "$driver_dir"
awk -F '\t' '
  NF != 2 || $1 == "" || $2 == "" || seen[$1]++ { exit 1 }
' "$manifest" || {
  echo "driver manifest contains a malformed or duplicate field" >&2
  exit 1
}
manifest_value() {
  local key="$1"
  local value
  local count
  count="$(awk -F '\t' -v wanted="$key" '$1 == wanted { count++ } END { print count + 0 }' "$manifest")"
  test "$count" -eq 1 || {
    echo "driver manifest must contain exactly one $key field" >&2
    exit 1
  }
  value="$(awk -F '\t' -v wanted="$key" '$1 == wanted && NF == 2 { print $2 }' "$manifest")"
  test -n "$value" || {
    echo "driver manifest field is empty or malformed: $key" >&2
    exit 1
  }
  printf '%s\n' "$value"
}

source_revision="$(manifest_value source_revision)"
source_tree="$(manifest_value source_tree)"
source_dirty="$(manifest_value source_dirty)"
manifest_release="$(manifest_value kernel_release)"
kernel_arch="$(manifest_value kernel_arch)"
build_image="$(manifest_value build_image)"
build_command="$(manifest_value build_command)"
base_dtb_sha256="$(manifest_value base_dtb_sha256)"
module_file="$(manifest_value module_file)"
module_sha256="$(manifest_value module_sha256)"
module_vermagic="$(manifest_value module_vermagic)"
module_license="$(manifest_value module_license)"
overlay_file="$(manifest_value overlay_file)"
overlay_sha256="$(manifest_value overlay_sha256)"
overlay_applied_dtb="$(manifest_value overlay_applied_dtb)"

[[ "$source_revision" =~ ^[0-9a-f]{40}$ ]] || {
  echo "driver manifest source revision is unsafe" >&2
  exit 1
}
[[ "$source_tree" =~ ^[0-9a-f]{40}$ ]] || {
  echo "driver manifest source tree is unsafe" >&2
  exit 1
}
test "$source_dirty" = false
test "$manifest_release" = "$release" || {
  echo "live kernel $release does not match driver manifest $manifest_release" >&2
  exit 1
}
test "$kernel_arch" = aarch64
test -n "$build_image"
test -n "$build_command"
[[ "$base_dtb_sha256" =~ ^[0-9a-f]{64}$ ]]
test "$module_file" = planeradar_hyperpixel2r.ko
[[ "$module_sha256" =~ ^[0-9a-f]{64}$ ]]
case "$module_vermagic" in
  "$release "*) ;;
  *)
    echo "driver module vermagic does not match the live kernel release" >&2
    exit 1
    ;;
esac
test "$module_license" = GPL
test "$overlay_file" = "planeradar-hyperpixel2r-${source_revision:0:12}.dtbo" || {
  echo "driver manifest overlay filename does not match its revision" >&2
  exit 1
}
test "$overlay_applied_dtb" = planeradar-hyperpixel2r-applied.dtb
[[ "$overlay_sha256" =~ ^[0-9a-f]{64}$ ]]
test "$(git rev-parse HEAD)" = "$source_revision" || {
  echo "driver manifest revision does not match the clean workspace" >&2
  exit 1
}
test "$(git rev-parse 'HEAD^{tree}')" = "$source_tree" || {
  echo "driver manifest tree does not match the clean workspace" >&2
  exit 1
}

module="$driver_dir/$module_file"
overlay="$driver_dir/$overlay_file"
driver_artifacts=(
  "$module" \
  "$overlay" \
  "$driver_dir/$overlay_applied_dtb" \
  "$driver_dir/module.sha256" \
  "$driver_dir/module.file.txt" \
  "$driver_dir/module.modinfo.txt" \
  "$driver_dir/module.readelf.txt" \
  "$driver_dir/host-fixdep" \
  "$driver_dir/host-modpost" \
  "$driver_dir/host-genksyms"
)
for artifact in "${driver_artifacts[@]}"; do
  require_regular_file "$artifact"
done
for artifact in "$driver_dir"/*; do
  require_regular_file "$artifact"
  case "$(basename "$artifact")" in
    manifest.txt|planeradar_hyperpixel2r.ko|planeradar-hyperpixel2r-applied.dtb|module.sha256|module.file.txt|module.modinfo.txt|module.readelf.txt|host-fixdep|host-modpost|host-genksyms|"$overlay_file") ;;
    *)
      echo "unexpected stale driver artifact: $artifact" >&2
      exit 1
      ;;
  esac
done
test "$(sha256_file "$module")" = "$module_sha256" || {
  echo "driver module checksum does not match manifest" >&2
  exit 1
}
validate_checksum_sidecar \
  "$driver_dir/module.sha256" \
  "$module_sha256" \
  planeradar_hyperpixel2r.ko || {
  echo "module.sha256 is malformed or does not match the manifest" >&2
  exit 1
}
test "$(sha256_file "$overlay")" = "$overlay_sha256" || {
  echo "driver overlay checksum does not match manifest" >&2
  exit 1
}

app="$app_dir/planeradar"
app_revision_file="$app_dir/planeradar.revision"
app_tree_file="$app_dir/planeradar.tree"
app_checksum_file="$app_dir/planeradar.sha256"
app_readelf="$app_dir/planeradar.readelf.txt"
for artifact in "$app" "$app_revision_file" "$app_tree_file" "$app_checksum_file" "$app_readelf"; do
  require_regular_file "$artifact"
done
app_revision="$(tr -d '\n' < "$app_revision_file")"
test "$app_revision" = "$source_revision" || {
  echo "app revision does not match the driver manifest" >&2
  exit 1
}
app_tree="$(tr -d '\n' < "$app_tree_file")"
test "$app_tree" = "$source_tree" || {
  echo "app source tree does not match the driver manifest" >&2
  exit 1
}
app_sha256="$(sha256_file "$app")"
validate_checksum_sidecar "$app_checksum_file" "$app_sha256" planeradar || {
  echo "planeradar.sha256 is malformed or does not match the app" >&2
  exit 1
}

for parameter in "${parameters[@]}"; do
  case "$parameter" in
    rotate=0|rotate=90|rotate=180|rotate=270|touchscreen-inverted-x|touchscreen-inverted-y|touchscreen-swapped-x-y) ;;
    *)
      echo "unsupported HyperPixel parameter: $parameter" >&2
      exit 1
      ;;
  esac
done
for ((left = 0; left < ${#parameters[@]}; left++)); do
  for ((right = left + 1; right < ${#parameters[@]}; right++)); do
    test "${parameters[left]}" != "${parameters[right]}" || {
      echo "duplicate HyperPixel parameter: ${parameters[left]}" >&2
      exit 1
    }
  done
done

payload="$(mktemp -d "${TMPDIR:-/tmp}/planeradar-hyperpixel-payload.XXXXXX")"
remote_stage=""
cleanup() {
  local status=$?
  if test -n "$remote_stage"; then
    ssh "${ssh_options[@]}" "$target" rm -rf -- "$remote_stage" >/dev/null 2>&1 || true
  fi
  rm -rf -- "$payload"
  return "$status"
}
trap cleanup EXIT

install -d -m 0755 "$payload/dkms-source"
install -m 0644 "$manifest" "$payload/manifest.txt"
for artifact in "${driver_artifacts[@]}"; do
  install -m 0644 "$artifact" "$payload/$(basename "$artifact")"
done
install -m 0755 "$app" "$payload/planeradar"
install -m 0644 "$app_revision_file" "$payload/planeradar.revision"
install -m 0644 "$app_tree_file" "$payload/planeradar.tree"
install -m 0644 "$app_checksum_file" "$payload/planeradar.sha256"
install -m 0644 "$app_readelf" "$payload/planeradar.readelf.txt"
printf '%s\n' "${parameters[@]}" > "$payload/display-parameters.txt"
chmod 0644 "$payload/display-parameters.txt"

kernel_sources=(
  Kbuild
  Makefile
  dkms.conf
  LICENSE
  README.md
  planeradar_hyperpixel2r_gpio.c
  planeradar_hyperpixel2r_gpio.h
  planeradar_hyperpixel2r_main.c
  planeradar_hyperpixel2r_protocol.c
  planeradar_hyperpixel2r_protocol.h
)
for source_name in "${kernel_sources[@]}"; do
  require_regular_file "kernel/$source_name"
  install -m 0644 "kernel/$source_name" "$payload/dkms-source/$source_name"
done

remote_stage="$(ssh "${ssh_options[@]}" "$target" mktemp -d /tmp/planeradar-hyperpixel-stage.XXXXXX)"
[[ "$remote_stage" =~ ^/tmp/planeradar-hyperpixel-stage\.[A-Za-z0-9]+$ ]] || {
  echo "target returned an unsafe staging path: $remote_stage" >&2
  exit 1
}
scp "${ssh_options[@]}" -rp "$payload/." "${target}:${remote_stage}/"

ssh "${ssh_options[@]}" "$target" bash -s -- \
  "$remote_stage" "$source_revision" "$release" "$overlay_file" "$module_file" <<'REMOTE'
set -euo pipefail
umask 022
remote_stage="$1"
revision="$2"
release="$3"
overlay_file="$4"
module_file="$5"
root="${PLANERADAR_INSTALL_ROOT:-}"
incoming="${root}${remote_stage}"
artifact_root="${root}/usr/lib/planeradar/hyperpixel"
artifact_parent="${artifact_root}/${revision}"
artifact_dir="${artifact_parent}/${release}"
module_dir="${root}/lib/modules/${release}/extra"
overlay_dir="${root}/boot/firmware/overlays"
normal_config="${root}/boot/firmware/config.txt"
tryboot_config="${root}/boot/firmware/tryboot.txt"
dkms_dir="${root}/usr/src/planeradar-hyperpixel2r-0.1.0"
publish_tmp=""
dkms_tmp=""
dkms_backup_parent=""
dkms_backup=""
dkms_upgrade_active=false
dkms_prior_registered=false
dkms_remove_attempted=false
dkms_add_attempted=false
rollback_dir="$(mktemp -d "${root}/tmp/planeradar-tryboot-rollback.XXXXXX")"
tryboot_backup=""
tryboot_mode=""
tryboot_existed=false
tryboot_touched=false
stage_complete=false

atomic_install() {
  local source="$1"
  local destination="$2"
  local mode="$3"
  local mode_policy="${4:-exact}"
  local directory
  local temporary
  local actual_mode
  directory="$(dirname "$destination")"
  sudo install -d -m 0755 "$directory"
  if sudo test -e "$destination" || sudo test -L "$destination"; then
    if ! sudo test -f "$destination" || sudo test -L "$destination"; then
      echo "refusing non-regular installation destination: $destination" >&2
      return 1
    fi
  fi
  temporary="$(sudo mktemp "${directory}/.$(basename "$destination").XXXXXX")"
  if ! sudo install -m "$mode" "$source" "$temporary"; then
    sudo rm -f -- "$temporary"
    return 1
  fi
  sudo chown root:root "$temporary"
  if ! sudo mv -f "$temporary" "$destination"; then
    sudo rm -f -- "$temporary"
    return 1
  fi
  sudo test -f "$destination"
  sudo test ! -L "$destination"
  actual_mode="$(sudo stat -c '%a' "$destination")"
  case "$mode_policy:$actual_mode" in
    "exact:${mode#0}"|boot-overlay:644|boot-overlay:755) ;;
    *)
      echo "unexpected installed mode: $destination ($actual_mode)" >&2
      return 1
      ;;
  esac
  test "$(sudo stat -c '%U:%G' "$destination")" = root:root
  test "$(sha256sum "$destination" | awk '{ print $1 }')" = \
    "$(sha256sum "$source" | awk '{ print $1 }')"
  sudo -u shayne test -r "$destination"
}

validate_tree_metadata() {
  local actual="$1"
  local expected="$2"
  local executable_relative="$3"
  local actual_list
  local expected_list
  local entry
  local relative
  local counterpart
  local expected_mode
  local mode
  local owner
  local failure=""

  actual_list="$(mktemp "$rollback_dir/tree-actual.XXXXXX")"
  expected_list="$(mktemp "$rollback_dir/tree-expected.XXXXXX")"
  if ! sudo find "$actual" -print0 > "$actual_list" ||
     ! sudo find "$expected" -print0 > "$expected_list"
  then
    rm -f -- "$actual_list" "$expected_list"
    echo "failed to enumerate staged tree metadata: $actual" >&2
    return 1
  fi

  while IFS= read -r -d '' entry; do
    if test "$entry" = "$actual"; then
      relative=""
      counterpart="$expected"
    else
      relative="${entry#"$actual"/}"
      counterpart="$expected/$relative"
    fi
    if sudo test -L "$entry"; then
      failure="symlink in staged tree: $entry"
      break
    elif sudo test -d "$entry"; then
      if ! sudo test -d "$counterpart" || sudo test -L "$counterpart"; then
        failure="unexpected directory entry in staged tree: $entry"
        break
      fi
      expected_mode=755
    elif sudo test -f "$entry"; then
      if ! sudo test -f "$counterpart" || sudo test -L "$counterpart"; then
        failure="unexpected regular-file entry in staged tree: $entry"
        break
      fi
      if test -n "$executable_relative" &&
         test "$relative" = "$executable_relative"
      then
        expected_mode=755
      else
        expected_mode=644
      fi
    else
      failure="special file in staged tree: $entry"
      break
    fi
    if ! mode="$(sudo stat -c '%a' "$entry")"; then
      failure="failed to read staged object mode: $entry"
      break
    fi
    if test "$mode" != "$expected_mode"; then
      failure="unexpected staged object mode: $entry ($mode)"
      break
    fi
    if ! owner="$(sudo stat -c '%U:%G' "$entry")"; then
      failure="failed to read staged object ownership: $entry"
      break
    fi
    if test "$owner" != root:root; then
      failure="non-root-owned staged object: $entry ($owner)"
      break
    fi
  done < "$actual_list"

  if test -z "$failure"; then
    while IFS= read -r -d '' entry; do
      if test "$entry" = "$expected"; then
        relative=""
        counterpart="$actual"
      else
        relative="${entry#"$expected"/}"
        counterpart="$actual/$relative"
      fi
      if sudo test -L "$entry"; then
        failure="symlink in expected staged tree: $entry"
        break
      elif sudo test -d "$entry"; then
        if ! sudo test -d "$counterpart" || sudo test -L "$counterpart"; then
          failure="missing expected directory in staged tree: $counterpart"
          break
        fi
      elif sudo test -f "$entry"; then
        if ! sudo test -f "$counterpart" || sudo test -L "$counterpart"; then
          failure="missing expected regular file in staged tree: $counterpart"
          break
        fi
      else
        failure="special file in expected staged tree: $entry"
        break
      fi
    done < "$expected_list"
  fi

  rm -f -- "$actual_list" "$expected_list"
  if test -n "$failure"; then
    echo "refusing unsafe staged tree metadata: $failure" >&2
    return 1
  fi
}

validate_installed_revision_source() {
  local candidate="$1"
  local release_dir="${candidate%/dkms-source}"
  local candidate_release="${release_dir##*/}"
  local revision_dir="${release_dir%/*}"
  local candidate_revision="${revision_dir##*/}"
  local manifest="$release_dir/manifest.txt"
  local revision_file="$release_dir/planeradar.revision"
  local tree_file="$release_dir/planeradar.tree"
  local path
  local expected_mode

  test "${revision_dir%/*}" = "$artifact_root" || {
    echo "installed DKMS source candidate escapes revision root: $candidate" >&2
    return 1
  }
  [[ "$candidate_revision" =~ ^[0-9a-f]{40}$ ]] || {
    echo "installed DKMS source candidate has an unsafe revision: $candidate" >&2
    return 1
  }
  case "$candidate_release" in
    ""|"."|".."|*[!A-Za-z0-9._+-]*)
      echo "installed DKMS source candidate has an unsafe release: $candidate" >&2
      return 1
      ;;
  esac
  for path in "$revision_dir" "$release_dir" "$candidate"; do
    sudo test -d "$path"
    sudo test ! -L "$path"
    test "$(sudo stat -c '%a' "$path")" = 755
    test "$(sudo stat -c '%U:%G' "$path")" = root:root
  done
  for path in "$manifest" "$revision_file" "$tree_file"; do
    sudo test -f "$path"
    sudo test ! -L "$path"
    expected_mode="$(sudo stat -c '%a' "$path")"
    test "$expected_mode" = 644
    test "$(sudo stat -c '%U:%G' "$path")" = root:root
  done
  test "$(cat "$revision_file")" = "$candidate_revision"
  [[ "$(cat "$tree_file")" =~ ^[0-9a-f]{40}$ ]]
  awk -F '\t' -v wanted="$candidate_revision" '
    $1 == "source_revision" {
      count++
      if (NF == 2 && $2 == wanted) matching++
    }
    END { exit !(count == 1 && matching == 1) }
  ' "$manifest"
  awk -F '\t' -v wanted="$candidate_release" '
    $1 == "kernel_release" {
      count++
      if (NF == 2 && $2 == wanted) matching++
    }
    END { exit !(count == 1 && matching == 1) }
  ' "$manifest"
  validate_tree_metadata "$candidate" "$candidate" ""
}

cleanup_remote() {
  local status=$?
  if "$dkms_upgrade_active" && ! "$stage_complete"; then
    if "$dkms_remove_attempted" || "$dkms_add_attempted"; then
      sudo dkms remove -m planeradar-hyperpixel2r -v 0.1.0 --all || status=1
    fi
    sudo rm -rf -- "$dkms_dir" || status=1
    if test -n "$dkms_backup" && sudo test -d "$dkms_backup"; then
      sudo mv "$dkms_backup" "$dkms_dir" || status=1
      dkms_backup=""
    else
      echo "failed to restore prior DKMS source after staging failure" >&2
      status=1
    fi
    if "$dkms_prior_registered"; then
      sudo dkms add -m planeradar-hyperpixel2r -v 0.1.0 || status=1
    fi
  fi
  if "$tryboot_touched" && ! "$stage_complete"; then
    if "$tryboot_existed"; then
      atomic_install "$tryboot_backup" "$tryboot_config" "$tryboot_mode" || status=1
    else
      sudo rm -f -- "$tryboot_config" || status=1
    fi
    sudo sync || status=1
  fi
  test -z "$publish_tmp" || sudo rm -rf -- "$publish_tmp"
  test -z "$dkms_tmp" || sudo rm -rf -- "$dkms_tmp"
  test -z "$dkms_backup_parent" || sudo rm -rf -- "$dkms_backup_parent" || status=1
  rm -rf -- "$rollback_dir"
  rm -rf -- "$incoming"
  return "$status"
}
trap cleanup_remote EXIT

test -d "$incoming"
test -z "$(find "$incoming" -type l -print -quit)"
test -z "$(find "$incoming" ! -type d ! -type f -print -quit)"
test "$(cat "$incoming/planeradar.revision")" = "$revision"
test "$(cat "$incoming/planeradar.tree")" = \
  "$(awk -F '\t' '$1 == "source_tree" { print $2 }' "$incoming/manifest.txt")"
incoming_app_sha="$(sha256sum "$incoming/planeradar" | awk '{ print $1 }')"
awk -v digest="$incoming_app_sha" '
  NR == 1 && NF == 2 && $1 == digest && $2 == "planeradar" { valid = 1 }
  END { exit !(NR == 1 && valid) }
' "$incoming/planeradar.sha256"
(cd "$incoming" && sha256sum -c planeradar.sha256)
module_sha="$(awk -F '\t' '$1 == "module_sha256" { print $2 }' "$incoming/manifest.txt")"
overlay_sha="$(awk -F '\t' '$1 == "overlay_sha256" { print $2 }' "$incoming/manifest.txt")"
awk -v digest="$module_sha" '
  NR == 1 && NF == 2 &&
    $1 == digest && $2 == "planeradar_hyperpixel2r.ko" { valid = 1 }
  END { exit !(NR == 1 && valid) }
' "$incoming/module.sha256"
test "$(sha256sum "$incoming/$module_file" | awk '{ print $1 }')" = "$module_sha"
test "$(sha256sum "$incoming/$overlay_file" | awk '{ print $1 }')" = "$overlay_sha"
for specification in \
  host-fixdep:host_fixdep_sha256 \
  host-modpost:host_modpost_sha256 \
  host-genksyms:host_genksyms_sha256
do
  helper="${specification%%:*}"
  checksum_key="${specification#*:}"
  helper_sha="$(
    awk -F '\t' -v wanted="$checksum_key" '$1 == wanted { print $2 }' \
      "$incoming/manifest.txt"
  )"
  test "$(sha256sum "$incoming/$helper" | awk '{ print $1 }')" = "$helper_sha"
done

test -f "$normal_config" && test ! -L "$normal_config"
normal_sha="$(sha256sum "$normal_config" | awk '{ print $1 }')"
normal_config_unchanged() {
  test "$(sha256sum "$normal_config" | awk '{ print $1 }')" = "$normal_sha" || {
    echo "normal boot configuration changed while staging tryboot" >&2
    return 1
  }
}
boot_config_lines_within_limit() {
  LC_ALL=C awk '
    {
      line = $0
      sub(/\r$/, "", line)
      if (length(line) > 98) exit 1
    }
  ' "$1"
}

sudo apt-get update
sudo apt-get install -y --no-install-recommends dkms evtest kmod pngcheck
normal_config_unchanged

sudo install -d -m 0755 "$artifact_parent"
if sudo test -L "$artifact_dir"; then
  echo "artifact destination is a symlink: $artifact_dir" >&2
  exit 1
fi
publish_tmp="$(sudo mktemp -d "${artifact_parent}/.${release}.XXXXXX")"
sudo cp -a "$incoming/." "$publish_tmp/"
sudo find "$publish_tmp" -type d -exec chmod 0755 {} +
sudo find "$publish_tmp" -type f -exec chmod 0644 {} +
sudo chmod 0755 "$publish_tmp/planeradar"
sudo chown -R root:root "$publish_tmp"
if sudo test -e "$artifact_dir"; then
  sudo test -d "$artifact_dir"
  validate_tree_metadata "$artifact_dir" "$publish_tmp" planeradar
  sudo diff -qr "$publish_tmp" "$artifact_dir" >/dev/null || {
    echo "staged artifact directory already exists with different contents" >&2
    exit 1
  }
  validate_tree_metadata "$artifact_dir" "$publish_tmp" planeradar
  sudo rm -rf -- "$publish_tmp"
  publish_tmp=""
else
  sudo mv "$publish_tmp" "$artifact_dir"
  publish_tmp=""
fi
validate_tree_metadata "$artifact_dir" "$artifact_dir" planeradar
sudo -u shayne test -x "$artifact_dir/planeradar"
sudo -u shayne test -r "$artifact_dir/manifest.txt"

atomic_install "$artifact_dir/$module_file" "$module_dir/planeradar_hyperpixel2r.ko" 0644
atomic_install \
  "$artifact_dir/$overlay_file" \
  "$overlay_dir/$overlay_file" \
  0644 \
  boot-overlay

sudo install -d -m 0755 "$(dirname "$dkms_dir")"
if sudo test -L "$dkms_dir"; then
  echo "DKMS source destination is a symlink: $dkms_dir" >&2
  exit 1
fi
dkms_tmp="$(sudo mktemp -d "$(dirname "$dkms_dir")/.planeradar-hyperpixel2r-0.1.0.XXXXXX")"
sudo cp -a "$artifact_dir/dkms-source/." "$dkms_tmp/"
sudo find "$dkms_tmp" -type d -exec chmod 0755 {} +
sudo find "$dkms_tmp" -type f -exec chmod 0644 {} +
sudo chown -R root:root "$dkms_tmp"
dkms_status=""
dkms_status_read=false
if dkms_status="$(
  sudo dkms status -m planeradar-hyperpixel2r -v 0.1.0 2>/dev/null
)"; then
  dkms_status_read=true
else
  dkms_status=""
fi
dkms_registered=false
dkms_status_recognized=true
dkms_built_record='^planeradar-hyperpixel2r/0\.1\.0, [A-Za-z0-9][A-Za-z0-9._+-]*, aarch64: (built|installed)$'
while IFS= read -r dkms_record; do
  test -n "$dkms_record" || continue
  if test "$dkms_record" = "planeradar-hyperpixel2r/0.1.0: added" ||
     [[ "$dkms_record" =~ $dkms_built_record ]]
  then
    dkms_registered=true
  else
    dkms_status_recognized=false
  fi
done <<<"$dkms_status"
if ! "$dkms_status_recognized"; then
  dkms_registered=false
fi

dkms_source_upgraded=false
if sudo test -e "$dkms_dir"; then
  sudo test -d "$dkms_dir"
  validate_tree_metadata "$dkms_dir" "$dkms_dir" ""
  if sudo diff -qr "$dkms_tmp" "$dkms_dir" >/dev/null; then
    validate_tree_metadata "$dkms_dir" "$dkms_tmp" ""
    sudo rm -rf -- "$dkms_tmp"
    dkms_tmp=""
  else
    "$dkms_status_read" && "$dkms_status_recognized" || {
      echo "refusing DKMS source upgrade with unrecognized status" >&2
      exit 1
    }
    installed_sources="$(mktemp "$rollback_dir/installed-dkms-sources.XXXXXX")"
    sudo find "$artifact_root" \
      -mindepth 3 \
      -maxdepth 3 \
      -type d \
      -name dkms-source \
      -print0 > "$installed_sources"
    trusted_source=""
    while IFS= read -r -d '' candidate_source; do
      validate_installed_revision_source "$candidate_source"
      if sudo diff -qr "$dkms_dir" "$candidate_source" >/dev/null; then
        validate_tree_metadata "$dkms_dir" "$dkms_dir" ""
        validate_installed_revision_source "$candidate_source"
        trusted_source="$candidate_source"
        break
      fi
    done < "$installed_sources"
    test -n "$trusted_source" || {
      echo "DKMS source directory differs from every installed Plane Radar revision" >&2
      exit 1
    }

    dkms_backup_parent="$(
      sudo mktemp -d \
        "$(dirname "$dkms_dir")/.planeradar-hyperpixel2r-0.1.0.rollback.XXXXXX"
    )"
    dkms_backup="$dkms_backup_parent/source"
    sudo install -d -m 0755 "$dkms_backup"
    validate_tree_metadata "$dkms_dir" "$dkms_dir" ""
    validate_installed_revision_source "$trusted_source"
    sudo diff -qr "$dkms_dir" "$trusted_source" >/dev/null
    sudo cp -a "$dkms_dir/." "$dkms_backup/"
    sudo chown -R root:root "$dkms_backup"
    validate_tree_metadata "$dkms_backup" "$dkms_dir" ""
    sudo diff -qr "$dkms_backup" "$dkms_dir" >/dev/null
    sudo diff -qr "$dkms_backup" "$trusted_source" >/dev/null
    validate_tree_metadata "$dkms_dir" "$dkms_backup" ""
    validate_installed_revision_source "$trusted_source"

    dkms_prior_registered="$dkms_registered"
    dkms_upgrade_active=true
    if "$dkms_prior_registered"; then
      dkms_remove_attempted=true
      sudo dkms remove -m planeradar-hyperpixel2r -v 0.1.0 --all
    fi
    sudo rm -rf -- "$dkms_dir"
    sudo mv "$dkms_tmp" "$dkms_dir"
    dkms_tmp=""
    validate_tree_metadata "$dkms_dir" "$dkms_dir" ""
    dkms_source_upgraded=true
  fi
else
  sudo mv "$dkms_tmp" "$dkms_dir"
  dkms_tmp=""
fi
validate_tree_metadata "$dkms_dir" "$dkms_dir" ""

if "$dkms_source_upgraded"; then
  dkms_add_attempted=true
  sudo dkms add -m planeradar-hyperpixel2r -v 0.1.0
elif ! "$dkms_registered"; then
  sudo dkms add -m planeradar-hyperpixel2r -v 0.1.0
fi
sudo depmod -a "$release"

normal_config_unchanged
if sudo test -e "$tryboot_config"; then
  sudo test -f "$tryboot_config"
  sudo test ! -L "$tryboot_config"
  tryboot_existed=true
  tryboot_backup="$rollback_dir/tryboot.txt"
  tryboot_mode="$(stat -c '%a' "$tryboot_config")"
  sudo cp "$tryboot_config" "$tryboot_backup"
fi
command=(
  "$artifact_dir/planeradar"
  stage-display
  --boot-config "$normal_config"
  --tryboot-config "$tryboot_config"
  --expected-boot-config-sha256 "$normal_sha"
  --overlay "${overlay_file%.dtbo}"
)
while IFS= read -r parameter; do
  test -z "$parameter" || command+=(--parameter "$parameter")
done < "$artifact_dir/display-parameters.txt"
tryboot_touched=true
sudo "${command[@]}"
boot_config_lines_within_limit "$tryboot_config" || {
  echo "tryboot configuration contains a line longer than 98 bytes" >&2
  exit 1
}
sudo sync
stage_complete=true
REMOTE

remote_stage=""
printf 'Staged HyperPixel candidate %s for %s\n' "$source_revision" "$release"
printf "sudo reboot '0 tryboot'\n"

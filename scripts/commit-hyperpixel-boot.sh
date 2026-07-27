#!/usr/bin/env bash
set -euo pipefail

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
repository="$(cd "$script_dir/.." && pwd -P)"
source "$script_dir/hyperpixel-build-common.sh"
cd "$repository"

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
manifest="$driver_dir/manifest.txt"
test ! -L "$manifest" && test -f "$manifest" || {
  echo "missing or unsafe driver manifest: $manifest" >&2
  exit 1
}

manifest_value() {
  local key="$1"
  local value
  test "$(awk -F '\t' -v wanted="$key" '$1 == wanted { count++ } END { print count + 0 }' "$manifest")" -eq 1 || {
    echo "driver manifest must contain exactly one $key field" >&2
    exit 1
  }
  value="$(awk -F '\t' -v wanted="$key" '$1 == wanted && NF == 2 { print $2 }' "$manifest")"
  test -n "$value"
  printf '%s\n' "$value"
}

revision="$(manifest_value source_revision)"
manifest_release="$(manifest_value kernel_release)"
overlay_file="$(manifest_value overlay_file)"
[[ "$revision" =~ ^[0-9a-f]{40}$ ]]
test "$manifest_release" = "$release"
test "$overlay_file" = "planeradar-hyperpixel2r-${revision:0:12}.dtbo"

ssh "${ssh_options[@]}" "$target" bash -s -- \
  "$revision" "$release" "${overlay_file%.dtbo}" <<'REMOTE'
set -euo pipefail
revision="$1"
release="$2"
overlay="$3"
root="${PLANERADAR_INSTALL_ROOT:-}"
artifact_dir="${root}/usr/lib/planeradar/hyperpixel/${revision}/${release}"
boot_config="${root}/boot/firmware/config.txt"
manifest="$artifact_dir/manifest.txt"
parameters_file="$artifact_dir/display-parameters.txt"
app="$artifact_dir/planeradar"

boot_config_lines_within_limit() {
  LC_ALL=C awk '
    {
      line = $0
      sub(/\r$/, "", line)
      if (length(line) > 98) exit 1
    }
  ' "$1"
}

test -f "$manifest" && test ! -L "$manifest"
test -f "$parameters_file" && test ! -L "$parameters_file"
test -x "$app" && test ! -L "$app"
test "$(awk -F '\t' '$1 == "source_revision" { print $2 }' "$manifest")" = "$revision"
test "$(awk -F '\t' '$1 == "kernel_release" { print $2 }' "$manifest")" = "$release"
test "$(awk -F '\t' '$1 == "overlay_file" { print $2 }' "$manifest")" = "$overlay.dtbo"
test "$(cat "$artifact_dir/planeradar.tree")" = \
  "$(awk -F '\t' '$1 == "source_tree" { print $2 }' "$manifest")"
"$app" version | grep -Fq "($revision)"

command=(
  "$app"
  commit-display
  --boot-config "$boot_config"
  --overlay "$overlay"
)
while IFS= read -r parameter; do
  test -z "$parameter" || command+=(--parameter "$parameter")
done < "$parameters_file"
sudo "${command[@]}"

stock_count="$(
  awk '
    {
      line = $0
      sub(/^[[:space:]]+/, "", line)
      sub(/[[:space:]]+$/, "", line)
      if (line ~ /^dtoverlay=vc4-kms-dpi-hyperpixel2r([,[:space:]]|$)/) count++
    }
    END { print count + 0 }
  ' "$boot_config"
)"
custom_count="$(
  awk -v wanted="dtoverlay=$overlay" '
    {
      line = $0
      sub(/^[[:space:]]+/, "", line)
      sub(/[[:space:]]+$/, "", line)
      if (line ~ /^dtoverlay=planeradar-hyperpixel2r-/) custom++
      if (line == wanted) accepted++
    }
    END {
      if (custom == 1 && accepted == 1) print 1
      else print 0
    }
  ' "$boot_config"
)"
test "$stock_count" -eq 0
test "$custom_count" -eq 1
boot_config_lines_within_limit "$boot_config"
sudo sync
REMOTE

printf 'Committed HyperPixel candidate %s to normal boot config\n' "$revision"
printf 'sudo reboot\n'

#!/usr/bin/env bash
set -euo pipefail

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
source "$script_dir/hyperpixel-build-common.sh"

target="${PLANERADAR_PI_TARGET:-shayne@planeradar.local}"
release="$(ssh "$target" uname -r)"
hp2r_validate_release "$release"
artifact_parent="dist/hyperpixel"
test -d "$artifact_parent"
artifact_dir="$(hp2r_release_path "$artifact_parent" "$release")"
manifest="$artifact_dir/manifest.txt"
module="$artifact_dir/planeradar_hyperpixel2r.ko"
image="planeradar-kernel-builder:debian-trixie-gcc14"

for artifact in \
  "$module" \
  "$manifest" \
  "$artifact_dir/module.sha256" \
  "$artifact_dir/module.modinfo.txt" \
  "$artifact_dir/host-fixdep" \
  "$artifact_dir/host-modpost" \
  "$artifact_dir/host-genksyms"
do
  test -f "$artifact" || {
    echo "missing driver artifact: $artifact" >&2
    exit 1
  }
done

target_parent="dist/kernel-target"
test -d "$target_parent"
target_dir="$(hp2r_release_path "$target_parent" "$release")"
target_file="$target_dir/target.txt"
test -f "$target_file" || {
  echo "missing target export for $release; run mise run export-pi-kernel-build" >&2
  exit 1
}
hp2r_validate_artifact_provenance "$manifest" "$target_file" "$artifact_dir"

manifest_value() {
  hp2r_manifest_value "$manifest" "$1"
}

target_manifest_value() {
  hp2r_manifest_value "$target_file" "$1"
}

overlay_file="$(manifest_value overlay_file)"
overlay_applied_dtb="$(manifest_value overlay_applied_dtb)"
hp2r_require_clean_source
checked_source_revision="$(git rev-parse HEAD)"
checked_source_tree="$(git rev-parse 'HEAD^{tree}')"
hp2r_validate_source_identity \
  "$(manifest_value source_dirty)" \
  "$(manifest_value source_revision)" \
  "$(manifest_value source_tree)" \
  "$overlay_file" \
  "$checked_source_revision" \
  "$checked_source_tree"
[[ "$overlay_file" =~ ^planeradar-hyperpixel2r-[0-9a-f]{12}\.dtbo$ ]] || {
  echo "driver artifact overlay filename is invalid: $overlay_file" >&2
  exit 1
}
test "$overlay_applied_dtb" = planeradar-hyperpixel2r-applied.dtb || {
  echo "driver artifact applied DTB filename is invalid: $overlay_applied_dtb" >&2
  exit 1
}
overlay="$artifact_dir/$overlay_file"
applied_dtb="$artifact_dir/$overlay_applied_dtb"
for artifact in "$overlay" "$applied_dtb"; do
  test -f "$artifact" || {
    echo "missing driver artifact: $artifact" >&2
    exit 1
  }
done

test "$(manifest_value kernel_release)" = "$release" || {
  echo "driver artifact kernel release does not match live target" >&2
  exit 1
}
test "$(manifest_value kernel_arch)" = aarch64
test "$(manifest_value module_file)" = planeradar_hyperpixel2r.ko
test "$(manifest_value module_license)" = GPL

actual_sha256="$(shasum -a 256 "$module" | awk '{ print $1 }')"
test "$(manifest_value module_sha256)" = "$actual_sha256" || {
  echo "driver module checksum does not match manifest" >&2
  exit 1
}
test "$(awk '{ print $1 }' "$artifact_dir/module.sha256")" = "$actual_sha256" || {
  echo "driver module checksum does not match module.sha256" >&2
  exit 1
}
actual_overlay_sha256="$(shasum -a 256 "$overlay" | awk '{ print $1 }')"
test "$(manifest_value overlay_sha256)" = "$actual_overlay_sha256" || {
  echo "driver overlay checksum does not match manifest" >&2
  exit 1
}
bash "$script_dir/validate-hyperpixel-overlay.sh" "$overlay"

base_dtb_path="$(target_manifest_value base_dtb_path)"
target_root="$target_dir/root"
base_dtb="$target_root$base_dtb_path"
test -f "$base_dtb" || {
  echo "missing exported base DTB: $base_dtb" >&2
  exit 1
}
actual_base_dtb_sha256="$(shasum -a 256 "$base_dtb" | awk '{ print $1 }')"
test "$(target_manifest_value base_dtb_sha256)" = "$actual_base_dtb_sha256"
test "$(manifest_value base_dtb_sha256)" = "$actual_base_dtb_sha256"

artifact_abs="$artifact_dir"
inspection_dir="$(mktemp -d "${TMPDIR:-/tmp}/planeradar-module-check.XXXXXX")"
trap 'rm -rf "$inspection_dir"' EXIT
docker run --rm \
  --volume "$artifact_abs:/artifacts:ro" \
  --volume "$inspection_dir:/inspection" \
  --volume "$target_root:/target-root:ro" \
  "$image" \
  sh -eu -c '
    file /artifacts/planeradar_hyperpixel2r.ko
    readelf -h /artifacts/planeradar_hyperpixel2r.ko
    modinfo /artifacts/planeradar_hyperpixel2r.ko
    case "$4" in
      arm64|aarch64) expected_machine=AArch64 ;;
      amd64|x86_64) expected_machine="Advanced Micro Devices X86-64" ;;
      *) exit 64 ;;
    esac
    for specification in \
      host-fixdep:1 \
      host-modpost:0 \
      host-genksyms:0
    do
      helper="${specification%:*}"
      expected_status="${specification#*:}"
      machine="$(
        readelf -h "/artifacts/$helper" |
          sed -n "s/^[[:space:]]*Machine:[[:space:]]*//p" |
          head -n 1
      )"
      test "$machine" = "$expected_machine"
      set +e
      "/artifacts/$helper" </dev/null >/dev/null 2>&1
      status="$?"
      set -e
      test "$status" -eq "$expected_status"
    done
    fdtoverlay \
      -i "/target-root$1" \
      -o /inspection/reapplied.dtb \
      "/artifacts/$2"
    cmp /inspection/reapplied.dtb "/artifacts/$3"
    dtc \
      -q \
      -I dtb \
      -O dts \
      -o /inspection/applied.dts \
      "/artifacts/$3"
    dtc \
      -q \
      -I dtb \
      -O dts \
      -o /inspection/overlay.dts \
      "/artifacts/$2"
  ' sh \
    "$base_dtb_path" \
    "$overlay_file" \
    "$overlay_applied_dtb" \
    "$(manifest_value build_host_arch)" \
  > "$inspection_dir/module-inspection.txt"

grep -Fq 'ARM aarch64' "$inspection_dir/module-inspection.txt"
grep -Eq 'Machine:[[:space:]]+AArch64' "$inspection_dir/module-inspection.txt"
license="$(
  awk -F ': *' '$1 == "license" { sub(/^license: */, ""); print; exit }' \
    "$inspection_dir/module-inspection.txt"
)"
vermagic="$(
  awk -F ': *' '$1 == "vermagic" { sub(/^vermagic: */, ""); print; exit }' \
    "$inspection_dir/module-inspection.txt"
)"
depends="$(
  awk -F ': *' '$1 == "depends" { sub(/^depends: */, ""); print; exit }' \
    "$inspection_dir/module-inspection.txt"
)"
test "$license" = GPL
grep -Eq '^alias:[[:space:]]+of:N\*T\*Cplaneradar,hyperpixel2r$' \
  "$inspection_dir/module-inspection.txt"
grep -Eq '^softdep:[[:space:]]+pre: edt_ft5x06$' \
  "$inspection_dir/module-inspection.txt"
grep -Eq '^name:[[:space:]]+planeradar_hyperpixel2r$' \
  "$inspection_dir/module-inspection.txt"
case "$vermagic" in
  "$release"*) ;;
  *)
    echo "driver module vermagic does not match live kernel: $vermagic" >&2
    exit 1
    ;;
esac
printf '%s\n' "$depends" | tr ',-' '\n_' | grep -Fqx i2c_algo_bit
test "$(manifest_value module_vermagic)" = "$vermagic"

fdt_hex() {
  docker run --rm \
    --volume "$artifact_abs:/artifacts:ro" \
    "$image" \
    fdtget -t x "/artifacts/$overlay_applied_dtb" "$1" "$2"
}

fdt_string() {
  docker run --rm \
    --volume "$artifact_abs:/artifacts:ro" \
    "$image" \
    fdtget -t s "/artifacts/$overlay_applied_dtb" "$1" "$2"
}

base_symbol_path() {
  docker run --rm \
    --volume "$target_root:/target-root:ro" \
    "$image" \
    fdtget -t s "/target-root$base_dtb_path" /__symbols__ "$1"
}

applied_dts="$inspection_dir/applied.dts"
overlay_dts="$inspection_dir/overlay.dts"
test "$(grep -Fc 'compatible = "planeradar,hyperpixel2r";' "$applied_dts")" -eq 1
test "$(grep -Fc 'compatible = "edt,edt-ft5406";' "$applied_dts")" -eq 1

panel_path=/planeradar-hyperpixel2r
touch_path="$panel_path/touchscreen@15"
panel_endpoint_path="$panel_path/port/endpoint"
gpio_path="$(base_symbol_path gpio)"
dpi_path="$(base_symbol_path dpi)"
dpi_endpoint_path="$dpi_path/port/endpoint"
dpi_pinctrl_path="$(base_symbol_path dpi_18bit_cpadhi_gpio0)"

gpio_phandle="$(fdt_hex "$gpio_path" phandle)"
dpi_pinctrl_phandle="$(fdt_hex "$dpi_pinctrl_path" phandle)"
test "$(fdt_string "$panel_path" compatible)" = planeradar,hyperpixel2r
test "$(fdt_hex "$panel_path" sda-gpios)" = "$gpio_phandle a 0"
test "$(fdt_hex "$panel_path" scl-gpios)" = "$gpio_phandle b 0"
test "$(fdt_hex "$panel_path" cs-gpios)" = "$gpio_phandle 12 1"
test "$(fdt_hex "$panel_path" backlight-gpios)" = "$gpio_phandle 13 0"
test "$(fdt_hex "$panel_path" rotation)" = 0

test "$(fdt_string "$touch_path" compatible)" = edt,edt-ft5406
test "$(fdt_hex "$touch_path" reg)" = 15
test "$(fdt_hex "$touch_path" interrupt-parent)" = "$gpio_phandle"
test "$(fdt_hex "$touch_path" interrupts)" = "1b 2"
test "$(fdt_hex "$touch_path" touchscreen-size-x)" = 1e0
test "$(fdt_hex "$touch_path" touchscreen-size-y)" = 1e0

panel_endpoint_phandle="$(fdt_hex "$panel_endpoint_path" phandle)"
dpi_endpoint_phandle="$(fdt_hex "$dpi_endpoint_path" phandle)"
test "$(fdt_hex "$panel_endpoint_path" remote-endpoint)" = "$dpi_endpoint_phandle"
test "$(fdt_hex "$dpi_endpoint_path" remote-endpoint)" = "$panel_endpoint_phandle"

test "$(fdt_string "$dpi_path" status)" = okay
test "$(fdt_string "$dpi_path" pinctrl-names)" = default
test "$(fdt_hex "$dpi_path" pinctrl-0)" = "$dpi_pinctrl_phandle"

for parameter in \
  rotate \
  touchscreen-inverted-x \
  touchscreen-inverted-y \
  touchscreen-swapped-x-y
do
  grep -Eq "^[[:space:]]*$parameter =" "$overlay_dts"
done

printf 'HyperPixel driver artifacts match live target %s\n' "$release"

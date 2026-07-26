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
  "$artifact_dir/module.modinfo.txt"
do
  test -f "$artifact" || {
    echo "missing driver artifact: $artifact" >&2
    exit 1
  }
done

manifest_value() {
  awk -F '\t' -v wanted="$1" '$1 == wanted { print $2 }' "$manifest"
}

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

artifact_abs="$artifact_dir"
inspection_dir="$(mktemp -d "${TMPDIR:-/tmp}/planeradar-module-check.XXXXXX")"
trap 'rm -rf "$inspection_dir"' EXIT
docker run --rm \
  --platform linux/arm64 \
  --volume "$artifact_abs:/artifacts:ro" \
  "$image" \
  sh -eu -c '
    ln -s /usr/bin/aarch64-linux-gnu-readelf /usr/local/bin/readelf
    file /artifacts/planeradar_hyperpixel2r.ko
    readelf -h /artifacts/planeradar_hyperpixel2r.ko
    modinfo /artifacts/planeradar_hyperpixel2r.ko
  ' > "$inspection_dir/module-inspection.txt"

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

printf 'HyperPixel driver artifacts match live target %s\n' "$release"

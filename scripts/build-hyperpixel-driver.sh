#!/usr/bin/env bash
set -euo pipefail

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
source "$script_dir/hyperpixel-build-common.sh"

target="${PLANERADAR_PI_TARGET:-shayne@planeradar.local}"
release="$(ssh "$target" uname -r)"
hp2r_validate_release "$release"

target_parent="dist/kernel-target"
test -d "$target_parent"
target_dir="$(hp2r_release_path "$target_parent" "$release")"
target_file="$target_dir/target.txt"
test -f "$target_file" || {
  echo "missing target export for $release; run mise run export-pi-kernel-build" >&2
  exit 1
}

manifest_value() {
  awk -F '\t' -v wanted="$1" '$1 == wanted { print $2 }' "$target_file"
}

test "$(manifest_value kernel_release)" = "$release"
test "$(manifest_value kernel_arch)" = aarch64
header_path="$(manifest_value header_path)"
kbuild_path="$(manifest_value kbuild_path)"
base_dtb_sha256="$(manifest_value base_dtb_sha256)"
root_dir="$target_dir/root"
config_path="$root_dir$header_path/.config"
test -f "$config_path"
test -f "$root_dir$header_path/Module.symvers"
test -d "$root_dir/usr/src"
test -d "$root_dir$kbuild_path"

for setting in \
  CONFIG_DRM_PANEL=y \
  CONFIG_I2C_ALGOBIT=m \
  CONFIG_TOUCHSCREEN_EDT_FT5X06=m \
  CONFIG_OF_OVERLAY=y \
  CONFIG_DRM_VC4=m \
  CONFIG_DRM_V3D=m
do
  grep -Fqx "$setting" "$config_path" || {
    echo "target kernel is missing required setting: $setting" >&2
    exit 1
  }
done

image="planeradar-kernel-builder:debian-trixie-gcc14"
docker info >/dev/null 2>&1 || {
  command -v orbctl >/dev/null && orbctl start
}
for attempt in {1..30}; do
  docker info >/dev/null 2>&1 && break
  sleep 1
done
docker info >/dev/null
docker buildx build \
  --platform linux/arm64 \
  --load \
  --tag "$image" \
  --file packaging/Dockerfile.kernel \
  .

build_dir="$(mktemp -d "${TMPDIR:-/tmp}/planeradar-kernel-build.XXXXXX")"
trap 'rm -rf "$build_dir"' EXIT
cp -R kernel "$build_dir/kernel"

build_command="make -C /usr/src/linux-headers-${release} M=/build/kernel ARCH=arm64 CROSS_COMPILE=aarch64-linux-gnu- W=1 modules"
docker run --rm \
  --platform linux/arm64 \
  --volume "$build_dir/kernel:/build/kernel" \
  --volume "$root_dir/usr/src:/usr/src:ro" \
  --volume "$root_dir$kbuild_path:$kbuild_path:ro" \
  "$image" \
  sh -eu -c '
    ln -s /usr/bin/aarch64-linux-gnu-as /usr/local/bin/as
    ln -s /usr/bin/aarch64-linux-gnu-readelf /usr/local/bin/readelf
    make -C "/usr/src/linux-headers-$1" \
      M=/build/kernel \
      ARCH=arm64 \
      CROSS_COMPILE=aarch64-linux-gnu- \
      W=1 \
      modules
    cd /build/kernel
    file planeradar_hyperpixel2r.ko > module.file.txt
    readelf -h planeradar_hyperpixel2r.ko > module.readelf.txt
    modinfo planeradar_hyperpixel2r.ko > module.modinfo.txt
    sha256sum planeradar_hyperpixel2r.ko > module.sha256
    cat module.file.txt module.readelf.txt module.modinfo.txt
  ' sh "$release"

output_parent="dist/hyperpixel"
mkdir -p "$output_parent"
output_dir="$(hp2r_release_path "$output_parent" "$release")"
mkdir -p "$output_dir"
rm -f \
  "$output_dir/planeradar_hyperpixel2r.ko" \
  "$output_dir/manifest.txt" \
  "$output_dir/module.file.txt" \
  "$output_dir/module.readelf.txt" \
  "$output_dir/module.modinfo.txt" \
  "$output_dir/module.sha256"
cp \
  "$build_dir/kernel/planeradar_hyperpixel2r.ko" \
  "$build_dir/kernel/module.file.txt" \
  "$build_dir/kernel/module.readelf.txt" \
  "$build_dir/kernel/module.modinfo.txt" \
  "$build_dir/kernel/module.sha256" \
  "$output_dir/"

source_revision="$(git rev-parse HEAD)"
if test -n "$(git status --porcelain)"; then
  source_dirty=true
else
  source_dirty=false
fi
module_sha256="$(awk '{ print $1 }' "$output_dir/module.sha256")"
module_vermagic="$(
  awk -F ': *' '$1 == "vermagic" { sub(/^vermagic: */, ""); print; exit }' \
    "$output_dir/module.modinfo.txt"
)"
module_license="$(
  awk -F ': *' '$1 == "license" { sub(/^license: */, ""); print; exit }' \
    "$output_dir/module.modinfo.txt"
)"

{
  printf 'source_revision\t%s\n' "$source_revision"
  printf 'source_dirty\t%s\n' "$source_dirty"
  printf 'kernel_release\t%s\n' "$release"
  printf 'kernel_arch\taarch64\n'
  printf 'build_image\t%s\n' "$image"
  printf 'build_command\t%s\n' "$build_command"
  printf 'base_dtb_sha256\t%s\n' "$base_dtb_sha256"
  printf 'module_file\tplaneradar_hyperpixel2r.ko\n'
  printf 'module_sha256\t%s\n' "$module_sha256"
  printf 'module_vermagic\t%s\n' "$module_vermagic"
  printf 'module_license\t%s\n' "$module_license"
} > "$output_dir/manifest.txt"

printf 'Built HyperPixel driver bundle at %s\n' "$output_dir"

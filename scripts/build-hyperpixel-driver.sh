#!/usr/bin/env bash
set -euo pipefail

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
source "$script_dir/hyperpixel-build-common.sh"

target="${PLANERADAR_PI_TARGET:-shayne@planeradar.local}"
release="$(ssh "$target" uname -r)"
hp2r_validate_release "$release"
hp2r_require_clean_source
git cat-file -e HEAD:kernel/planeradar-hyperpixel2r-overlay.dts || {
  echo "overlay source is not present in checked HEAD" >&2
  exit 1
}
source_revision="$(git rev-parse HEAD)"
source_tree="$(git rev-parse 'HEAD^{tree}')"

output_parent="dist/hyperpixel"
mkdir -p "$output_parent"
hp2r_release_path "$output_parent" "$release" >/dev/null

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
common_header_path="$(manifest_value common_header_path)"
kbuild_path="$(manifest_value kbuild_path)"
kernel_source_package="$(manifest_value kernel_source_package)"
kernel_source_version="$(manifest_value kernel_source_version)"
kernel_source_deb_package="$(manifest_value kernel_source_deb_package)"
kernel_source_deb_sha256="$(manifest_value kernel_source_deb_sha256)"
kernel_source_deb="$(manifest_value kernel_source_deb)"
base_dtb_path="$(manifest_value base_dtb_path)"
base_dtb_sha256="$(manifest_value base_dtb_sha256)"
root_dir="$target_dir/root"
config_path="$root_dir$header_path/.config"
test -f "$config_path"
test -f "$root_dir$header_path/Module.symvers"
test -d "$root_dir/usr/src"
test -d "$root_dir$common_header_path/include"
test -d "$root_dir$kbuild_path"
test -f "$root_dir$base_dtb_path"
test "$kernel_source_package" = linux
test -n "$kernel_source_version"
[[ "$kernel_source_deb_package" =~ ^linux-source-[0-9]+\.[0-9]+$ ]]
[[ "$kernel_source_deb_sha256" =~ ^[0-9a-f]{64}$ ]]
test "$kernel_source_deb" = kernel-source.deb
test -f "$target_dir/$kernel_source_deb"
hp2r_verify_sha256 \
  "$target_dir/$kernel_source_deb" \
  "$kernel_source_deb_sha256" \
  "kernel source package"

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
  --load \
  --tag "$image" \
  --file packaging/Dockerfile.kernel \
  .

build_dir="$(mktemp -d "${TMPDIR:-/tmp}/planeradar-kernel-build.XXXXXX")"
trap 'rm -rf "$build_dir"' EXIT
mkdir "$build_dir/source"
git archive HEAD \
  kernel \
  scripts/hyperpixel-build-common.sh \
  scripts/prepare-kbuild-host-tools.sh |
  tar -x -C "$build_dir/source"
cp -R "$build_dir/source/kernel" "$build_dir/kernel"
mkdir "$build_dir/out"

build_command="make -C /usr/src/linux-headers-${release} M=/build/kernel ARCH=arm64 CROSS_COMPILE=aarch64-linux-gnu- W=1 modules"
overlay_revision="${source_revision:0:12}"
[[ "$overlay_revision" =~ ^[0-9a-f]{12}$ ]] || {
  echo "source revision does not have a 12-character hexadecimal prefix" >&2
  exit 1
}
overlay_file="planeradar-hyperpixel2r-${overlay_revision}.dtbo"
overlay_applied_dtb="planeradar-hyperpixel2r-applied.dtb"
docker run --rm \
  --volume "$build_dir:/build" \
  --volume "$build_dir/source:/workspace:ro" \
  --volume "$target_dir:/target-export:ro" \
  "$image" \
  /workspace/scripts/prepare-kbuild-host-tools.sh \
    "/target-export/$kernel_source_deb" \
    "$kernel_source_deb_sha256" \
    "$kernel_source_deb_package" \
    "$kernel_source_version" \
    "/target-export/root$header_path/.config" \
    "/target-export/root$kbuild_path" \
    /build/kbuild \
    /build/host-build

host_tools_file="$build_dir/host-build/host-tools.txt"
test -f "$host_tools_file"
host_tool_value() {
  awk -F '\t' -v wanted="$1" '$1 == wanted { print $2 }' "$host_tools_file"
}
build_host_arch="$(host_tool_value host_arch)"
host_fixdep_sha256="$(host_tool_value host_fixdep_sha256)"
host_modpost_sha256="$(host_tool_value host_modpost_sha256)"
host_genksyms_sha256="$(host_tool_value host_genksyms_sha256)"
case "$build_host_arch" in
  aarch64|arm64|x86_64|amd64) ;;
  *)
    echo "unsupported build host architecture: $build_host_arch" >&2
    exit 1
    ;;
esac
for checksum in \
  "$host_fixdep_sha256" \
  "$host_modpost_sha256" \
  "$host_genksyms_sha256"
do
  [[ "$checksum" =~ ^[0-9a-f]{64}$ ]]
done

docker run --rm \
  --volume "$build_dir:/build" \
  --volume "$build_dir/source:/workspace:ro" \
  --volume "$root_dir:/target-root:ro" \
  --volume "$root_dir/usr/src:/usr/src:ro" \
  --volume "$build_dir/kbuild:$kbuild_path:ro" \
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
    aarch64-linux-gnu-gcc-14 \
      -E \
      -nostdinc \
      -undef \
      -D__DTS__ \
      -x assembler-with-cpp \
      -I"/target-root$3/include" \
      /workspace/kernel/planeradar-hyperpixel2r-overlay.dts \
      -o /build/planeradar-hyperpixel2r-overlay.preprocessed.dts
    if ! dtc \
      -@ \
      -I dts \
      -O dtb \
      -o "/build/out/$2" \
      /build/planeradar-hyperpixel2r-overlay.preprocessed.dts \
      2>/build/overlay-dtc.stderr
    then
      cat /build/overlay-dtc.stderr >&2
      exit 1
    fi
    if test -s /build/overlay-dtc.stderr; then
      echo "overlay compilation emitted warnings:" >&2
      cat /build/overlay-dtc.stderr >&2
      exit 1
    fi
    fdtoverlay \
      -i "/target-root$4" \
      -o /build/out/planeradar-hyperpixel2r-applied.dtb \
      "/build/out/$2"
  ' sh "$release" "$overlay_file" "$common_header_path" "$base_dtb_path"

bash "$script_dir/validate-hyperpixel-overlay.sh" \
  "$build_dir/out/$overlay_file"
hp2r_require_clean_source
test "$(git rev-parse HEAD)" = "$source_revision" || {
  echo "source revision changed while building artifacts" >&2
  exit 1
}
test "$(git rev-parse 'HEAD^{tree}')" = "$source_tree" || {
  echo "source tree changed while building artifacts" >&2
  exit 1
}

output_dir="$(hp2r_release_path "$output_parent" "$release")"
mkdir -p "$output_dir"
rm -f \
  "$output_dir/planeradar_hyperpixel2r.ko" \
  "$output_dir"/planeradar-hyperpixel2r-*.dtbo \
  "$output_dir/planeradar-hyperpixel2r-applied.dtb" \
  "$output_dir/manifest.txt" \
  "$output_dir/module.file.txt" \
  "$output_dir/module.readelf.txt" \
  "$output_dir/module.modinfo.txt" \
  "$output_dir/module.sha256" \
  "$output_dir/host-fixdep" \
  "$output_dir/host-modpost" \
  "$output_dir/host-genksyms"
cp \
  "$build_dir/kernel/planeradar_hyperpixel2r.ko" \
  "$build_dir/kernel/module.file.txt" \
  "$build_dir/kernel/module.readelf.txt" \
  "$build_dir/kernel/module.modinfo.txt" \
  "$build_dir/kernel/module.sha256" \
  "$build_dir/out/$overlay_file" \
  "$build_dir/out/$overlay_applied_dtb" \
  "$output_dir/"
install -m 0755 \
  "$build_dir/kbuild/scripts/basic/fixdep" \
  "$output_dir/host-fixdep"
install -m 0755 \
  "$build_dir/kbuild/scripts/mod/modpost" \
  "$output_dir/host-modpost"
install -m 0755 \
  "$build_dir/kbuild/scripts/genksyms/genksyms" \
  "$output_dir/host-genksyms"

module_sha256="$(awk '{ print $1 }' "$output_dir/module.sha256")"
overlay_sha256="$(shasum -a 256 "$output_dir/$overlay_file" | awk '{ print $1 }')"
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
  printf 'source_tree\t%s\n' "$source_tree"
  printf 'source_dirty\tfalse\n'
  printf 'kernel_release\t%s\n' "$release"
  printf 'kernel_arch\taarch64\n'
  printf 'build_image\t%s\n' "$image"
  printf 'build_command\t%s\n' "$build_command"
  printf 'build_host_arch\t%s\n' "$build_host_arch"
  printf 'kernel_source_package\t%s\n' "$kernel_source_package"
  printf 'kernel_source_version\t%s\n' "$kernel_source_version"
  printf 'kernel_source_deb_package\t%s\n' "$kernel_source_deb_package"
  printf 'kernel_source_deb_sha256\t%s\n' "$kernel_source_deb_sha256"
  printf 'host_fixdep_sha256\t%s\n' "$host_fixdep_sha256"
  printf 'host_modpost_sha256\t%s\n' "$host_modpost_sha256"
  printf 'host_genksyms_sha256\t%s\n' "$host_genksyms_sha256"
  printf 'base_dtb_sha256\t%s\n' "$base_dtb_sha256"
  printf 'overlay_file\t%s\n' "$overlay_file"
  printf 'overlay_sha256\t%s\n' "$overlay_sha256"
  printf 'overlay_applied_dtb\t%s\n' "$overlay_applied_dtb"
  printf 'module_file\tplaneradar_hyperpixel2r.ko\n'
  printf 'module_sha256\t%s\n' "$module_sha256"
  printf 'module_vermagic\t%s\n' "$module_vermagic"
  printf 'module_license\t%s\n' "$module_license"
} > "$output_dir/manifest.txt"

printf 'Built HyperPixel driver bundle at %s\n' "$output_dir"

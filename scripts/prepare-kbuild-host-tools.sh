#!/usr/bin/env bash
set -euo pipefail

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
source "$script_dir/hyperpixel-build-common.sh"

if test "$#" -ne 8; then
  echo "usage: $0 SOURCE_DEB SOURCE_SHA256 SOURCE_PACKAGE SOURCE_VERSION TARGET_CONFIG EXPORTED_KBUILD OUTPUT_KBUILD WORK_DIR" >&2
  exit 64
fi

source_deb="$1"
source_sha256="$2"
source_package="$3"
source_version="$4"
target_config="$5"
exported_kbuild="$6"
output_kbuild="$7"
work_dir="$8"
hostcc="${HOSTCC:-gcc}"
cross_compile="${CROSS_COMPILE:-aarch64-linux-gnu-}"

test -f "$source_deb" || {
  echo "missing matching kernel source package: $source_deb" >&2
  exit 1
}
[[ "$source_sha256" =~ ^[0-9a-f]{64}$ ]] || {
  echo "kernel source package checksum is not SHA-256" >&2
  exit 1
}
test -f "$target_config" || {
  echo "missing exported target kernel config: $target_config" >&2
  exit 1
}
test -d "$exported_kbuild" || {
  echo "missing exported target kbuild tree: $exported_kbuild" >&2
  exit 1
}
for destination in "$output_kbuild" "$work_dir"; do
  test ! -e "$destination" || {
    echo "host-tool destination must not already exist: $destination" >&2
    exit 1
  }
done

command -v dpkg-deb >/dev/null 2>&1 || {
  echo "missing host-tool prerequisite: dpkg-deb" >&2
  exit 1
}
hp2r_verify_sha256 "$source_deb" "$source_sha256" "kernel source package"
actual_package="$(dpkg-deb -f "$source_deb" Package)"
actual_version="$(dpkg-deb -f "$source_deb" Version)"
actual_architecture="$(dpkg-deb -f "$source_deb" Architecture)"
test "$actual_package" = "$source_package" || {
  echo "source package name does not match target metadata" >&2
  exit 1
}
test "$actual_version" = "$source_version" || {
  echo "source package version does not match target metadata" >&2
  exit 1
}
test "$actual_architecture" = all || {
  echo "kernel source package must be architecture-independent" >&2
  exit 1
}
command -v "$hostcc" >/dev/null 2>&1 || {
  echo "missing host C compiler: $hostcc" >&2
  exit 1
}
for prerequisite in make readelf tar; do
  command -v "$prerequisite" >/dev/null 2>&1 || {
    echo "missing host-tool prerequisite: $prerequisite" >&2
    exit 1
  }
done

mkdir "$work_dir"
package_root="$work_dir/package"
source_root="$work_dir/source"
host_output="$work_dir/output"
mkdir "$package_root" "$source_root" "$host_output"
dpkg-deb -x "$source_deb" "$package_root"
source_archive="$package_root/usr/src/$source_package.tar.xz"
test -f "$source_archive" || {
  echo "matching kernel source archive is missing from source package" >&2
  exit 1
}
tar -xJf "$source_archive" -C "$source_root" --strip-components=1
test -f "$source_root/Makefile" || {
  echo "kernel source package did not contain a buildable source root" >&2
  exit 1
}

cp "$target_config" "$host_output/.config"
make -s -C "$source_root" \
  O="$host_output" \
  ARCH=arm64 \
  CROSS_COMPILE="$cross_compile" \
  HOSTCC="$hostcc" \
  olddefconfig
make -s -C "$source_root" \
  O="$host_output" \
  ARCH=arm64 \
  CROSS_COMPILE="$cross_compile" \
  HOSTCC="$hostcc" \
  prepare

case "$(uname -m)" in
  arm64|aarch64) expected_machine=AArch64 ;;
  x86_64|amd64) expected_machine="Advanced Micro Devices X86-64" ;;
  *)
    echo "unsupported build host architecture: $(uname -m)" >&2
    exit 1
    ;;
esac

helpers=(
  scripts/basic/fixdep
  scripts/mod/modpost
  scripts/genksyms/genksyms
)
for helper in "${helpers[@]}"; do
  helper_path="$host_output/$helper"
  case "$helper" in
    scripts/basic/fixdep) expected_status=1 ;;
    *) expected_status=0 ;;
  esac
  hp2r_validate_host_helper \
    "$helper_path" \
    "$expected_machine" \
    "$expected_status"
done

cp -a "$exported_kbuild" "$output_kbuild"
for helper in "${helpers[@]}"; do
  install -m 0755 "$host_output/$helper" "$output_kbuild/$helper"
  test "$(hp2r_sha256 "$output_kbuild/$helper")" = \
    "$(hp2r_sha256 "$host_output/$helper")" || {
    echo "copied host helper does not match fresh build: $helper" >&2
    exit 1
  }
done

{
  printf 'host_arch\t%s\n' "$(uname -m)"
  printf 'kernel_source_package\t%s\n' "$source_package"
  printf 'kernel_source_version\t%s\n' "$source_version"
  printf 'kernel_source_sha256\t%s\n' "$source_sha256"
  for helper in "${helpers[@]}"; do
    key="${helper##*/}"
    printf 'host_%s_sha256\t%s\n' \
      "$key" \
      "$(hp2r_sha256 "$output_kbuild/$helper")"
  done
} > "$work_dir/host-tools.txt"

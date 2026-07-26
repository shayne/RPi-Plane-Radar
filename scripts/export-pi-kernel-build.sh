#!/usr/bin/env bash
set -euo pipefail

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
source "$script_dir/hyperpixel-build-common.sh"

target="${PLANERADAR_PI_TARGET:-shayne@planeradar.local}"
base_dtb_path="/boot/firmware/bcm2710-rpi-zero-2-w.dtb"

metadata="$(
  ssh "$target" bash -s <<'REMOTE'
set -eu
release="$(uname -r)"
arch="$(uname -m)"
test "$arch" = aarch64 || {
  echo "target architecture must be aarch64, got $arch" >&2
  exit 1
}
header_path="$(readlink -f "/lib/modules/$release/build")"
test -d "$header_path"
test -f "$header_path/.config"
test -f "$header_path/Module.symvers"
common_makefile="$(
  awk '$1 == "include" && $2 ~ /^\// { print $2; exit }' \
    "$header_path/Makefile"
)"
test -f "$common_makefile"
common_header_path="$(dirname "$common_makefile")"
test -d "$common_header_path"
scripts_path="$(readlink -f "$header_path/scripts")"
test -d "$scripts_path"
kbuild_path="$(dirname "$scripts_path")"
test -d "$kbuild_path"
kbuild_package="$(
  dpkg-query -S "$kbuild_path/scripts/basic/fixdep" |
    awk -F ': ' 'NR == 1 { print $1 }'
)"
test -n "$kbuild_package"
kernel_source_package="$(
  dpkg-query -W -f='${source:Package}' "$kbuild_package"
)"
kernel_source_version="$(
  dpkg-query -W -f='${source:Version}' "$kbuild_package"
)"
kernel_series="$(
  printf '%s\n' "$release" |
    sed -nE 's/^([0-9]+\.[0-9]+).*/\1/p'
)"
test -n "$kernel_series"
kernel_source_deb_package="linux-source-$kernel_series"
kernel_source_deb_metadata="$(
  apt-cache show "$kernel_source_deb_package=$kernel_source_version"
)"
kernel_source_deb_arch="$(
  printf '%s\n' "$kernel_source_deb_metadata" |
    awk '$1 == "Architecture:" { print $2; exit }'
)"
kernel_source_deb_filename="$(
  printf '%s\n' "$kernel_source_deb_metadata" |
    awk '$1 == "Filename:" { print $2; exit }'
)"
kernel_source_deb_sha256="$(
  printf '%s\n' "$kernel_source_deb_metadata" |
    awk '$1 == "SHA256:" { print $2; exit }'
)"
test "$kernel_source_package" = linux
test "$kernel_source_deb_arch" = all
test -n "$kernel_source_deb_filename"
test -n "$kernel_source_deb_sha256"
base_dtb_path=/boot/firmware/bcm2710-rpi-zero-2-w.dtb
test -f "$base_dtb_path"
base_dtb_sha256="$(sha256sum "$base_dtb_path" | awk '{ print $1 }')"
printf "kernel_release\t%s\n" "$release"
printf "kernel_arch\t%s\n" "$arch"
printf "header_path\t%s\n" "$header_path"
printf "common_header_path\t%s\n" "$common_header_path"
printf "kbuild_path\t%s\n" "$kbuild_path"
printf "kernel_source_package\t%s\n" "$kernel_source_package"
printf "kernel_source_version\t%s\n" "$kernel_source_version"
printf "kernel_source_deb_package\t%s\n" "$kernel_source_deb_package"
printf "kernel_source_deb_filename\t%s\n" "$kernel_source_deb_filename"
printf "kernel_source_deb_sha256\t%s\n" "$kernel_source_deb_sha256"
printf "base_dtb_path\t%s\n" "$base_dtb_path"
printf "base_dtb_sha256\t%s\n" "$base_dtb_sha256"
REMOTE
)"

release=""
arch=""
header_path=""
common_header_path=""
kbuild_path=""
kernel_source_package=""
kernel_source_version=""
kernel_source_deb_package=""
kernel_source_deb_filename=""
kernel_source_deb_sha256=""
base_dtb_sha256=""
while IFS=$'\t' read -r key value; do
  case "$key" in
    kernel_release) release="$value" ;;
    kernel_arch) arch="$value" ;;
    header_path) header_path="$value" ;;
    common_header_path) common_header_path="$value" ;;
    kbuild_path) kbuild_path="$value" ;;
    kernel_source_package) kernel_source_package="$value" ;;
    kernel_source_version) kernel_source_version="$value" ;;
    kernel_source_deb_package) kernel_source_deb_package="$value" ;;
    kernel_source_deb_filename) kernel_source_deb_filename="$value" ;;
    kernel_source_deb_sha256) kernel_source_deb_sha256="$value" ;;
    base_dtb_path)
      test "$value" = "$base_dtb_path" || {
        echo "unexpected target DTB path: $value" >&2
        exit 1
      }
      ;;
    base_dtb_sha256) base_dtb_sha256="$value" ;;
    *)
      echo "unexpected target metadata key: $key" >&2
      exit 1
      ;;
  esac
done <<< "$metadata"

test "$arch" = aarch64
test -n "$release"
test -n "$header_path"
test -n "$common_header_path"
test -n "$kbuild_path"
test "$kernel_source_package" = linux
test -n "$kernel_source_version"
[[ "$kernel_source_deb_package" =~ ^linux-source-[0-9]+\.[0-9]+$ ]]
case "$kernel_source_deb_filename" in
  pool/*)
    case "$kernel_source_deb_filename" in
      *".."*|*[$'\t\r\n ']*)
        echo "unsafe kernel source package filename: $kernel_source_deb_filename" >&2
        exit 1
        ;;
    esac
    ;;
  *)
    echo "unexpected kernel source package filename: $kernel_source_deb_filename" >&2
    exit 1
    ;;
esac
[[ "$kernel_source_deb_sha256" =~ ^[0-9a-f]{64}$ ]]
test -n "$base_dtb_sha256"
hp2r_validate_release "$release"

export_parent="dist/kernel-target"
mkdir -p "$export_parent"
export_dir="$(hp2r_release_path "$export_parent" "$release")"
target_file="$export_dir/target.txt"
if test -f "$target_file"; then
  existing_release="$(awk -F '\t' '$1 == "kernel_release" { print $2 }' "$target_file")"
  existing_dtb_sha256="$(awk -F '\t' '$1 == "base_dtb_sha256" { print $2 }' "$target_file")"
  existing_source_version="$(
    awk -F '\t' '$1 == "kernel_source_version" { print $2 }' "$target_file"
  )"
  existing_source_sha256="$(
    awk -F '\t' '$1 == "kernel_source_deb_sha256" { print $2 }' "$target_file"
  )"
  test "$existing_release" = "$release" || {
    echo "existing export release does not match live target" >&2
    exit 1
  }
  test "$existing_dtb_sha256" = "$base_dtb_sha256" || {
    echo "existing export base DTB does not match live target" >&2
    exit 1
  }
  if test -n "$existing_source_version"; then
    test "$existing_source_version" = "$kernel_source_version" || {
      echo "existing export source version does not match live target" >&2
      exit 1
    }
  fi
  if test -n "$existing_source_sha256"; then
    test "$existing_source_sha256" = "$kernel_source_deb_sha256" || {
      echo "existing export source package does not match live target" >&2
      exit 1
    }
  fi
fi

temporary_dir="$(mktemp -d "$export_parent/.${release}.XXXXXX")"
trap 'rm -rf "$temporary_dir"' EXIT
mkdir -p "$temporary_dir/root"

ssh "$target" \
  "tar -C / -cf - '${header_path#/}' '${common_header_path#/}' '${kbuild_path#/}' '${base_dtb_path#/}'" \
  | tar -C "$temporary_dir/root" -xf -

test -f "$temporary_dir/root$header_path/.config"
test -f "$temporary_dir/root$header_path/Module.symvers"
test -d "$temporary_dir/root$common_header_path"
test -d "$temporary_dir/root$kbuild_path"
test -f "$temporary_dir/root$base_dtb_path"
hp2r_verify_sha256 \
  "$temporary_dir/root$base_dtb_path" \
  "$base_dtb_sha256"
command -v curl >/dev/null 2>&1 || {
  echo "curl is required to fetch the exact target kernel source package" >&2
  exit 1
}
kernel_source_base_url="$(
  printf '%s' \
    "${PLANERADAR_KERNEL_SOURCE_BASE_URL:-https://archive.raspberrypi.com/debian}" |
    sed 's:/*$::'
)"
kernel_source_deb="$temporary_dir/kernel-source.deb"
curl \
  --fail \
  --location \
  --retry 2 \
  --output "$kernel_source_deb" \
  "$kernel_source_base_url/$kernel_source_deb_filename"
hp2r_verify_sha256 \
  "$kernel_source_deb" \
  "$kernel_source_deb_sha256" \
  "kernel source package"

{
  printf 'kernel_release\t%s\n' "$release"
  printf 'kernel_arch\taarch64\n'
  printf 'header_path\t%s\n' "$header_path"
  printf 'common_header_path\t%s\n' "$common_header_path"
  printf 'kbuild_path\t%s\n' "$kbuild_path"
  printf 'kernel_source_package\t%s\n' "$kernel_source_package"
  printf 'kernel_source_version\t%s\n' "$kernel_source_version"
  printf 'kernel_source_deb_package\t%s\n' "$kernel_source_deb_package"
  printf 'kernel_source_deb_filename\t%s\n' "$kernel_source_deb_filename"
  printf 'kernel_source_deb_sha256\t%s\n' "$kernel_source_deb_sha256"
  printf 'kernel_source_deb\tkernel-source.deb\n'
  printf 'base_dtb_path\t%s\n' "$base_dtb_path"
  printf 'base_dtb_sha256\t%s\n' "$base_dtb_sha256"
} > "$temporary_dir/target.txt"

if test -e "$export_dir"; then
  rm -rf "$export_dir"
fi
mv "$temporary_dir" "$export_dir"
trap - EXIT
printf 'Exported %s kernel build inputs to %s\n' "$release" "$export_dir"

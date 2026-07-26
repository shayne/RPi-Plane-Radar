#!/usr/bin/env bash

hp2r_validate_release() {
  local release="${1-}"

  case "$release" in
    ""|"."|".."|*[!A-Za-z0-9._+-]*)
      echo "unsafe kernel release returned by target: $release" >&2
      return 1
      ;;
  esac
}

hp2r_release_path() {
  local parent="$1"
  local release="$2"
  local parent_path
  local release_path

  hp2r_validate_release "$release" || return
  parent_path="$(cd "$parent" && pwd -P)" || return
  release_path="$parent_path/$release"
  # A release destination must be absent or a direct, real directory.
  # Symlinks are rejected even when their current target is inside the parent.
  if test -L "$release_path"; then
    echo "symlinked kernel release destination is unsafe: $release_path" >&2
    return 1
  fi
  if test -e "$release_path"; then
    test -d "$release_path" || {
      echo "kernel release destination is not a directory: $release_path" >&2
      return 1
    }
    release_path="$(cd "$release_path" && pwd -P)" || return
  fi
  case "$release_path" in
    "$parent_path"/*) ;;
    *)
      echo "kernel release path escapes fixed parent: $release_path" >&2
      return 1
      ;;
  esac
  printf '%s\n' "$release_path"
}

hp2r_verify_sha256() {
  local file="$1"
  local expected="$2"
  local actual

  if command -v sha256sum >/dev/null 2>&1; then
    actual="$(sha256sum "$file" | awk '{ print $1 }')"
  else
    actual="$(shasum -a 256 "$file" | awk '{ print $1 }')"
  fi
  test "$actual" = "$expected" || {
    echo "base DTB checksum mismatch for $file" >&2
    return 1
  }
}

hp2r_validate_host_helper() {
  local helper="$1"
  local expected_machine="$2"
  local expected_status="$3"
  local machine
  local helper_status

  test -f "$helper" -a -x "$helper" || {
    echo "host helper is missing or not executable: $helper" >&2
    return 1
  }
  command -v readelf >/dev/null 2>&1 || {
    echo "missing host-tool prerequisite: readelf" >&2
    return 1
  }
  machine="$(
    readelf -h "$helper" |
      awk -F ': *' '$1 ~ /^[[:space:]]*Machine$/ { print $2; exit }'
  )"
  test "$machine" = "$expected_machine" || {
    echo "host helper has the wrong architecture: $helper ($machine)" >&2
    return 1
  }
  set +e
  "$helper" </dev/null >/dev/null 2>&1
  helper_status="$?"
  set -e
  test "$helper_status" -eq "$expected_status" || {
    echo "host helper is not executable in the build container: $helper" >&2
    return 1
  }
}

hp2r_manifest_value() {
  local manifest="$1"
  local key="$2"

  awk -F '\t' -v wanted="$key" '$1 == wanted { print $2 }' "$manifest"
}

hp2r_validate_artifact_provenance() {
  local manifest="$1"
  local target_manifest="$2"
  local artifact_dir="$3"
  local required_keys=(
    source_revision
    source_tree
    source_dirty
    kernel_release
    kernel_arch
    build_image
    build_command
    build_host_arch
    kernel_source_package
    kernel_source_version
    kernel_source_deb_package
    kernel_source_deb_sha256
    host_fixdep_sha256
    host_modpost_sha256
    host_genksyms_sha256
    base_dtb_sha256
    overlay_file
    overlay_sha256
    overlay_applied_dtb
    module_file
    module_sha256
    module_vermagic
    module_license
  )
  local key
  local count
  local value
  local target_value
  local helper
  local checksum_key
  local actual_checksum

  test -f "$manifest" -a -f "$target_manifest" || {
    echo "artifact provenance manifest is missing" >&2
    return 1
  }
  test "$(awk 'END { print NR }' "$manifest")" -eq "${#required_keys[@]}" || {
    echo "artifact manifest schema has the wrong cardinality" >&2
    return 1
  }
  awk -F '\t' 'NF != 2 || $1 == "" || $2 == "" { exit 1 }' "$manifest" || {
    echo "artifact manifest schema has an invalid row" >&2
    return 1
  }
  for key in "${required_keys[@]}"; do
    count="$(awk -F '\t' -v wanted="$key" '$1 == wanted { count++ } END { print count + 0 }' "$manifest")"
    test "$count" -eq 1 || {
      echo "artifact manifest schema requires exactly one $key" >&2
      return 1
    }
  done

  value="$(hp2r_manifest_value "$manifest" build_host_arch)"
  case "$value" in
    aarch64|arm64|x86_64|amd64) ;;
    *)
      echo "artifact build host architecture is unsupported: $value" >&2
      return 1
      ;;
  esac
  test "$(hp2r_manifest_value "$manifest" kernel_arch)" = aarch64 || {
    echo "artifact target kernel architecture must be aarch64" >&2
    return 1
  }
  test "$(hp2r_manifest_value "$manifest" kernel_source_package)" = linux || {
    echo "artifact kernel source package must be linux" >&2
    return 1
  }
  value="$(hp2r_manifest_value "$manifest" kernel_source_version)"
  [[ "$value" =~ ^[A-Za-z0-9.+:~_-]+$ ]] || {
    echo "artifact kernel source version has an invalid format" >&2
    return 1
  }
  value="$(hp2r_manifest_value "$manifest" kernel_source_deb_package)"
  [[ "$value" =~ ^linux-source-[0-9]+\.[0-9]+$ ]] || {
    echo "artifact kernel source package name has an invalid format" >&2
    return 1
  }
  for key in \
    kernel_source_deb_sha256 \
    host_fixdep_sha256 \
    host_modpost_sha256 \
    host_genksyms_sha256
  do
    value="$(hp2r_manifest_value "$manifest" "$key")"
    [[ "$value" =~ ^[0-9a-f]{64}$ ]] || {
      echo "artifact $key is not lowercase SHA-256" >&2
      return 1
    }
  done

  for key in \
    kernel_release \
    kernel_arch \
    kernel_source_package \
    kernel_source_version \
    kernel_source_deb_package \
    kernel_source_deb_sha256 \
    base_dtb_sha256
  do
    value="$(hp2r_manifest_value "$manifest" "$key")"
    target_value="$(hp2r_manifest_value "$target_manifest" "$key")"
    test -n "$target_value" -a "$value" = "$target_value" || {
      echo "artifact $key does not match target export" >&2
      return 1
    }
  done

  for specification in \
    host-fixdep:host_fixdep_sha256 \
    host-modpost:host_modpost_sha256 \
    host-genksyms:host_genksyms_sha256
  do
    helper="${specification%%:*}"
    checksum_key="${specification#*:}"
    test -f "$artifact_dir/$helper" || {
      echo "missing host helper evidence: $artifact_dir/$helper" >&2
      return 1
    }
    actual_checksum="$(hp2r_sha256 "$artifact_dir/$helper")" || return
    test "$actual_checksum" = \
      "$(hp2r_manifest_value "$manifest" "$checksum_key")" || {
      echo "host helper checksum does not match manifest: $helper" >&2
      return 1
    }
  done

  value="$(hp2r_manifest_value "$manifest" module_vermagic)"
  case "$value" in
    "$(hp2r_manifest_value "$manifest" kernel_release)"*) ;;
    *)
      echo "artifact module vermagic does not match kernel release" >&2
      return 1
      ;;
  esac
}

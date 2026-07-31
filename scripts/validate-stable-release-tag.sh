#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

usage() {
  cat <<'USAGE'
Usage: validate-stable-release-tag.sh TAG SOURCE-REVISION

Require a canonical stable vMAJOR.MINOR.PATCH tag whose semantic version
exactly matches the workspace package version in the selected durable source.
USAGE
}

if test "$#" -ne 2; then
  usage >&2
  exit 64
fi

tag="$1"
source_ref="$2"
if [[ ! "$tag" =~ ^v([0-9]+\.[0-9]+\.[0-9]+)$ ]]; then
  printf 'stable release tag must be vMAJOR.MINOR.PATCH: %s\n' "$tag" >&2
  exit 1
fi
tag_version="${BASH_REMATCH[1]}"

source_revision="$(git -C "$repo_root" rev-parse --verify "$source_ref^{commit}")"
package_version="$({
  git -C "$repo_root" show "$source_revision:Cargo.toml" |
    sed -n '/^\[package\]$/,/^\[/s/^version = "\([^"]*\)"/\1/p' |
    head -1
})"
if [[ ! "$package_version" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]]; then
  printf 'selected source does not declare a stable semantic package version\n' >&2
  exit 1
fi
if test "$tag_version" != "$package_version"; then
  printf 'stable release tag version %s does not match source package version %s\n' \
    "$tag_version" "$package_version" >&2
  exit 1
fi

printf '%s\n' "$source_revision"

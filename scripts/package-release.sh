#!/usr/bin/env bash
set -euo pipefail

readonly REPOSITORY_URL="https://github.com/shayne/RPi-Plane-Radar"
readonly WORKFLOW_REPOSITORY="shayne/RPi-Plane-Radar"
readonly CANDIDATE_WORKFLOW_PATH=".github/workflows/release.yml"
readonly STABLE_WORKFLOW_PATH=".github/workflows/stable-draft.yml"
readonly APPLICATION_ARCHIVE="planeradar-aarch64-linux-gnu.tar.zst"
readonly CONTROL_ARM64_ARCHIVE="planeradarctl-aarch64-apple-darwin.tar.zst"
readonly CONTROL_X86_64_ARCHIVE="planeradarctl-x86_64-apple-darwin.tar.zst"
readonly ARCHIVE_IMAGE="rust:1.97.1-trixie"@"sha256:1bcff4befb740599103a2c7cb51058e14479b2e35e3a34a3f0dc4ede09927488"
readonly DEBIAN_SNAPSHOT="https://snapshot.debian.org/archive/debian/20260701T000000Z"
readonly RELEASE_FILES=(
  "$APPLICATION_ARCHIVE"
  "$CONTROL_ARM64_ARCHIVE"
  "$CONTROL_X86_64_ARCHIVE"
  "install.sh"
  "release-manifest.json"
  "SHA256SUMS"
  "SBOM.spdx.json"
)

die() {
  printf 'package-release: %s\n' "$*" >&2
  exit 1
}

require_command() {
  command -v "$1" >/dev/null 2>&1 || die "required command is unavailable: $1"
}

reject_unexpected_release_files() {
  local directory=$1
  local actual expected
  actual="$(find "$directory" -mindepth 1 -maxdepth 1 -type f -exec basename {} \; | LC_ALL=C sort)"
  expected="$(printf '%s\n' "${RELEASE_FILES[@]}" | LC_ALL=C sort)"
  [[ "$actual" == "$expected" ]] || die "release directory contains missing or unexpected files"
  [[ "$(find "$directory" -mindepth 1 -maxdepth 1 ! -type f -print -quit)" == "" ]] ||
    die "release directory contains a non-regular entry"
}

canonical_version() {
  local value=$1
  [[ "$value" =~ ^(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)(-([0-9A-Za-z-]+)(\.[0-9A-Za-z-]+)*)?(\+([0-9A-Za-z-]+)(\.[0-9A-Za-z-]+)*)?$ ]] ||
    die "version must be canonical semantic version text"
  printf '%s\n' "$value"
}

sha256_file() {
  shasum -a 256 "$1" | awk '{print $1}'
}

file_size() {
  if stat -f '%z' "$1" >/dev/null 2>&1; then
    stat -f '%z' "$1"
  else
    stat -c '%s' "$1"
  fi
}

verify_binary_architecture() {
  local path=$1 expected=$2 description
  description="$(file -b "$path")"
  case "$expected" in
    linux-aarch64)
      [[ "$description" == *"ELF 64-bit LSB"* && "$description" == *"ARM aarch64"* ]] ||
        die "application is not a real ELF 64-bit LSB ARM aarch64 executable"
      ;;
    darwin-arm64)
      [[ "$description" == *"Mach-O 64-bit"* && "$description" == *"arm64"* ]] ||
        die "control binary is not a Mach-O 64-bit arm64 executable"
      [[ "$(lipo -archs "$path")" == "arm64" ]] ||
        die "control binary has a mislabeled or universal architecture"
      ;;
    darwin-x86_64)
      [[ "$description" == *"Mach-O 64-bit"* && "$description" == *"x86_64"* ]] ||
        die "control binary is not a Mach-O 64-bit x86_64 executable"
      [[ "$(lipo -archs "$path")" == "x86_64" ]] ||
        die "control binary has a mislabeled or universal architecture"
      ;;
    *)
      die "internal unsupported architecture check"
      ;;
  esac
}

make_normalized_archive() {
  local input=$1 member=$2 output=$3
  local stage
  stage="$(mktemp -d "${TMPDIR:-/tmp}/planeradar-archive.XXXXXX")"
  trap 'rm -rf -- "$stage"' RETURN
  install -m 0755 "$input" "$stage/$member"
  docker run --rm --platform linux/arm64 \
    --volume "$stage:/input:ro" \
    --volume "$(dirname "$output"):/output" \
    --env "SOURCE_DATE_EPOCH=$SOURCE_DATE_EPOCH" \
    --env "DEBIAN_SNAPSHOT=$DEBIAN_SNAPSHOT" \
    "$ARCHIVE_IMAGE" \
    bash -o pipefail -ceu '
      rm -f /etc/apt/sources.list /etc/apt/sources.list.d/*
      printf "%s\n" \
        "Types: deb" \
        "URIs: $DEBIAN_SNAPSHOT" \
        "Suites: trixie" \
        "Components: main" \
        "Signed-By: /usr/share/keyrings/debian-archive-keyring.gpg" \
        > /etc/apt/sources.list.d/snapshot.sources
      apt-get -o Acquire::Check-Valid-Until=false update >/dev/null
      apt-get install -y --no-install-recommends zstd >/dev/null
      tar --sort=name --format=gnu --owner=0 --group=0 --numeric-owner \
        --mode=0755 --mtime="@$SOURCE_DATE_EPOCH" \
        -cf - -C /input "$1" |
        zstd -19 -T1 --no-progress -o "/output/$2"
    ' -- "$member" "$(basename "$output")"
  rm -rf -- "$stage"
  trap - RETURN
}

version="${1:-}"
[[ $# -eq 1 ]] || die "usage: scripts/package-release.sh VERSION"
version="$(canonical_version "$version")"
if [[ "$version" == *-* ]]; then
  workflow_path="$CANDIDATE_WORKFLOW_PATH"
else
  workflow_path="$STABLE_WORKFLOW_PATH"
fi

require_command git
repo_root="$(git rev-parse --show-toplevel 2>/dev/null)" ||
  die "run this command from a Plane Radar clone"
cd "$repo_root"

[[ -z "$(git status --porcelain --untracked-files=all)" ]] ||
  die "release packaging requires clean tracked source and no untracked files"
git diff-index --quiet HEAD -- ||
  die "release packaging requires clean tracked source"

start_head="$(git rev-parse HEAD)"
start_tree="$(git rev-parse 'HEAD^{tree}')"
source_ref="${PLANERADAR_SOURCE_REF:-HEAD}"
source_commit="$(git rev-parse --verify "${source_ref}^{commit}" 2>/dev/null)" ||
  die "selected source is not a reachable commit"
git merge-base --is-ancestor "$source_commit" HEAD ||
  die "selected source is not reachable from the current checkout"
[[ "$source_commit" == "$start_head" ]] ||
  die "selected source does not match current checkout"
source_tree="$(git rev-parse --verify "${source_commit}^{tree}")"
source_epoch="$(git show -s --format=%ct "$source_commit")"
export SOURCE_DATE_EPOCH="$source_epoch"
workflow_ref_is_set="${PLANERADAR_WORKFLOW_REF+x}"
workflow_commit_is_set="${PLANERADAR_WORKFLOW_COMMIT+x}"
if [[ "$workflow_ref_is_set" == x || "$workflow_commit_is_set" == x ]]; then
  [[ "$workflow_ref_is_set" == x && "$workflow_commit_is_set" == x ]] ||
    die "PLANERADAR_WORKFLOW_REF and PLANERADAR_WORKFLOW_COMMIT must be set together"
  workflow_ref="$PLANERADAR_WORKFLOW_REF"
  workflow_commit="$PLANERADAR_WORKFLOW_COMMIT"
else
  workflow_ref="$(git symbolic-ref -q HEAD 2>/dev/null)" ||
    die "detached HEAD requires PLANERADAR_WORKFLOW_REF and PLANERADAR_WORKFLOW_COMMIT"
  workflow_commit="$source_commit"
fi
[[ "$workflow_ref" =~ ^refs/(heads|tags)/[A-Za-z0-9]([A-Za-z0-9._/-]*[A-Za-z0-9])?$ &&
   "$workflow_ref" != *".."* &&
   "$workflow_ref" != *"//"* ]] ||
  die "workflow ref is not a safe full GitHub ref"
[[ "$workflow_commit" =~ ^[0-9a-f]{40}$ ]] ||
  die "workflow commit is malformed"
[[ "$workflow_commit" == "$source_commit" ]] ||
  die "workflow invocation commit does not match selected source"
if resolved_workflow_commit="$(git rev-parse --verify "${workflow_ref}^{commit}" 2>/dev/null)"; then
  :
elif [[ "$workflow_ref" == refs/heads/* ]]; then
  remote_workflow_ref="refs/remotes/origin/${workflow_ref#refs/heads/}"
  resolved_workflow_commit="$(git rev-parse --verify "${remote_workflow_ref}^{commit}" 2>/dev/null)" ||
    die "workflow ref is not an attainable commit in this clone"
else
  die "workflow ref is not an attainable commit in this clone"
fi
[[ "$resolved_workflow_commit" == "$workflow_commit" ]] ||
  die "workflow ref does not resolve to workflow commit"

require_command awk
package_version="$(awk -F ' *= *' '
  /^\[package\]$/ { package = 1; next }
  /^\[/ { package = 0 }
  package && $1 == "version" { gsub(/"/, "", $2); print $2; exit }
' Cargo.toml)"
[[ "${version%%-*}" == "$package_version" ]] ||
  die "version does not match Cargo.toml package version"
git cat-file -e "$workflow_commit:$workflow_path" ||
  die "selected source does not contain the required release workflow"

for command in docker cargo file lipo shasum find python3; do
  require_command "$command"
done

work="$(mktemp -d "${TMPDIR:-/tmp}/planeradar-release.XXXXXX")"
cleanup() {
  local status=$?
  rm -rf -- "$work"
  return "$status"
}
trap cleanup EXIT
output="$work/release"
mkdir -p "$output"

if [[ "${PLANERADAR_PACKAGE_SKIP_BUILDS:-0}" != "1" ]]; then
  PLANERADAR_SOURCE_REF="$source_commit" ./scripts/build-pi.sh
  mise exec -- rustup target add aarch64-apple-darwin x86_64-apple-darwin
  PLANERADAR_REVISION="$source_commit" mise exec -- cargo build --locked --release \
    -p planeradarctl --target aarch64-apple-darwin
  PLANERADAR_REVISION="$source_commit" mise exec -- cargo build --locked --release \
    -p planeradarctl --target x86_64-apple-darwin
fi

app_binary="${PLANERADAR_APP_BINARY:-dist/planeradar}"
control_arm64="${PLANERADAR_CTL_ARM64_BINARY:-target/aarch64-apple-darwin/release/planeradarctl}"
control_x86_64="${PLANERADAR_CTL_X86_64_BINARY:-target/x86_64-apple-darwin/release/planeradarctl}"
for binary in "$app_binary" "$control_arm64" "$control_x86_64"; do
  [[ -f "$binary" && ! -L "$binary" && -x "$binary" ]] ||
    die "release input is not a regular executable: $binary"
done
verify_binary_architecture "$app_binary" linux-aarch64
verify_binary_architecture "$control_arm64" darwin-arm64
verify_binary_architecture "$control_x86_64" darwin-x86_64

if [[ -f dist/planeradar.revision ]]; then
  [[ "$(tr -d '\r\n' <dist/planeradar.revision)" == "$source_commit" ]] ||
    die "application revision does not match selected source"
fi
if [[ -f dist/planeradar.tree ]]; then
  [[ "$(tr -d '\r\n' <dist/planeradar.tree)" == "$source_tree" ]] ||
    die "application tree does not match selected source"
fi

make_normalized_archive "$app_binary" planeradar "$output/$APPLICATION_ARCHIVE"
make_normalized_archive "$control_arm64" planeradarctl "$output/$CONTROL_ARM64_ARCHIVE"
make_normalized_archive "$control_x86_64" planeradarctl "$output/$CONTROL_X86_64_ARCHIVE"
install -m 0755 scripts/install.sh "$output/install.sh"

driver_repository="$(awk -F ' *= *' '$1 == "repository" { gsub(/"/, "", $2); print $2 }' driver.lock.toml)"
driver_version="$(awk -F ' *= *' '$1 == "version" { gsub(/"/, "", $2); print $2 }' driver.lock.toml)"
driver_commit="$(awk -F ' *= *' '$1 == "commit" { gsub(/"/, "", $2); print $2 }' driver.lock.toml)"
driver_manifest="$(awk -F ' *= *' '$1 == "manifest_sha256" { gsub(/"/, "", $2); print $2 }' driver.lock.toml)"
driver_protocol="$(awk -F ' *= *' '$1 == "lifecycle_protocol" { gsub(/"/, "", $2); print $2 }' driver.lock.toml)"

export PLANERADAR_PACKAGE_VERSION="$version"
export PLANERADAR_SOURCE_COMMIT="$source_commit"
export PLANERADAR_SOURCE_TREE="$source_tree"
export PLANERADAR_SOURCE_EPOCH="$source_epoch"
export PLANERADAR_WORKFLOW_REF="$workflow_ref"
export PLANERADAR_WORKFLOW_COMMIT="$workflow_commit"
export PLANERADAR_WORKFLOW_PATH="$workflow_path"
export PLANERADAR_RELEASE_OUTPUT="$output"
export PLANERADAR_APP_BINARY="$app_binary"
export PLANERADAR_CTL_ARM64_BINARY="$control_arm64"
export PLANERADAR_CTL_X86_64_BINARY="$control_x86_64"
export PLANERADAR_DRIVER_REPOSITORY="$driver_repository"
export PLANERADAR_DRIVER_VERSION="$driver_version"
export PLANERADAR_DRIVER_COMMIT="$driver_commit"
export PLANERADAR_DRIVER_MANIFEST="$driver_manifest"
export PLANERADAR_DRIVER_PROTOCOL="$driver_protocol"
python3 - <<'PY'
import datetime
import hashlib
import json
import os
import pathlib
import tomllib

out = pathlib.Path(os.environ["PLANERADAR_RELEASE_OUTPUT"])
version = os.environ["PLANERADAR_PACKAGE_VERSION"]
commit = os.environ["PLANERADAR_SOURCE_COMMIT"]
tree = os.environ["PLANERADAR_SOURCE_TREE"]
workflow_ref = os.environ["PLANERADAR_WORKFLOW_REF"]
workflow_commit = os.environ["PLANERADAR_WORKFLOW_COMMIT"]
workflow_path = os.environ["PLANERADAR_WORKFLOW_PATH"]
epoch = int(os.environ["PLANERADAR_SOURCE_EPOCH"])
timestamp = datetime.datetime.fromtimestamp(
    epoch, datetime.timezone.utc
).strftime("%Y-%m-%dT%H:%M:%SZ")

artifacts = [
    ("planeradar-aarch64-linux-gnu.tar.zst", "application", "linux-gnu", "aarch64"),
    ("planeradarctl-aarch64-apple-darwin.tar.zst", "control", "apple-darwin", "aarch64"),
    ("planeradarctl-x86_64-apple-darwin.tar.zst", "control", "apple-darwin", "x86_64"),
]

def digest(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()

def checksums(path):
    contents = path.read_bytes()
    return [
        {"algorithm": "SHA1", "checksumValue": hashlib.sha1(contents).hexdigest()},
        {"algorithm": "SHA256", "checksumValue": hashlib.sha256(contents).hexdigest()},
    ]

lock = tomllib.loads(pathlib.Path("Cargo.lock").read_text())
packages = sorted(
    lock["package"], key=lambda package: (package["name"], package["version"], package.get("source", ""))
)
spdx_packages = []
relationships = []
package_ids = {}
for index, package in enumerate(packages):
    spdx_id = f"SPDXRef-Package-{index}"
    package_ids[(package["name"], package["version"])] = spdx_id
    entry = {
        "SPDXID": spdx_id,
        "name": package["name"],
        "versionInfo": package["version"],
        "downloadLocation": "NOASSERTION",
        "filesAnalyzed": False,
        "licenseConcluded": "NOASSERTION",
        "licenseDeclared": "NOASSERTION",
        "copyrightText": "NOASSERTION",
    }
    if checksum := package.get("checksum"):
        entry["checksums"] = [{"algorithm": "SHA256", "checksumValue": checksum}]
    spdx_packages.append(entry)
    relationships.append({
        "spdxElementId": "SPDXRef-DOCUMENT",
        "relationshipType": "DESCRIBES",
        "relatedSpdxElement": spdx_id,
    })

names = {}
for package in packages:
    names.setdefault(package["name"], []).append(package["version"])
for package in packages:
    source_id = package_ids[(package["name"], package["version"])]
    for dependency in package.get("dependencies", []):
        parts = dependency.rsplit(" ", 1)
        if len(parts) == 2 and (parts[0], parts[1]) in package_ids:
            target_id = package_ids[(parts[0], parts[1])]
        else:
            candidates = names.get(dependency, [])
            if len(candidates) != 1:
                raise SystemExit(f"ambiguous Cargo.lock dependency identity: {dependency}")
            target_id = package_ids[(dependency, candidates[0])]
        relationships.append({
            "spdxElementId": source_id,
            "relationshipType": "DEPENDS_ON",
            "relatedSpdxElement": target_id,
        })

executable_inputs = [
    ("planeradar-aarch64-linux-gnu/planeradar", pathlib.Path(os.environ["PLANERADAR_APP_BINARY"])),
    (
        "planeradarctl-aarch64-apple-darwin/planeradarctl",
        pathlib.Path(os.environ["PLANERADAR_CTL_ARM64_BINARY"]),
    ),
    (
        "planeradarctl-x86_64-apple-darwin/planeradarctl",
        pathlib.Path(os.environ["PLANERADAR_CTL_X86_64_BINARY"]),
    ),
]
spdx_files = []
for index, (name, path) in enumerate(executable_inputs):
    file_id = f"SPDXRef-Executable-{index}"
    spdx_files.append({
        "SPDXID": file_id,
        "fileName": f"./{name}",
        "checksums": checksums(path),
        "fileTypes": ["BINARY"],
        "licenseConcluded": "NOASSERTION",
        "copyrightText": "NOASSERTION",
    })
    relationships.append({
        "spdxElementId": "SPDXRef-Package-ReleaseInputs",
        "relationshipType": "CONTAINS",
        "relatedSpdxElement": file_id,
    })

verification_input = "".join(
    sorted(
        checksum["checksumValue"]
        for item in spdx_files
        for checksum in item["checksums"]
        if checksum["algorithm"] == "SHA1"
    )
).encode()
release_inputs = {
    "SPDXID": "SPDXRef-Package-ReleaseInputs",
    "name": "RPi-Plane-Radar-release-inputs",
    "versionInfo": version,
    "downloadLocation": "NOASSERTION",
    "filesAnalyzed": True,
    "packageVerificationCode": {
        "packageVerificationCodeValue": hashlib.sha1(verification_input).hexdigest(),
    },
    "licenseConcluded": "NOASSERTION",
    "licenseDeclared": "NOASSERTION",
    "copyrightText": "NOASSERTION",
}
relationships.append({
    "spdxElementId": "SPDXRef-DOCUMENT",
    "relationshipType": "DESCRIBES",
    "relatedSpdxElement": release_inputs["SPDXID"],
})

sbom = {
    "spdxVersion": "SPDX-2.3",
    "dataLicense": "CC0-1.0",
    "SPDXID": "SPDXRef-DOCUMENT",
    "name": f"RPi-Plane-Radar-{version}",
    "documentNamespace": f"https://github.com/shayne/RPi-Plane-Radar/releases/download/v{version}/sbom/{commit}",
    "creationInfo": {
        "created": timestamp,
        "creators": ["Tool: scripts/package-release.sh"],
    },
    "documentDescribes": [release_inputs["SPDXID"]],
    "packages": [release_inputs, *spdx_packages],
    "files": spdx_files,
    "relationships": relationships,
    "externalDocumentRefs": [],
    "annotations": [{
        "annotationDate": timestamp,
        "annotationType": "OTHER",
        "annotator": "Tool: scripts/package-release.sh",
        "comment": f"Built from source commit {commit} and tree {tree}",
    }],
}
(out / "SBOM.spdx.json").write_text(
    json.dumps(sbom, sort_keys=True, separators=(",", ":")) + "\n"
)

artifact_manifest = {}
for name, kind, platform, architecture in artifacts:
    path = out / name
    artifact_manifest[name] = {
        "kind": kind,
        "platform": platform,
        "architecture": architecture,
        "size": path.stat().st_size,
        "sha256": digest(path),
        "runnable": True,
    }

manifest = {
    "schema_version": 1,
    "version": version,
    "source_commit": commit,
    "source_tree": tree,
    "source_timestamp": timestamp,
    "source_date_epoch": epoch,
    "repository": "https://github.com/shayne/RPi-Plane-Radar",
    "workflow": {
        "repository": "shayne/RPi-Plane-Radar",
        "path": workflow_path,
        "ref": workflow_ref,
        "commit": workflow_commit,
    },
    "supported": {
        "model": "Raspberry Pi Zero 2 W",
        "display": "HyperPixel 2.1 Round",
        "operating_system": "Raspberry Pi OS Lite Trixie (64-bit)",
        "architecture": "aarch64",
        "kernel_policy": "driver-manifest-supported",
    },
    "required_target_packages": [
        "avahi-daemon", "build-essential", "ca-certificates", "device-tree-compiler",
        "dkms", "evtest", "kmod", "libegl1", "libgl1-mesa-dri", "libgles2",
        "libsdl2-2.0-0", "linux-headers-rpi-v8", "pngcheck",
    ],
    "minimum_control_version": "0.1.0",
    "driver": {
        "repository": os.environ["PLANERADAR_DRIVER_REPOSITORY"],
        "version": os.environ["PLANERADAR_DRIVER_VERSION"],
        "commit": os.environ["PLANERADAR_DRIVER_COMMIT"],
        "manifest_sha256": os.environ["PLANERADAR_DRIVER_MANIFEST"],
        "lifecycle_protocol": os.environ["PLANERADAR_DRIVER_PROTOCOL"],
    },
    "artifacts": artifact_manifest,
}
(out / "release-manifest.json").write_text(
    json.dumps(manifest, sort_keys=True, separators=(",", ":")) + "\n"
)
PY

./scripts/validate-release-metadata.sh \
  "$output/release-manifest.json" \
  "$output/SBOM.spdx.json"

(
  cd "$output"
  LC_ALL=C
  export LC_ALL
  for subject in \
    "$APPLICATION_ARCHIVE" \
    "$CONTROL_ARM64_ARCHIVE" \
    "$CONTROL_X86_64_ARCHIVE" \
    install.sh \
    release-manifest.json \
    SBOM.spdx.json; do
    shasum -a 256 "$subject"
  done | LC_ALL=C sort -k2 >SHA256SUMS
)

[[ "$(git rev-parse HEAD)" == "$start_head" &&
   "$(git rev-parse 'HEAD^{tree}')" == "$start_tree" &&
   -z "$(git status --porcelain --untracked-files=all)" ]] ||
  die "source changed while packaging"
reject_unexpected_release_files "$output"
(cd "$output" && shasum -a 256 -c SHA256SUMS)

mkdir -p dist
rm -rf -- dist/release
mv "$output" dist/release
trap - EXIT
rm -rf -- "$work"
printf 'Plane Radar %s release assets: %s\n' "$version" "$repo_root/dist/release"

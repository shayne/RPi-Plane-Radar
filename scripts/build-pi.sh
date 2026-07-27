#!/usr/bin/env bash
set -euo pipefail

git diff-index --quiet HEAD -- || {
  echo "build-pi requires clean tracked source" >&2
  exit 1
}
source_ref="${PLANERADAR_SOURCE_REF:-HEAD}"
workspace_tree="$(git rev-parse 'HEAD^{tree}')"
if [[ "$source_ref" == "HEAD" ]]; then
  revision="$(git rev-parse HEAD)"
  source_tree="$workspace_tree"
else
  revision="$(git rev-parse --verify "${source_ref}^{commit}")"
  source_tree="$(git rev-parse --verify "${source_ref}^{tree}")"
fi
test "$source_tree" = "$workspace_tree" || {
  echo "source ref tree does not match the clean workspace: ${source_ref}" >&2
  exit 1
}
build_context="$(mktemp -d "${TMPDIR:-/tmp}/planeradar-app-context.XXXXXX")"
cleanup() {
  local status=$?
  rm -rf -- "$build_context"
  return "$status"
}
trap cleanup EXIT
git archive --format=tar "$revision" | tar -xf - -C "$build_context"
docker info >/dev/null 2>&1 || {
  command -v orbctl >/dev/null && orbctl start
}
for attempt in {1..30}; do
  docker info >/dev/null 2>&1 && break
  sleep 1
done
docker info >/dev/null
rm -f \
  dist/planeradar \
  dist/planeradar.readelf.txt \
  dist/planeradar.revision \
  dist/planeradar.tree \
  dist/planeradar.sha256
mkdir -p dist
docker buildx build --platform linux/arm64 \
  --build-arg "PLANERADAR_REVISION=${revision}" \
  --file "$build_context/packaging/Dockerfile.build" \
  --target artifact \
  --output type=local,dest=dist "$build_context"
git diff-index --quiet HEAD -- || {
  echo "tracked source changed while building the app" >&2
  exit 1
}
test "$(git rev-parse 'HEAD^{tree}')" = "$workspace_tree" || {
  echo "workspace source tree changed while building the app" >&2
  exit 1
}
if [[ "$source_ref" == "HEAD" ]]; then
  test "$(git rev-parse HEAD)" = "$revision" || {
    echo "workspace revision changed while building the app" >&2
    exit 1
  }
else
  test "$(git rev-parse --verify "${source_ref}^{commit}")" = "$revision" || {
    echo "source ref changed while building the app: ${source_ref}" >&2
    exit 1
  }
  test "$(git rev-parse --verify "${source_ref}^{tree}")" = "$source_tree" || {
    echo "source ref tree changed while building the app: ${source_ref}" >&2
    exit 1
  }
fi
printf '%s\n' "$revision" > dist/planeradar.revision
printf '%s\n' "$source_tree" > dist/planeradar.tree
(cd dist && shasum -a 256 planeradar > planeradar.sha256)
file dist/planeradar | grep -q 'ARM aarch64'

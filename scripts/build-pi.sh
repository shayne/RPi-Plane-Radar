#!/usr/bin/env bash
set -euo pipefail

test -z "$(git status --porcelain)" || {
  echo "build-pi requires a clean workspace" >&2
  exit 1
}
docker info >/dev/null 2>&1 || {
  command -v orbctl >/dev/null && orbctl start
}
for attempt in {1..30}; do
  docker info >/dev/null 2>&1 && break
  sleep 1
done
docker info >/dev/null
source_ref="${PLANERADAR_SOURCE_REF:-rpi-port}"
revision="$(git rev-parse --verify "${source_ref}^{commit}")"
rm -f \
  dist/planeradar \
  dist/planeradar.readelf.txt \
  dist/planeradar.revision \
  dist/planeradar.sha256
mkdir -p dist
docker buildx build --platform linux/arm64 \
  --build-arg "PLANERADAR_REVISION=${revision}" \
  --file packaging/Dockerfile.build \
  --target artifact \
  --output type=local,dest=dist .
printf '%s\n' "$revision" > dist/planeradar.revision
(cd dist && shasum -a 256 planeradar > planeradar.sha256)
file dist/planeradar | grep -q 'ARM aarch64'

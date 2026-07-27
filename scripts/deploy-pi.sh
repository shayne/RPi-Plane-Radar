#!/usr/bin/env bash
set -euo pipefail

target="${PLANERADAR_PI_TARGET:-shayne@planeradar.local}"
for artifact in planeradar planeradar.sha256 planeradar.revision planeradar.readelf.txt; do
  test -f "dist/${artifact}" || {
    echo "missing dist/${artifact}; run mise run build-pi first" >&2
    exit 1
  }
done

stage="$(ssh "$target" 'mktemp -d /tmp/planeradar-stage.XXXXXX')"
scp \
  dist/planeradar \
  dist/planeradar.sha256 \
  dist/planeradar.revision \
  dist/planeradar.readelf.txt \
  "${target}:${stage}/"
ssh "$target" "cd '${stage}' && sha256sum -c planeradar.sha256 && chmod 0755 planeradar"
printf '%s\n' "$stage" > dist/last-stage-path
printf 'Staged verified installer at %s:%s/planeradar\n' "$target" "$stage"

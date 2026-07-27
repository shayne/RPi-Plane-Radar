#!/usr/bin/env bash
set -euo pipefail

target="${1:-${PLANERADAR_PI_TARGET:-pi@raspberrypi.local}}"
test -f dist/last-stage-path || {
  echo "missing dist/last-stage-path; run mise run deploy-pi first" >&2
  exit 1
}

stage="$(<dist/last-stage-path)"
revision="$(<dist/planeradar.revision)"
ssh "$target" "'${stage}/planeradar' version" | grep -F "(${revision})"

#!/usr/bin/env bash
set -euo pipefail

target="${1:-${PLANERADAR_PI_TARGET:-pi@raspberrypi.local}}"
if (( $# > 0 )); then
  shift
fi
if (( $# != 0 )); then
  echo "usage: mise run smoke-pi -- [target]" >&2
  exit 64
fi

cd "$(dirname "$0")/.."
umask 077

if [[ ! -d dist || -L dist || ! -d dist/release || -L dist/release ]]; then
  echo "missing or unsafe dist/release; run mise run package-release first" >&2
  exit 1
fi
if [[ -e dist/smoke-radar.png || -L dist/smoke-radar.png ]]; then
  if [[ ! -f dist/smoke-radar.png || -L dist/smoke-radar.png ]]; then
    echo "unsafe dist/smoke-radar.png" >&2
    exit 1
  fi
  rm -f -- dist/smoke-radar.png
fi

private="$(mktemp -d "${TMPDIR:-/tmp}/planeradar-smoke.XXXXXX")"
chmod 700 "$private"
cleanup() {
  rm -rf -- "$private"
}
trap cleanup EXIT
doctor_json="$private/doctor.json"
captured_after="$(date +%s)"

mise run status -- "$target"
mise run doctor -- "$target" --json >"$doctor_json"
mise run screenshot -- "$target" --output dist/smoke-radar.png
mise run smoke-verify -- \
  --release-dir dist/release \
  --doctor-json "$doctor_json" \
  --screenshot dist/smoke-radar.png \
  --captured-after "$captured_after"

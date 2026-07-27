#!/usr/bin/env bash
set -euo pipefail

case "${1-}" in
  --expect-tryboot)
    expected_boot=tryboot
    ;;
  --expect-normal)
    expected_boot=normal
    ;;
  *)
    echo "usage: $0 --expect-tryboot|--expect-normal" >&2
    exit 2
    ;;
esac
test "$#" -eq 1 || {
  echo "usage: $0 --expect-tryboot|--expect-normal" >&2
  exit 2
}

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
repository="$(cd "$script_dir/.." && pwd -P)"
source "$script_dir/hyperpixel-build-common.sh"
cd "$repository"

target="${1:-${PLANERADAR_PI_TARGET:-pi@raspberrypi.local}}"
case "$target" in
  ""|-*|*[!A-Za-z0-9._@-]*|@*|*@|*@*@*)
    echo "unsafe Plane Radar SSH target: $target" >&2
    exit 1
    ;;
esac
ssh_options=(-o BatchMode=yes -o ConnectTimeout=8 -o ConnectionAttempts=1)
release="$(ssh "${ssh_options[@]}" "$target" uname -r)"
hp2r_validate_release "$release"
driver_dir="${PLANERADAR_DRIVER_ARTIFACT_DIR:-dist/hyperpixel/$release}"
manifest="$driver_dir/manifest.txt"
test ! -L "$manifest" && test -f "$manifest" || {
  echo "missing or unsafe driver manifest: $manifest" >&2
  exit 1
}

manifest_value() {
  local key="$1"
  local value
  test "$(awk -F '\t' -v wanted="$key" '$1 == wanted { count++ } END { print count + 0 }' "$manifest")" -eq 1 || {
    echo "driver manifest must contain exactly one $key field" >&2
    exit 1
  }
  value="$(awk -F '\t' -v wanted="$key" '$1 == wanted && NF == 2 { print $2 }' "$manifest")"
  test -n "$value"
  printf '%s\n' "$value"
}

revision="$(manifest_value source_revision)"
manifest_release="$(manifest_value kernel_release)"
[[ "$revision" =~ ^[0-9a-f]{40}$ ]]
test "$manifest_release" = "$release"

ssh "${ssh_options[@]}" "$target" bash -s -- \
  "$expected_boot" "$revision" "$release" <<'REMOTE'
set -euo pipefail
expected_boot="$1"
revision="$2"
release="$3"
root="${PLANERADAR_INSTALL_ROOT:-}"
artifact_dir="${root}/usr/lib/planeradar/hyperpixel/${revision}/${release}"
tryboot_flag="${root}/proc/device-tree/chosen/bootloader/tryboot"
unit=planeradar-hyperpixel-checkpoint
debug_png="${root}/var/lib/planeradar/debug.png"
started=false

cleanup() {
  local status=$?
  trap - EXIT
  if "$started"; then
    if ! timeout 10 sudo systemctl stop "$unit.service"; then
      echo "transient Plane Radar service did not stop within ten seconds" >&2
      test "$status" -ne 0 || status=1
    fi
  fi
  exit "$status"
}
trap cleanup EXIT

test "$(uname -m)" = aarch64
test "$(uname -r)" = "$release"

tryboot_hex=""
if test -f "$tryboot_flag"; then
  tryboot_hex="$(od -An -tx1 -N4 "$tryboot_flag" | tr -d '[:space:]')"
fi
case "$expected_boot" in
  tryboot)
    test "$tryboot_hex" = 00000001 || {
      echo "tryboot flag is not one" >&2
      exit 1
    }
    ;;
  normal)
    test "$tryboot_hex" != 00000001 || {
      echo "tryboot flag unexpectedly reports one" >&2
      exit 1
    }
    ;;
esac

loaded_modules="$(lsmod | awk 'NR > 1 { print $1 }')"
for module in planeradar_hyperpixel2r i2c_algo_bit edt_ft5x06 vc4; do
  grep -Fxq "$module" <<<"$loaded_modules" || {
    echo "required kernel module is not loaded: $module" >&2
    exit 1
  }
done

if ! grep -Fxq v3d <<<"$loaded_modules"; then
  compatible_path="${root}/proc/device-tree/compatible"
  compatible_lines=""
  if test -f "$compatible_path" && test ! -L "$compatible_path"; then
    compatible_lines="$(tr '\000' '\n' < "$compatible_path")"
  fi
  grep -Fxq raspberrypi,model-zero-2-w <<<"$compatible_lines" &&
    grep -Fxq brcm,bcm2837 <<<"$compatible_lines" || {
      echo "V3D has neither a loaded module nor a supported integrated VC4 platform" >&2
      exit 1
    }

  v3d_status="${root}/proc/device-tree/soc/v3d@7ec00000/status"
  test -f "$v3d_status" && test ! -L "$v3d_status" &&
    test "$(od -An -tx1 "$v3d_status" | tr -d '[:space:]')" = 6f6b617900 || {
    echo "integrated VC4 V3D device-tree node is not enabled" >&2
    exit 1
  }

  render_node="${root}/dev/dri/renderD128"
  test ! -L "$render_node" || {
    echo "integrated VC4 V3D render node is missing or unsafe" >&2
    exit 1
  }
  sudo test -c "$render_node" || {
    echo "integrated VC4 V3D render node is missing or unsafe" >&2
    exit 1
  }

  kernel_log="$(sudo journalctl -b -k --no-pager)"
  grep -Eq \
    '(^|[[:space:]])vc4-drm[[:space:]]+[^[:space:]]+:[[:space:]]+bound[[:space:]]+[0-9a-f]+\.v3d[[:space:]]+\(ops[[:space:]]+vc4_v3d_ops[[:space:]]+\[vc4\]\)($|[[:space:]])' \
    <<<"$kernel_log" || {
      echo "current boot does not prove integrated V3D is bound through VC4" >&2
      exit 1
    }
fi

driver_dir="${root}/sys/bus/platform/drivers/planeradar-hyperpixel2r"
test -d "$driver_dir"
bound_devices=()
for node in "$driver_dir"/*; do
  case "$(basename "$node")" in
    bind|module|new_id|remove_id|uevent|unbind) continue ;;
  esac
  if test -e "$node" || test -L "$node"; then
    bound_device="$(readlink -f -- "$node")"
    test -d "$bound_device"
    bound_devices+=("$bound_device")
  fi
done
((${#bound_devices[@]} > 0)) || {
  echo "custom HyperPixel platform node is not bound" >&2
  exit 1
}

connected_count=0
for status_path in "${root}"/sys/class/drm/card*-*/status; do
  test -f "$status_path" || continue
  if test "$(cat "$status_path")" = connected; then
    modes_path="$(dirname "$status_path")/modes"
    grep -Fxq 480x480 "$modes_path" || continue
    connected_count=$((connected_count + 1))
  fi
done
test "$connected_count" -eq 1 || {
  echo "expected exactly one connected 480x480 DRM connector" >&2
  exit 1
}

event_name=""
event_device_name=""
for name_path in "${root}"/sys/class/input/event*/device/name; do
  test -f "$name_path" || continue
  grep -Eiq 'EDT|FT5' "$name_path" || continue
  input_device="$(readlink -f -- "$(dirname "$name_path")")"
  for bound_device in "${bound_devices[@]}"; do
    case "$input_device" in
      "$bound_device"/*)
        event_name="$(basename "$(dirname "$(dirname "$name_path")")")"
        event_device_name="$(cat "$name_path")"
        break 2
        ;;
    esac
  done
done
test -n "$event_name" || {
  echo "no EDT or FT5 input event device belongs to the bound HyperPixel platform device" >&2
  exit 1
}
event_device="${root}/dev/input/$event_name"
axis_info=""
axis_probe_status=0
axis_info="$(
  sudo timeout \
    --signal=INT \
    --kill-after=1 \
    2 \
    stdbuf -oL -eL \
    evtest "$event_device" 2>&1
)" || axis_probe_status=$?
if test "$axis_probe_status" -ne 124; then
  if grep -Eiq 'No such (file or directory|device)' <<<"$axis_info"; then
    echo \
      "touch event device became unavailable before capability probing: ${event_device}" \
      >&2
  else
    echo \
      "touch capability probe failed before its expected timeout (status ${axis_probe_status}): ${event_device}" \
      >&2
  fi
  test -z "$axis_info" || printf '%s\n' "$axis_info" >&2
  exit 1
fi
grep -Fxq "Input device name: \"${event_device_name}\"" <<<"$axis_info" || {
  echo \
    "touch capability output does not identify the ancestry-validated device: ${event_device}" \
    >&2
  exit 1
}
axis_check_status=0
awk '
  /Event code .*\(ABS_MT_POSITION_X\)/ {
    axis = "x"
    present[axis] = 1
    next
  }
  /Event code .*\(ABS_MT_POSITION_Y\)/ {
    axis = "y"
    present[axis] = 1
    next
  }
  /Event code / { axis = ""; next }
  axis != "" && $1 == "Max" {
    if ($2 == 479 || $2 == 480) valid[axis] = 1
    axis = ""
  }
  END {
    if (!(present["x"] && present["y"])) exit 20
    if (!(valid["x"] && valid["y"])) exit 21
  }
' <<<"$axis_info" || axis_check_status=$?
case "$axis_check_status" in
  0) ;;
  20)
    echo \
      "touch input capabilities are missing ABS_MT_POSITION_X or ABS_MT_POSITION_Y: ${event_device}" \
      >&2
    exit 1
    ;;
  21)
    echo "touch input axes do not report 479 or 480 maxima: ${event_device}" >&2
    exit 1
    ;;
  *)
    echo "touch capability parser failed with status ${axis_check_status}: ${event_device}" >&2
    exit 1
    ;;
esac

test -f "$artifact_dir/manifest.txt"
test -f "$artifact_dir/planeradar.revision"
test -f "$artifact_dir/planeradar.tree"
test -x "$artifact_dir/planeradar"
test "$(cat "$artifact_dir/planeradar.revision")" = "$revision"
test "$(cat "$artifact_dir/planeradar.tree")" = \
  "$(awk -F '\t' '$1 == "source_tree" { print $2 }' "$artifact_dir/manifest.txt")"
"$artifact_dir/planeradar" version | grep -Fq "($revision)"

sudo rm -f -- "$debug_png"
journal_cursor="$(
  sudo journalctl -b -n 0 --show-cursor --no-pager |
    awk '/^-- cursor: / { cursor = substr($0, 12) } END { print cursor }'
)"
test -n "$journal_cursor" || {
  echo "could not capture the pre-launch journal cursor" >&2
  exit 1
}
sudo systemd-run \
  --unit="$unit" \
  --collect \
  --uid=shayne \
  --property=StateDirectory=planeradar \
  --property=StateDirectoryMode=0750 \
  --property=AmbientCapabilities=CAP_NET_BIND_SERVICE \
  --setenv=SDL_VIDEODRIVER=kmsdrm \
  --setenv=SDL_RENDER_DRIVER=opengles2 \
  --setenv=RUST_LOG=info \
  "$artifact_dir/planeradar" run \
  --settings "${root}/var/lib/planeradar/settings.json" \
  --geocode-cache "${root}/var/lib/planeradar/geocode-cache.json" \
  --debug-frame "$debug_png" \
  --http 0.0.0.0:80
started=true

health=""
for _attempt in {1..40}; do
  if health="$(
    curl \
      --fail \
      --silent \
      --show-error \
      --header 'Host: planeradar.local' \
      http://127.0.0.1/healthz \
      2>/dev/null
  )"; then
    break
  fi
  sleep 0.25
done
test -n "$health" || {
  echo "Plane Radar health endpoint did not become ready" >&2
  exit 1
}
compact_health="$(tr -d '[:space:]' <<<"$health")"
grep -Fq "\"revision\":\"$revision\"" <<<"$compact_health" || {
  echo "Plane Radar health revision does not match the driver manifest" >&2
  exit 1
}

sdl_ready_in_log() {
  local line
  while IFS= read -r line; do
    case "$line" in
      *"] SDL display ready: video_driver=kmsdrm render_driver=opengles2" | \
        *"] SDL display ready: video_driver=KMSDRM render_driver=opengles2")
        return 0
        ;;
    esac
  done
  return 1
}
wait_for_sdl_ready() {
  local wait_unit="$1"
  local wait_cursor="$2"
  local candidate_log
  while true; do
    if ! candidate_log="$(
      sudo journalctl \
        -b \
        -u "$wait_unit.service" \
        --after-cursor="$wait_cursor" \
        --no-pager
    )"; then
      return 2
    fi
    if sdl_ready_in_log <<<"$candidate_log"; then
      return 0
    fi
    sleep 0.25
  done
}
export -f sdl_ready_in_log wait_for_sdl_ready
set +e
timeout --signal=TERM --kill-after=1 10 \
  bash -c 'wait_for_sdl_ready "$1" "$2"' _ "$unit" "$journal_cursor"
sdl_wait_status=$?
set -e
unset -f sdl_ready_in_log wait_for_sdl_ready
case "$sdl_wait_status" in
  0) ;;
  124 | 137)
    echo "timed out waiting up to 10 seconds for current invocation exact KMSDRM/opengles2 readiness" >&2
    exit 1
    ;;
  *)
    echo "current invocation SDL readiness journal probe failed with status $sdl_wait_status" >&2
    exit 1
    ;;
esac

main_pid="$(systemctl show "$unit.service" --property=MainPID --value)"
[[ "$main_pid" =~ ^[1-9][0-9]*$ ]]
sudo kill -USR1 "$main_pid"
for _attempt in {1..40}; do
  test -f "$debug_png" && break
  sleep 0.25
done
test -f "$debug_png"
pngcheck -q "$debug_png"
test "$(od -An -tx1 -N8 "$debug_png" | tr -d '[:space:]')" = 89504e470d0a1a0a
test "$(od -An -tx1 -j16 -N8 "$debug_png" | tr -d '[:space:]')" = 000001e0000001e0

boot_log="$(sudo journalctl -b --no-pager)"
if grep -Eiq \
  'planeradar[-_]hyperpixel2r.*(warn|error|fail)|blocked for more than|INFO: task .* blocked|kernel oops|Oops:|BUG: unable to handle kernel|Kernel panic' \
  <<<"$boot_log"
then
  echo "current boot journal contains a driver, blocked-task, or kernel failure" >&2
  exit 1
fi
failed_units="$(systemctl --failed --no-legend --plain)"
test -z "$failed_units" || {
  echo "current boot has failed systemd units:" >&2
  printf '%s\n' "$failed_units" >&2
  exit 1
}

printf 'Verified HyperPixel %s boot for revision %s\n' "$expected_boot" "$revision"
REMOTE

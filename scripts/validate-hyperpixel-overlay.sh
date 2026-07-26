#!/usr/bin/env bash
set -euo pipefail

overlay_path="${1-}"
test -n "$overlay_path" && test -f "$overlay_path" || {
  echo "usage: $0 OVERLAY_DTBO" >&2
  exit 2
}
overlay_dir="$(cd "$(dirname "$overlay_path")" && pwd -P)"
overlay_file="$(basename "$overlay_path")"
image="${PLANERADAR_KERNEL_BUILD_IMAGE:-planeradar-kernel-builder:debian-trixie-gcc14}"

docker run --rm \
  --volume "$overlay_dir:/overlay:ro" \
  "$image" \
  sh -eu -c '
    overlay="/overlay/$1"

    fail() {
      echo "$1" >&2
      exit 1
    }

    require_node_shape() {
      path="$1"
      expected_properties="$2"
      expected_children="$3"
      error="$4"
      properties="$(fdtget -p "$overlay" "$path" | LC_ALL=C sort)" ||
        fail "$error"
      children="$(fdtget -l "$overlay" "$path" | LC_ALL=C sort)" ||
        fail "$error"
      test "$properties" = "$expected_properties" ||
        fail "$error"
      test "$children" = "$expected_children" ||
        fail "$error"
    }

    test "$(fdtget -t s "$overlay" / compatible)" = brcm,bcm2835 ||
      fail "root compatible is invalid"
    require_node_shape \
      / \
      compatible \
      "__fixups__
__local_fixups__
__overrides__
__symbols__
fragment@0
fragment@1" \
      "compiled overlay root shape is invalid"
    require_node_shape \
      /fragment@0 \
      target-path \
      __overlay__ \
      "root fragment shape is invalid"
    require_node_shape \
      /fragment@0/__overlay__ \
      "" \
      planeradar-hyperpixel2r \
      "root fragment overlay shape is invalid"

    panel_path=/fragment@0/__overlay__/planeradar-hyperpixel2r
    touch_path="$panel_path/touchscreen@15"
    require_node_shape \
      "$panel_path" \
      "#address-cells
#size-cells
backlight-gpios
compatible
cs-gpios
phandle
rotation
scl-gpios
sda-gpios" \
      "port
touchscreen@15" \
      "panel subtree shape is invalid"
    require_node_shape \
      "$touch_path" \
      "compatible
interrupt-parent
interrupts
phandle
reg
touchscreen-size-x
touchscreen-size-y" \
      "" \
      "touchscreen subtree shape is invalid"
    require_node_shape \
      "$panel_path/port" \
      "" \
      endpoint \
      "panel port shape is invalid"
    require_node_shape \
      "$panel_path/port/endpoint" \
      "phandle
remote-endpoint" \
      "" \
      "panel endpoint shape is invalid"

    require_node_shape \
      /fragment@1 \
      target \
      __overlay__ \
      "DPI fragment shape is invalid"
    require_node_shape \
      /fragment@1/__overlay__ \
      "pinctrl-0
pinctrl-names
status" \
      port \
      "DPI overlay shape is invalid"
    require_node_shape \
      /fragment@1/__overlay__/port \
      "" \
      endpoint \
      "DPI port shape is invalid"
    require_node_shape \
      /fragment@1/__overlay__/port/endpoint \
      "phandle
remote-endpoint" \
      "" \
      "DPI endpoint shape is invalid"

    require_node_shape \
      /__overrides__ \
      "rotate
touchscreen-inverted-x
touchscreen-inverted-y
touchscreen-swapped-x-y" \
      "" \
      "overlay override shape is invalid"
    require_node_shape \
      /__symbols__ \
      "dpi_out
panel_in
planeradar_panel
polytouch" \
      "" \
      "overlay symbol shape is invalid"
    require_node_shape \
      /__fixups__ \
      "dpi
dpi_18bit_cpadhi_gpio0
gpio" \
      "" \
      "overlay fixup shape is invalid"
    require_node_shape \
      /__local_fixups__ \
      "" \
      "__overrides__
fragment@0
fragment@1" \
      "overlay local-fixup shape is invalid"
    require_node_shape \
      /__local_fixups__/fragment@0 \
      "" \
      __overlay__ \
      "root local-fixup shape is invalid"
    require_node_shape \
      /__local_fixups__/fragment@0/__overlay__ \
      "" \
      planeradar-hyperpixel2r \
      "root overlay local-fixup shape is invalid"
    require_node_shape \
      /__local_fixups__/fragment@0/__overlay__/planeradar-hyperpixel2r \
      "" \
      port \
      "panel local-fixup shape is invalid"
    require_node_shape \
      /__local_fixups__/fragment@0/__overlay__/planeradar-hyperpixel2r/port \
      "" \
      endpoint \
      "panel port local-fixup shape is invalid"
    require_node_shape \
      /__local_fixups__/fragment@0/__overlay__/planeradar-hyperpixel2r/port/endpoint \
      remote-endpoint \
      "" \
      "panel endpoint local-fixup shape is invalid"
    require_node_shape \
      /__local_fixups__/fragment@1 \
      "" \
      __overlay__ \
      "DPI local-fixup shape is invalid"
    require_node_shape \
      /__local_fixups__/fragment@1/__overlay__ \
      "" \
      port \
      "DPI overlay local-fixup shape is invalid"
    require_node_shape \
      /__local_fixups__/fragment@1/__overlay__/port \
      "" \
      endpoint \
      "DPI port local-fixup shape is invalid"
    require_node_shape \
      /__local_fixups__/fragment@1/__overlay__/port/endpoint \
      remote-endpoint \
      "" \
      "DPI endpoint local-fixup shape is invalid"
    require_node_shape \
      /__local_fixups__/__overrides__ \
      "rotate
touchscreen-inverted-x
touchscreen-inverted-y
touchscreen-swapped-x-y" \
      "" \
      "override local-fixup shape is invalid"

    override_properties="$(fdtget -p "$overlay" /__overrides__ | sort)"
    expected_override_properties="rotate
touchscreen-inverted-x
touchscreen-inverted-y
touchscreen-swapped-x-y"
    test "$override_properties" = "$expected_override_properties" ||
      fail "overlay override property set is invalid"

    test "$(fdtget -t bx "$overlay" /__overrides__ rotate)" = \
      "0 0 0 3 72 6f 74 61 74 69 6f 6e 3a 30 0" ||
      fail "invalid rotate override encoding"
    test "$(fdtget -t bx "$overlay" /__overrides__ touchscreen-inverted-x)" = \
      "0 0 0 4 74 6f 75 63 68 73 63 72 65 65 6e 2d 69 6e 76 65 72 74 65 64 2d 78 3f 0" ||
      fail "invalid touchscreen-inverted-x override encoding"
    test "$(fdtget -t bx "$overlay" /__overrides__ touchscreen-inverted-y)" = \
      "0 0 0 4 74 6f 75 63 68 73 63 72 65 65 6e 2d 69 6e 76 65 72 74 65 64 2d 79 3f 0" ||
      fail "invalid touchscreen-inverted-y override encoding"
    test "$(fdtget -t bx "$overlay" /__overrides__ touchscreen-swapped-x-y)" = \
      "0 0 0 4 74 6f 75 63 68 73 63 72 65 65 6e 2d 73 77 61 70 70 65 64 2d 78 2d 79 3f 0" ||
      fail "invalid touchscreen-swapped-x-y override encoding"

    test "$(fdtget -t x "$overlay" "$panel_path" phandle)" = 3 ||
      fail "rotate override target phandle is invalid"
    test "$(fdtget -t x "$overlay" "$touch_path" phandle)" = 4 ||
      fail "touchscreen override target phandle is invalid"
    for parameter in \
      rotate \
      touchscreen-inverted-x \
      touchscreen-inverted-y \
      touchscreen-swapped-x-y
    do
      test "$(
        fdtget -t x "$overlay" /__local_fixups__/__overrides__ "$parameter"
      )" = 0 || fail "override local fixup is invalid: $parameter"
    done

    fixup_properties="$(fdtget -p "$overlay" /__fixups__ | sort)"
    expected_fixup_properties="dpi
dpi_18bit_cpadhi_gpio0
gpio"
    test "$fixup_properties" = "$expected_fixup_properties" ||
      fail "overlay fixup property set is invalid"
    test "$(fdtget -t s "$overlay" /__fixups__ dpi)" = \
      "/fragment@1:target:0" ||
      fail "DPI fragment target fixup is invalid"
    test "$(
      fdtget -t s "$overlay" /__fixups__ dpi_18bit_cpadhi_gpio0
    )" = "/fragment@1/__overlay__:pinctrl-0:0" ||
      fail "DPI pinctrl fixup is invalid"
    test "$(fdtget -t s "$overlay" /__fixups__ gpio)" = \
      "/fragment@0/__overlay__/planeradar-hyperpixel2r:sda-gpios:0 /fragment@0/__overlay__/planeradar-hyperpixel2r:scl-gpios:0 /fragment@0/__overlay__/planeradar-hyperpixel2r:cs-gpios:0 /fragment@0/__overlay__/planeradar-hyperpixel2r:backlight-gpios:0 /fragment@0/__overlay__/planeradar-hyperpixel2r/touchscreen@15:interrupt-parent:0" ||
      fail "GPIO fixups are invalid"

    fragments="$(fdtget -l "$overlay" / | grep "^fragment@" | sort)"
    expected_fragments="fragment@0
fragment@1"
    test "$fragments" = "$expected_fragments" ||
      fail "compiled overlay fragment set is invalid"
    test "$(fdtget -t s "$overlay" /fragment@0 target-path)" = / ||
      fail "root fragment target-path is invalid"
    if fdtget "$overlay" /fragment@0 target >/dev/null 2>&1; then
      fail "root fragment must not contain a target phandle"
    fi
    test "$(fdtget -t x "$overlay" /fragment@1 target)" = ffffffff ||
      fail "DPI fragment target placeholder is invalid"
    if fdtget "$overlay" /fragment@1 target-path >/dev/null 2>&1; then
      fail "DPI fragment must not contain target-path"
    fi

    test "$(fdtget -t s "$overlay" "$panel_path" compatible)" = \
      planeradar,hyperpixel2r ||
      fail "panel compatible is invalid"
    test "$(fdtget -t x "$overlay" "$panel_path" sda-gpios)" = \
      "ffffffff a 0" ||
      fail "panel SDA GPIO payload is invalid"
    test "$(fdtget -t x "$overlay" "$panel_path" scl-gpios)" = \
      "ffffffff b 0" ||
      fail "panel SCL GPIO payload is invalid"
    test "$(fdtget -t x "$overlay" "$panel_path" cs-gpios)" = \
      "ffffffff 12 1" ||
      fail "panel CS GPIO payload is invalid"
    test "$(fdtget -t x "$overlay" "$panel_path" backlight-gpios)" = \
      "ffffffff 13 0" ||
      fail "panel backlight GPIO payload is invalid"
    test "$(fdtget -t x "$overlay" "$panel_path" rotation)" = 0 ||
      fail "panel default rotation is invalid"
    test "$(fdtget -t x "$overlay" "$panel_path" "#address-cells")" = 1 ||
      fail "panel address-cell count is invalid"
    test "$(fdtget -t x "$overlay" "$panel_path" "#size-cells")" = 0 ||
      fail "panel size-cell count is invalid"
    test "$(fdtget -t s "$overlay" "$touch_path" compatible)" = \
      edt,edt-ft5406 ||
      fail "touchscreen compatible is invalid"
    test "$(fdtget -t x "$overlay" "$touch_path" reg)" = 15 ||
      fail "touchscreen address is invalid"
    test "$(fdtget -t x "$overlay" "$touch_path" interrupt-parent)" = ffffffff ||
      fail "touchscreen interrupt parent is invalid"
    test "$(fdtget -t x "$overlay" "$touch_path" interrupts)" = "1b 2" ||
      fail "touchscreen interrupt payload is invalid"
    test "$(fdtget -t x "$overlay" "$touch_path" touchscreen-size-x)" = 1e0 ||
      fail "touchscreen X size is invalid"
    test "$(fdtget -t x "$overlay" "$touch_path" touchscreen-size-y)" = 1e0 ||
      fail "touchscreen Y size is invalid"
    test "$(
      fdtget -t x "$overlay" "$panel_path/port/endpoint" phandle
    )" = 2 || fail "panel endpoint phandle is invalid"
    test "$(
      fdtget -t x "$overlay" "$panel_path/port/endpoint" remote-endpoint
    )" = 1 || fail "panel endpoint link is invalid"
    test "$(fdtget -t s "$overlay" /fragment@1/__overlay__ status)" = okay ||
      fail "DPI status is invalid"
    test "$(
      fdtget -t s "$overlay" /fragment@1/__overlay__ pinctrl-names
    )" = default || fail "DPI pinctrl name is invalid"
    test "$(
      fdtget -t x "$overlay" /fragment@1/__overlay__ pinctrl-0
    )" = ffffffff || fail "DPI pinctrl payload is invalid"
    test "$(
      fdtget -t x "$overlay" /fragment@1/__overlay__/port/endpoint phandle
    )" = 1 || fail "DPI endpoint phandle is invalid"
    test "$(
      fdtget -t x "$overlay" /fragment@1/__overlay__/port/endpoint remote-endpoint
    )" = 2 || fail "DPI endpoint link is invalid"

    test "$(fdtget -t s "$overlay" /__symbols__ planeradar_panel)" = \
      "$panel_path" ||
      fail "panel symbol is invalid"
    test "$(fdtget -t s "$overlay" /__symbols__ polytouch)" = \
      "$touch_path" ||
      fail "touchscreen symbol is invalid"
    test "$(fdtget -t s "$overlay" /__symbols__ panel_in)" = \
      "$panel_path/port/endpoint" ||
      fail "panel endpoint symbol is invalid"
    test "$(fdtget -t s "$overlay" /__symbols__ dpi_out)" = \
      /fragment@1/__overlay__/port/endpoint ||
      fail "DPI endpoint symbol is invalid"

    test "$(
      fdtget -t x "$overlay" \
        /__local_fixups__/fragment@0/__overlay__/planeradar-hyperpixel2r/port/endpoint \
        remote-endpoint
    )" = 0 || fail "panel endpoint local fixup is invalid"
    test "$(
      fdtget -t x "$overlay" \
        /__local_fixups__/fragment@1/__overlay__/port/endpoint \
        remote-endpoint
    )" = 0 || fail "DPI endpoint local fixup is invalid"
  ' sh "$overlay_file"

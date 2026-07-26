#!/usr/bin/env bash
set -euo pipefail

test_binary="$(mktemp "${TMPDIR:-/tmp}/planeradar-hp2r-gpio-test.XXXXXX")"
trap 'rm -f "$test_binary"' EXIT

"${CC:-cc}" \
  -std=c11 \
  -Wall \
  -Wextra \
  -Werror \
  kernel/planeradar_hyperpixel2r_gpio.c \
  kernel/tests/gpio_test.c \
  -o "$test_binary"
"$test_binary"

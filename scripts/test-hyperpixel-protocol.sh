#!/usr/bin/env bash
set -euo pipefail

build_dir="$(mktemp -d "${TMPDIR:-/tmp}/planeradar-protocol.XXXXXX")"
trap 'rm -rf "$build_dir"' EXIT
"${CC:-cc}" -std=c11 -Wall -Wextra -Werror -pedantic \
  -Ikernel \
  kernel/planeradar_hyperpixel2r_protocol.c \
  kernel/tests/protocol_test.c \
  -o "$build_dir/protocol-test"
"$build_dir/protocol-test"

#!/bin/bash
set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
TEST_DIR="$(mktemp -d "${TMPDIR:-/tmp}/greenmole-microphone-tests.XXXXXXXX")"
trap 'rm -rf "$TEST_DIR"' EXIT
xcrun clang -std=c11 -O1 -g -Wall -Wextra -Werror -fsanitize=undefined \
  -mmacosx-version-min=14.0 -framework CoreAudio -framework CoreFoundation \
  "$SCRIPT_DIR/tests.c" -o "$TEST_DIR/tests"
"$TEST_DIR/tests"
/bin/sh "$SCRIPT_DIR/test-installer-permissions.sh"
bash -n "$SCRIPT_DIR/build.sh"
sh -n "$SCRIPT_DIR/install-microphone-driver.sh"
sh -n "$SCRIPT_DIR/uninstall-microphone-driver.sh"

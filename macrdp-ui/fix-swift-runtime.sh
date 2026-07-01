#!/bin/bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
APP_BUNDLE="${1:-$SCRIPT_DIR/src-tauri/target/release/bundle/macos/macrdp.app}"
APP_BIN="$APP_BUNDLE/Contents/MacOS/macrdp-ui"
FRAMEWORKS_DIR="$APP_BUNDLE/Contents/Frameworks"
TARGET_LIB="libswift_Concurrency.dylib"
RPATH="@executable_path/../Frameworks"
BUNDLE_ID="${BUNDLE_ID:-com.macrdp.app}"
SWIFT_RUNTIME_MODE="${MACRDP_SWIFT_RUNTIME_MODE:-auto}"

if [ ! -f "$APP_BIN" ]; then
  echo "App binary not found: $APP_BIN" >&2
  exit 1
fi

find_swift_lib_dir() {
  local candidates=()

  if [ -n "${SWIFT_LIB_PATH:-}" ]; then
    candidates+=("$SWIFT_LIB_PATH")
  fi

  candidates+=("/usr/lib/swift")

  if command -v xcode-select >/dev/null 2>&1; then
    local dev_path
    dev_path="$(xcode-select -p 2>/dev/null || true)"
    if [ -n "$dev_path" ]; then
      if [[ "$dev_path" == *CommandLineTools* ]]; then
        candidates+=(
          "$dev_path/usr/lib/swift-5.5/macosx"
          "$dev_path/usr/lib/swift/macosx"
        )
      else
        candidates+=(
          "$dev_path/Toolchains/XcodeDefault.xctoolchain/usr/lib/swift-5.5/macosx"
          "$dev_path/Toolchains/XcodeDefault.xctoolchain/usr/lib/swift/macosx"
        )
      fi
    fi
  fi

  candidates+=(
    "/Library/Developer/CommandLineTools/usr/lib/swift-5.5/macosx"
    "/Library/Developer/CommandLineTools/usr/lib/swift/macosx"
  )

  for dir in "${candidates[@]}"; do
    if [ -f "$dir/$TARGET_LIB" ]; then
      echo "$dir"
      return 0
    fi
  done

  return 1
}

SWIFT_LIB_DIR="$(find_swift_lib_dir)"
SYSTEM_SWIFT_LIB="/usr/lib/swift/$TARGET_LIB"

if [ "$SWIFT_RUNTIME_MODE" = "auto" ] && [ -f "$SYSTEM_SWIFT_LIB" ]; then
  SWIFT_RUNTIME_MODE="system"
fi

if [ "$SWIFT_RUNTIME_MODE" = "system" ]; then
  if otool -L "$APP_BIN" | grep -q "@rpath/$TARGET_LIB"; then
    install_name_tool -change "@rpath/$TARGET_LIB" "$SYSTEM_SWIFT_LIB" "$APP_BIN"
  fi
  rm -f "$FRAMEWORKS_DIR/$TARGET_LIB"
elif [ "$SWIFT_RUNTIME_MODE" = "bundle" ] || [ "$SWIFT_RUNTIME_MODE" = "auto" ]; then
  mkdir -p "$FRAMEWORKS_DIR"
  cp "$SWIFT_LIB_DIR/$TARGET_LIB" "$FRAMEWORKS_DIR/$TARGET_LIB"

  if ! otool -l "$APP_BIN" | grep -A2 "LC_RPATH" | grep -q "$RPATH"; then
    install_name_tool -add_rpath "$RPATH" "$APP_BIN"
  fi
else
  echo "Unsupported MACRDP_SWIFT_RUNTIME_MODE: $SWIFT_RUNTIME_MODE" >&2
  exit 1
fi

# DMG consumers on another Mac must be able to read the full bundle even when
# the original file owner UID does not exist there.
find "$APP_BUNDLE" -type d -exec chmod 755 {} +
find "$APP_BUNDLE" -type f -exec chmod 644 {} +
chmod 755 "$APP_BIN"
if [ -f "$FRAMEWORKS_DIR/$TARGET_LIB" ]; then
  chmod 755 "$FRAMEWORKS_DIR/$TARGET_LIB"
fi

if [ -f "$FRAMEWORKS_DIR/$TARGET_LIB" ]; then
  codesign --force --sign - "$FRAMEWORKS_DIR/$TARGET_LIB"
fi
codesign --force --sign - --identifier "$BUNDLE_ID" "$APP_BUNDLE"

if [ "$SWIFT_RUNTIME_MODE" = "system" ]; then
  echo "Swift concurrency linked against system runtime: $SYSTEM_SWIFT_LIB"
else
  echo "Swift runtime bundled from: $SWIFT_LIB_DIR/$TARGET_LIB"
fi
echo "Patched app bundle: $APP_BUNDLE"

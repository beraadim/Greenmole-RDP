#!/bin/bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
APP_BUNDLE="${1:-$SCRIPT_DIR/src-tauri/target/release/bundle/macos/Greenmole RDP.app}"
APP_BIN="$APP_BUNDLE/Contents/MacOS/macrdp-ui"
FRAMEWORKS_DIR="$APP_BUNDLE/Contents/Frameworks"
TARGET_LIB="libswift_Concurrency.dylib"
RPATH="@executable_path/../Frameworks"
BUNDLE_ID="${BUNDLE_ID:-com.macrdp.app}"
SIGNING_IDENTITY="${MACRDP_SIGNING_IDENTITY:--}"
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

remove_rpath_if_present() {
  local binary="$1"
  local rpath="$2"

  if otool -l "$binary" | awk '
    $1 == "cmd" && $2 == "LC_RPATH" { in_rpath = 1; next }
    in_rpath && $1 == "path" {
      if ($2 == target) found = 1;
      in_rpath = 0;
    }
    END { exit(found ? 0 : 1) }
  ' target="$rpath"; then
    install_name_tool -delete_rpath "$rpath" "$binary"
  fi
}

remove_known_swift_toolchain_rpaths() {
  local binary="$1"
  local -a stale_rpaths=(
    "/Library/Developer/CommandLineTools/usr/lib/swift-5.5/macosx"
    "/Library/Developer/CommandLineTools/usr/lib/swift/macosx"
  )

  if command -v xcode-select >/dev/null 2>&1; then
    local dev_path
    dev_path="$(xcode-select -p 2>/dev/null || true)"
    if [ -n "$dev_path" ]; then
      if [[ "$dev_path" == *CommandLineTools* ]]; then
        stale_rpaths+=(
          "$dev_path/usr/lib/swift-5.5/macosx"
          "$dev_path/usr/lib/swift/macosx"
        )
      else
        stale_rpaths+=(
          "$dev_path/Toolchains/XcodeDefault.xctoolchain/usr/lib/swift-5.5/macosx"
          "$dev_path/Toolchains/XcodeDefault.xctoolchain/usr/lib/swift/macosx"
        )
      fi
    fi
  fi

  for stale_rpath in "${stale_rpaths[@]}"; do
    remove_rpath_if_present "$binary" "$stale_rpath"
  done
}

verify_runtime_shape() {
  local binary="$1"
  local mode="$2"

  if [ "$mode" = "system" ]; then
    if otool -L "$binary" | grep -q "@rpath/$TARGET_LIB"; then
      echo "Expected system Swift runtime, but $TARGET_LIB still uses @rpath in $binary" >&2
      exit 1
    fi
  else
    if ! otool -L "$binary" | grep -q "@rpath/$TARGET_LIB"; then
      echo "Expected bundled Swift runtime, but $TARGET_LIB is not linked via @rpath in $binary" >&2
      exit 1
    fi
    if ! otool -l "$binary" | awk '
      $1 == "cmd" && $2 == "LC_RPATH" { in_rpath = 1; next }
      in_rpath && $1 == "path" {
        if ($2 == target) found = 1;
        in_rpath = 0;
      }
      END { exit(found ? 0 : 1) }
    ' target="$RPATH"; then
      echo "Expected bundled Swift runtime rpath $RPATH in $binary" >&2
      exit 1
    fi
  fi

  if otool -l "$binary" | grep -q "/Library/Developer/CommandLineTools/usr/lib/swift"; then
    echo "Refusing mixed Swift runtime: Command Line Tools rpath still present in $binary" >&2
    exit 1
  fi
}

SYSTEM_SWIFT_LIB="/usr/lib/swift/$TARGET_LIB"

# On current macOS releases the Swift concurrency runtime may come from the
# dyld shared cache even when no on-disk stub exists. Prefer that system
# install name by default so we do not load both the system runtime and a
# manually bundled Command Line Tools copy at once.
if [ "$SWIFT_RUNTIME_MODE" = "auto" ]; then
  SWIFT_RUNTIME_MODE="system"
fi

if [ "$SWIFT_RUNTIME_MODE" = "system" ]; then
  if otool -L "$APP_BIN" | grep -q "@rpath/$TARGET_LIB"; then
    install_name_tool -change "@rpath/$TARGET_LIB" "$SYSTEM_SWIFT_LIB" "$APP_BIN"
  fi
  rm -f "$FRAMEWORKS_DIR/$TARGET_LIB"
elif [ "$SWIFT_RUNTIME_MODE" = "bundle" ] || [ "$SWIFT_RUNTIME_MODE" = "auto" ]; then
  SWIFT_LIB_DIR="$(find_swift_lib_dir)"
  mkdir -p "$FRAMEWORKS_DIR"
  cp "$SWIFT_LIB_DIR/$TARGET_LIB" "$FRAMEWORKS_DIR/$TARGET_LIB"

  if ! otool -l "$APP_BIN" | grep -A2 "LC_RPATH" | grep -q "$RPATH"; then
    install_name_tool -add_rpath "$RPATH" "$APP_BIN"
  fi
else
  echo "Unsupported MACRDP_SWIFT_RUNTIME_MODE: $SWIFT_RUNTIME_MODE" >&2
  exit 1
fi

remove_known_swift_toolchain_rpaths "$APP_BIN"
verify_runtime_shape "$APP_BIN" "$SWIFT_RUNTIME_MODE"

# DMG consumers on another Mac must be able to read the full bundle even when
# the original file owner UID does not exist there.
find "$APP_BUNDLE" -type d -exec chmod 755 {} +
find "$APP_BUNDLE" -type f -exec chmod 644 {} +
chmod 755 "$APP_BIN"
if [ -f "$FRAMEWORKS_DIR/$TARGET_LIB" ]; then
  chmod 755 "$FRAMEWORKS_DIR/$TARGET_LIB"
fi

if [ -f "$FRAMEWORKS_DIR/$TARGET_LIB" ]; then
  if [ "$SIGNING_IDENTITY" = - ]; then
    codesign --force --sign - "$FRAMEWORKS_DIR/$TARGET_LIB"
  else
    codesign --force --sign "$SIGNING_IDENTITY" --options runtime --timestamp "$FRAMEWORKS_DIR/$TARGET_LIB"
  fi
fi
if [ "$SIGNING_IDENTITY" = - ]; then
  codesign --force --sign - --identifier "$BUNDLE_ID" "$APP_BUNDLE"
else
  codesign --force --sign "$SIGNING_IDENTITY" --identifier "$BUNDLE_ID" --options runtime --timestamp "$APP_BUNDLE"
fi

if [ "$SWIFT_RUNTIME_MODE" = "system" ]; then
  echo "Swift concurrency linked against system runtime: $SYSTEM_SWIFT_LIB"
else
  echo "Swift runtime bundled from: $SWIFT_LIB_DIR/$TARGET_LIB"
fi
echo "Patched app bundle: $APP_BUNDLE"

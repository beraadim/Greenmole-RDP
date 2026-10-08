#!/bin/bash
set -euo pipefail

BUNDLE_ID="${BUNDLE_ID:-com.macrdp.app}"
PROFILE="${MACRDP_CLI_PROFILE:-release}"
SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"

case "$PROFILE" in
  release)
    TARGET_DIR="release"
    TAURI_ARGS=(--no-sign)
    ;;
  debug)
    TARGET_DIR="debug"
    TAURI_ARGS=(--debug --no-sign)
    ;;
  *)
    echo "Unsupported MACRDP_CLI_PROFILE: $PROFILE" >&2
    exit 1
    ;;
esac

APP_BUNDLE="$SCRIPT_DIR/src-tauri/target/$TARGET_DIR/bundle/macos/macrdp.app"
DMG_PATH="$SCRIPT_DIR/src-tauri/target/$TARGET_DIR/bundle/dmg/macrdp_0.1.0_aarch64.dmg"
APP_BIN="$SCRIPT_DIR/src-tauri/target/$TARGET_DIR/macrdp-ui"

echo "==> Building macrdp-ui (profile: $PROFILE)..."
cd "$SCRIPT_DIR"
npm run tauri build -- "${TAURI_ARGS[@]}" -- --locked

if [ -d "$APP_BUNDLE" ]; then
    if [ -f "$APP_BIN" ]; then
        echo "==> Refreshing bundled binary from $APP_BIN"
        cp "$APP_BIN" "$APP_BUNDLE/Contents/MacOS/macrdp-ui"
    fi

    echo "==> Bundling Swift Concurrency runtime into $APP_BUNDLE"
    BUNDLE_ID="$BUNDLE_ID" "$SCRIPT_DIR/fix-swift-runtime.sh" "$APP_BUNDLE"

    echo "==> Repacking DMG at $DMG_PATH"
    "$SCRIPT_DIR/repack-dmg.sh" "$APP_BUNDLE" "$DMG_PATH"

    echo "==> Done. Identifier:"
    codesign -d --verbose=1 "$APP_BUNDLE" 2>&1 | grep Identifier
else
    echo "==> No .app bundle found at $APP_BUNDLE"
    echo "    Sign manually: codesign --force --sign - --identifier $BUNDLE_ID /path/to/macrdp.app"
fi

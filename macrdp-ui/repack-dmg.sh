#!/bin/bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
APP_BUNDLE="${1:-$SCRIPT_DIR/src-tauri/target/release/bundle/macos/macrdp.app}"
DMG_PATH="${2:-$SCRIPT_DIR/src-tauri/target/release/bundle/dmg/macrdp_0.1.0_aarch64.dmg}"
VOLNAME="${VOLNAME:-macrdp}"

if [ ! -d "$APP_BUNDLE" ]; then
  echo "App bundle not found: $APP_BUNDLE" >&2
  exit 1
fi

mkdir -p "$(dirname "$DMG_PATH")"
rm -f "$DMG_PATH"

hdiutil create \
  -volname "$VOLNAME" \
  -srcfolder "$APP_BUNDLE" \
  -ov \
  -format UDZO \
  "$DMG_PATH"

echo "Repacked DMG: $DMG_PATH"

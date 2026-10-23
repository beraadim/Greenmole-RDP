#!/bin/bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
APP_BUNDLE="${1:-$SCRIPT_DIR/src-tauri/target/release/bundle/macos/Greenmole RDP.app}"
DMG_PATH="${2:?Pass the destination DMG path}"
VOLNAME="${VOLNAME:-Greenmole RDP}"

if [ ! -d "$APP_BUNDLE" ]; then
  echo "App bundle not found: $APP_BUNDLE" >&2
  exit 1
fi

mkdir -p "$(dirname "$DMG_PATH")"

hdiutil create \
  -volname "$VOLNAME" \
  -srcfolder "$APP_BUNDLE" \
  -ov \
  -format UDZO \
  "$DMG_PATH"

echo "Repacked DMG: $DMG_PATH"

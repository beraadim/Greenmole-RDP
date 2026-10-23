#!/bin/bash
set -euo pipefail

APP_NAME="Greenmole RDP.app"
BUNDLE_ID="com.macrdp.app"
DEFAULT_DMG="$(find "$HOME/Desktop" -maxdepth 1 -type f \( -name 'Greenmole_RDP*.dmg' -o -name 'macrdp*.dmg' \) -print0 | xargs -0 ls -t 2>/dev/null | head -n1 || true)"
DMG_PATH="${1:-$DEFAULT_DMG}"
EXISTING_APP="$(osascript -e "POSIX path of (path to application id \"$BUNDLE_ID\")" 2>/dev/null | tr -d '\n' || true)"
APP_DEST="${EXISTING_APP:-$HOME/Applications/$APP_NAME}"

if [[ -z "${DMG_PATH:-}" || ! -f "$DMG_PATH" ]]; then
  echo "Usage: $0 /path/to/Greenmole_RDP.dmg" >&2
  echo "No Greenmole RDP DMG found on the Desktop." >&2
  exit 1
fi

MOUNT_DIR="$(mktemp -d /tmp/macrdp-update.XXXXXX)"
ATTACHED=0

cleanup() {
  if [[ "$ATTACHED" -eq 1 ]]; then
    hdiutil detach "$MOUNT_DIR" -force >/dev/null 2>&1 || true
  fi
  rmdir "$MOUNT_DIR" >/dev/null 2>&1 || true
}
trap cleanup EXIT

echo "==> Quitting running Greenmole RDP instances"
osascript -e "tell application id \"$BUNDLE_ID\" to quit" >/dev/null 2>&1 || true
pkill -x macrdp-ui >/dev/null 2>&1 || true
sleep 2

echo "==> Mounting $DMG_PATH"
hdiutil attach -nobrowse -mountpoint "$MOUNT_DIR" "$DMG_PATH" >/dev/null
ATTACHED=1

if [[ ! -d "$MOUNT_DIR/$APP_NAME" ]]; then
  echo "Mounted DMG does not contain $APP_NAME" >&2
  exit 1
fi

echo "==> Installing to $APP_DEST"
mkdir -p "$(dirname "$APP_DEST")"
rm -rf "$APP_DEST"
cp -R "$MOUNT_DIR/$APP_NAME" "$APP_DEST"
xattr -dr com.apple.quarantine "$APP_DEST" >/dev/null 2>&1 || true

echo "==> Launching $APP_DEST"
open -a "$APP_DEST"

for _ in {1..20}; do
  if pgrep -x macrdp-ui >/dev/null 2>&1; then
    echo "==> Greenmole RDP is running"
    exit 0
  fi
  sleep 1
done

echo "Greenmole RDP did not appear to start cleanly" >&2
exit 1

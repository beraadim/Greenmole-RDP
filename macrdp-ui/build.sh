#!/bin/bash
set -euo pipefail

BUNDLE_ID="${BUNDLE_ID:-com.macrdp.app}"
SIGNING_IDENTITY="${MACRDP_SIGNING_IDENTITY:--}"
NOTARY_PROFILE="${MACRDP_NOTARY_PROFILE:-}"
PUBLIC_RELEASE="${MACRDP_PUBLIC_RELEASE:-0}"
case "$PUBLIC_RELEASE" in
  0|1) ;;
  *) echo "MACRDP_PUBLIC_RELEASE must be 0 or 1" >&2; exit 1 ;;
esac
if [ "$PUBLIC_RELEASE" = 1 ]; then
  case "$SIGNING_IDENTITY" in
    "Developer ID Application:"*) ;;
    *) echo "Public releases require a Developer ID Application signing identity" >&2; exit 1 ;;
  esac
  [ -n "$NOTARY_PROFILE" ] || { echo "Public releases require MACRDP_NOTARY_PROFILE" >&2; exit 1; }
fi
if [ -n "$NOTARY_PROFILE" ] && [ "$SIGNING_IDENTITY" = - ]; then
  echo "Notarization requires a certificate-backed signing identity" >&2
  exit 1
fi
PROFILE="${MACRDP_CLI_PROFILE:-release}"
SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
case "$PROFILE" in
  release) TAURI_ARGS=(--no-sign --bundles app) ;;
  debug) TAURI_ARGS=(--debug --no-sign --bundles app) ;;
  *) echo "Unsupported MACRDP_CLI_PROFILE: $PROFILE" >&2; exit 1 ;;
esac

VERSION="$(node -p "require('$SCRIPT_DIR/src-tauri/tauri.conf.json').version")"
TARGET="$(rustc -vV | awk '/^host:/ { print $2 }')"
ARCH="${TARGET%%-*}"
REVISION="${MACRDP_BUILD_REVISION:-$(git -C "$ROOT" rev-parse --short=12 HEAD 2>/dev/null || printf unknown)}"
if [[ ! "$REVISION" =~ ^[A-Za-z0-9._-]+$ ]]; then
  echo "Invalid build revision" >&2
  exit 1
fi
export MACRDP_BUILD_REVISION="$REVISION"
export VITE_APP_VERSION="$VERSION"
export VITE_BUILD_REVISION="$REVISION"
APP_BUNDLE="$SCRIPT_DIR/src-tauri/target/$PROFILE/bundle/macos/macrdp.app"
DMG_PATH="$SCRIPT_DIR/src-tauri/target/$PROFILE/bundle/dmg/macrdp_${VERSION}_${ARCH}_${REVISION}.dmg"

cd "$ROOT"
if [ "$PROFILE" = release ]; then
  cargo build -p macrdp-server --release --locked
else
  cargo build -p macrdp-server --locked
fi
"$ROOT/tools/fix-swift-binary.sh" "$ROOT/target/$PROFILE/macrdp-server"
cd "$SCRIPT_DIR"
npm run tauri build -- "${TAURI_ARGS[@]}" -- --locked
[[ -d "$APP_BUNDLE" ]] || { echo "Missing application bundle: $APP_BUNDLE" >&2; exit 1; }

mkdir -p "$APP_BUNDLE/Contents/Resources"
node -e 'const fs=require("fs"); fs.writeFileSync(process.argv[1], JSON.stringify({version:process.env.VITE_APP_VERSION,revision:process.env.VITE_BUILD_REVISION,architecture:process.argv[2],built_at:new Date().toISOString()},null,2)+"\n")' "$APP_BUNDLE/Contents/Resources/build-info.json" "$ARCH"
BUNDLE_ID="$BUNDLE_ID" "$SCRIPT_DIR/fix-swift-runtime.sh" "$APP_BUNDLE"
codesign --verify --deep --strict "$APP_BUNDLE"
"$SCRIPT_DIR/repack-dmg.sh" "$APP_BUNDLE" "$DMG_PATH"
if [ "$SIGNING_IDENTITY" != - ]; then
  codesign --force --sign "$SIGNING_IDENTITY" --timestamp "$DMG_PATH"
  codesign --verify --strict "$DMG_PATH"
fi
if [ -n "$NOTARY_PROFILE" ]; then
  # Credentials stay in the build machine's Keychain, outside source and logs.
  xcrun notarytool submit "$DMG_PATH" --keychain-profile "$NOTARY_PROFILE" --wait
  xcrun stapler staple "$DMG_PATH"
  xcrun stapler validate "$DMG_PATH"
  spctl --assess --type open --context context:primary-signature "$DMG_PATH"
fi
hdiutil verify "$DMG_PATH"
(cd "$(dirname "$DMG_PATH")" && shasum -a 256 "$(basename "$DMG_PATH")") > "$DMG_PATH.sha256"
cp "$APP_BUNDLE/Contents/Resources/build-info.json" "$DMG_PATH.build-info.json"
printf 'Verified DMG: %s\n' "$DMG_PATH"

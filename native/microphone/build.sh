#!/bin/bash
set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
OUTPUT="${1:?Pass the application Resources/microphone directory}"
SIGNING_IDENTITY="${MACRDP_SIGNING_IDENTITY:--}"
DRIVER="$OUTPUT/GreenmoleMicrophone.driver"
mkdir -p "$DRIVER/Contents/MacOS"
xcrun clang -std=c11 -O2 -Wall -Wextra -Werror -fvisibility=hidden \
  -arch arm64 -arch x86_64 -mmacosx-version-min=14.0 -bundle \
  -framework CoreAudio -framework CoreFoundation \
  "$SCRIPT_DIR/GreenmoleMicrophone.c" "$SCRIPT_DIR/DriverProperties.c" -o "$DRIVER/Contents/MacOS/GreenmoleMicrophone"
install -m 644 "$SCRIPT_DIR/Info.plist" "$DRIVER/Contents/Info.plist"
install -m 644 "$SCRIPT_DIR/install-microphone-driver.sh" "$OUTPUT/install-microphone-driver.sh"
install -m 644 "$SCRIPT_DIR/uninstall-microphone-driver.sh" "$OUTPUT/uninstall-microphone-driver.sh"
if [ "$SIGNING_IDENTITY" = - ]; then
  codesign --force --sign - --identifier com.macrdp.microphone "$DRIVER"
else
  codesign --force --sign "$SIGNING_IDENTITY" --identifier com.macrdp.microphone --options runtime --timestamp "$DRIVER"
fi
digest() { /usr/bin/shasum -a 256 "$1" | /usr/bin/awk '{print $1}'; }
{
  digest "$DRIVER/Contents/MacOS/GreenmoleMicrophone"
  digest "$DRIVER/Contents/Info.plist"
  digest "$DRIVER/Contents/_CodeSignature/CodeResources"
} | /usr/bin/shasum -a 256 | /usr/bin/awk '{print $1}' > "$OUTPUT/driver.sha256"
codesign --verify --strict --requirement 'identifier "com.macrdp.microphone"' "$DRIVER"
printf 'Built input-only microphone driver: %s\n' "$DRIVER"

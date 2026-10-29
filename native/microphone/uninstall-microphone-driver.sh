#!/bin/sh
# Embedded into the UI executable; no caller-controlled removal path.
set -eu
PATH=/usr/bin:/bin:/usr/sbin:/sbin
export PATH
umask 077
fail() { echo "GM_MIC_ERROR:$1" >&2; exit 1; }
[ "$(id -u)" = 0 ] && [ "$#" = 0 ] || fail remove_failed
HAL=/Library/Audio/Plug-Ins/HAL
DEST="$HAL/GreenmoleMicrophone.driver"
[ -e "$DEST" ] || { [ ! -L "$DEST" ] || fail destination_invalid; echo GM_MIC_REMOVED; exit 0; }
for DIR in /Library /Library/Audio /Library/Audio/Plug-Ins "$HAL" "$DEST"; do
  [ -d "$DIR" ] && [ ! -L "$DIR" ] && [ "$(stat -f %u "$DIR")" = 0 ] || fail destination_invalid
  MODE=$(stat -f %Lp "$DIR")
  [ "$((0$MODE & 022))" = 0 ] || fail destination_invalid
  [ "$(ls -lde "$DIR" | wc -l | tr -d ' ')" = 1 ] || fail destination_invalid
done
codesign --verify --strict --requirement 'identifier "com.macrdp.microphone"' "$DEST" >/dev/null 2>&1 || fail destination_invalid
STAGE=$(mktemp -d "$HAL/.GreenmoleMicrophone.remove.XXXXXXXX") || fail remove_failed
rmdir "$STAGE"
mv "$DEST" "$STAGE" || fail remove_failed
rm -rf "$STAGE" || fail remove_failed
launchctl kickstart -k system/com.apple.audio.coreaudiod >/dev/null 2>&1 || fail audio_restart_failed
echo GM_MIC_REMOVED

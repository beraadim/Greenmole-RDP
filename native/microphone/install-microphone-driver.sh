#!/bin/sh
# This program is embedded into the UI executable and passed as literal text to
# the administrator shell. Never execute a script path in a writable app as root.
set -eu
PATH=/usr/bin:/bin:/usr/sbin:/sbin
export PATH
umask 077
fail() { echo "GM_MIC_ERROR:$1" >&2; exit 1; }
# These helpers are also exercised by the unprivileged installer regression test.
# They manipulate only the caller-provided private stage; privilege/path checks
# and signature verification remain in gm_install_main.
gm_stage_driver() {
  mkdir -m 700 "$2/Contents" "$2/Contents/MacOS" "$2/Contents/_CodeSignature" || return 1
  install -m 600 "$1/Contents/Info.plist" "$2/Contents/Info.plist" || return 1
  install -m 700 "$1/Contents/MacOS/GreenmoleMicrophone" "$2/Contents/MacOS/GreenmoleMicrophone" || return 1
  install -m 600 "$1/Contents/_CodeSignature/CodeResources" "$2/Contents/_CodeSignature/CodeResources" || return 1
}

gm_verify_driver_modes() {
  for gm_dir in "$1" "$1/Contents" "$1/Contents/MacOS" "$1/Contents/_CodeSignature"; do
    [ -d "$gm_dir" ] && [ ! -L "$gm_dir" ] || return 1
    [ "$(stat -f %Lp "$gm_dir")" = 755 ] && [ -r "$gm_dir" ] && [ -x "$gm_dir" ] || return 1
    [ "$(ls -lde "$gm_dir" | wc -l | tr -d ' ')" = 1 ] || return 1
  done
  for gm_file in "$1/Contents/Info.plist" "$1/Contents/_CodeSignature/CodeResources"; do
    [ -f "$gm_file" ] && [ ! -L "$gm_file" ] || return 1
    [ "$(stat -f %Lp "$gm_file")" = 644 ] && [ -r "$gm_file" ] || return 1
    [ "$(ls -lde "$gm_file" | wc -l | tr -d ' ')" = 1 ] || return 1
  done
  gm_binary="$1/Contents/MacOS/GreenmoleMicrophone"
  [ -f "$gm_binary" ] && [ ! -L "$gm_binary" ] || return 1
  [ "$(stat -f %Lp "$gm_binary")" = 755 ] && [ -r "$gm_binary" ] && [ -x "$gm_binary" ] || return 1
  [ "$(ls -lde "$gm_binary" | wc -l | tr -d ' ')" = 1 ] || return 1
}

gm_finalize_driver() {
  chmod 755 "$1" "$1/Contents" "$1/Contents/MacOS" "$1/Contents/_CodeSignature" || return 1
  chmod 644 "$1/Contents/Info.plist" "$1/Contents/_CodeSignature/CodeResources" || return 1
  chmod 755 "$1/Contents/MacOS/GreenmoleMicrophone" || return 1
  gm_verify_driver_modes "$1"
}

gm_install_main() {
  [ "$(id -u)" = 0 ] || fail install_failed
  [ "$#" = 2 ] || fail source_invalid
  SOURCE=$1
  EXPECTED=$2
  case "$EXPECTED" in ''|*[!0-9a-f]*) fail signature_invalid ;; esac
  [ "${#EXPECTED}" = 64 ] || fail signature_invalid
  case "$SOURCE" in /*/GreenmoleMicrophone.driver) ;; *) fail source_invalid ;; esac
  [ -d "$SOURCE" ] && [ ! -L "$SOURCE" ] || fail source_missing
  [ -f "$SOURCE/Contents/Info.plist" ] && [ ! -L "$SOURCE/Contents/Info.plist" ] || fail source_invalid
  [ -f "$SOURCE/Contents/MacOS/GreenmoleMicrophone" ] && [ ! -L "$SOURCE/Contents/MacOS/GreenmoleMicrophone" ] || fail source_invalid
  [ -f "$SOURCE/Contents/_CodeSignature/CodeResources" ] && [ ! -L "$SOURCE/Contents/_CodeSignature/CodeResources" ] || fail source_invalid
  HAL=/Library/Audio/Plug-Ins/HAL
  # Every privileged destination component must already be root-owned and deny
  # writes to other users. Create only missing, fixed components beneath /Library.
  for DIR in /Library /Library/Audio /Library/Audio/Plug-Ins "$HAL"; do
    [ ! -L "$DIR" ] || fail destination_invalid
    if [ ! -d "$DIR" ]; then mkdir -m 755 "$DIR" || fail destination_invalid; fi
    [ "$(stat -f %u "$DIR")" = 0 ] || fail destination_invalid
    MODE=$(stat -f %Lp "$DIR")
    [ "$((0$MODE & 022))" = 0 ] || fail destination_invalid
    # ACLs could grant writes even when the POSIX mode is safe.
    [ "$(ls -lde "$DIR" | wc -l | tr -d ' ')" = 1 ] || fail destination_invalid
  done
  DEST="$HAL/GreenmoleMicrophone.driver"
  if [ -e "$DEST" ] || [ -L "$DEST" ]; then
    [ -d "$DEST" ] && [ ! -L "$DEST" ] && [ "$(stat -f %u "$DEST")" = 0 ] || fail destination_invalid
    codesign --verify --strict --requirement 'identifier "com.macrdp.microphone"' "$DEST" >/dev/null 2>&1 || fail destination_invalid
  fi
  STAGE=$(mktemp -d "$HAL/.GreenmoleMicrophone.stage.XXXXXXXX") || fail stage_failed
  BACKUP=
  cleanup() {
    if [ -n "$BACKUP" ] && [ -d "$BACKUP" ] && [ ! -e "$DEST" ]; then mv "$BACKUP" "$DEST" || true; fi
    if [ -n "$STAGE" ] && [ -d "$STAGE" ]; then rm -rf "$STAGE"; fi
  }
  trap cleanup EXIT
  trap 'exit 1' HUP INT TERM
  # Copy only fixed files into a private root-owned stage. A source replacement
  # cannot redirect writes or disclose privileged files: validate before publishing.
  gm_stage_driver "$SOURCE" "$STAGE" || fail stage_failed
  digest() { /usr/bin/shasum -a 256 "$1" | /usr/bin/awk '{print $1}'; }
  ACTUAL=$({
    digest "$STAGE/Contents/MacOS/GreenmoleMicrophone"
    digest "$STAGE/Contents/Info.plist"
    digest "$STAGE/Contents/_CodeSignature/CodeResources"
  } | /usr/bin/shasum -a 256 | /usr/bin/awk '{print $1}')
  [ "$ACTUAL" = "$EXPECTED" ] || fail signature_invalid
  [ "$(/usr/libexec/PlistBuddy -c 'Print :CFBundleIdentifier' "$STAGE/Contents/Info.plist" 2>/dev/null)" = com.macrdp.microphone ] || fail source_invalid
  [ "$(/usr/libexec/PlistBuddy -c 'Print :CFBundleExecutable' "$STAGE/Contents/Info.plist" 2>/dev/null)" = GreenmoleMicrophone ] || fail source_invalid
  [ "$(/usr/libexec/PlistBuddy -c 'Print :CFBundleVersion' "$STAGE/Contents/Info.plist" 2>/dev/null)" = 1 ] || fail source_invalid
  # Reconstructing the fixed signed bundle retains the code signature;
  # unexpected resources or symlinks from the source bundle are never installed.
  codesign --verify --strict --requirement 'identifier "com.macrdp.microphone"' "$STAGE" >/dev/null 2>&1 || fail signature_invalid
  # The HAL runs under _coreaudiod, so its loader needs the other-user read and
  # traversal bits. The stage itself has deliberately been private until now.
  gm_finalize_driver "$STAGE" || fail stage_failed
  if [ -d "$DEST" ]; then
    BACKUP=$(mktemp -d "$HAL/.GreenmoleMicrophone.previous.XXXXXXXX") || fail stage_failed
    rmdir "$BACKUP"
    mv "$DEST" "$BACKUP" || fail install_failed
  fi
  mv "$STAGE" "$DEST" || fail install_failed
  STAGE=
  gm_verify_driver_modes "$DEST" || fail install_failed
  if [ -n "$BACKUP" ]; then rm -rf "$BACKUP"; BACKUP=; fi
  launchctl kickstart -k system/com.apple.audio.coreaudiod >/dev/null 2>&1 || fail audio_restart_failed
  echo GM_MIC_INSTALLED
}

gm_install_main "$@"

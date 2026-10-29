#!/bin/sh
# Exercise the production staging and permission helpers under an ordinary UID.
# Never invoke the privileged installer main, access HAL, or restart audio.
set -eu
SCRIPT_DIR=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
TEST_DIR=$(mktemp -d "${TMPDIR:-/tmp}/greenmole-microphone-permissions.XXXXXXXX")
trap 'rm -rf "$TEST_DIR"' EXIT
trap 'exit 1' HUP INT TERM

INSTALLER="$SCRIPT_DIR/install-microphone-driver.sh"
[ "$(tail -n 1 "$INSTALLER")" = 'gm_install_main "$@"' ] || {
  echo 'Installer main invocation changed; refusing to source the script.' >&2
  exit 1
}
# Source the shipped functions verbatim, excluding only the unconditional main
# invocation. Production has no test mode or privilege-check bypass.
sed '$d' "$INSTALLER" > "$TEST_DIR/helpers.sh"
. "$TEST_DIR/helpers.sh"

assert_mode() {
  actual=$(stat -f %Lp "$1")
  [ "$actual" = "$2" ] || {
    echo "Unexpected mode $actual for $1; expected $2" >&2
    exit 1
  }
}
assert_rejected() {
  if gm_verify_driver_modes "$DESTINATION"; then
    echo 'Permission verifier accepted an unsafe bundle.' >&2
    exit 1
  fi
}

SOURCE_FIXTURE="$TEST_DIR/source with spaces/GreenmoleMicrophone.driver"
STAGE_FIXTURE="$TEST_DIR/private stage"
DESTINATION="$TEST_DIR/published driver"
mkdir -p "$SOURCE_FIXTURE/Contents/MacOS" "$SOURCE_FIXTURE/Contents/_CodeSignature"
echo 'synthetic plist' > "$SOURCE_FIXTURE/Contents/Info.plist"
echo 'synthetic binary' > "$SOURCE_FIXTURE/Contents/MacOS/GreenmoleMicrophone"
echo 'synthetic seal' > "$SOURCE_FIXTURE/Contents/_CodeSignature/CodeResources"
chmod 644 "$SOURCE_FIXTURE/Contents/Info.plist" "$SOURCE_FIXTURE/Contents/MacOS/GreenmoleMicrophone" "$SOURCE_FIXTURE/Contents/_CodeSignature/CodeResources"
mkdir -m 700 "$STAGE_FIXTURE"

gm_stage_driver "$SOURCE_FIXTURE" "$STAGE_FIXTURE"
for directory in "$STAGE_FIXTURE" "$STAGE_FIXTURE/Contents" "$STAGE_FIXTURE/Contents/MacOS" "$STAGE_FIXTURE/Contents/_CodeSignature"; do
  assert_mode "$directory" 700
done
assert_mode "$STAGE_FIXTURE/Contents/Info.plist" 600
assert_mode "$STAGE_FIXTURE/Contents/MacOS/GreenmoleMicrophone" 700
assert_mode "$STAGE_FIXTURE/Contents/_CodeSignature/CodeResources" 600

gm_finalize_driver "$STAGE_FIXTURE"
mv "$STAGE_FIXTURE" "$DESTINATION"
gm_verify_driver_modes "$DESTINATION"
for directory in "$DESTINATION" "$DESTINATION/Contents" "$DESTINATION/Contents/MacOS" "$DESTINATION/Contents/_CodeSignature"; do
  assert_mode "$directory" 755
done
assert_mode "$DESTINATION/Contents/Info.plist" 644
assert_mode "$DESTINATION/Contents/MacOS/GreenmoleMicrophone" 755
assert_mode "$DESTINATION/Contents/_CodeSignature/CodeResources" 644
cmp "$SOURCE_FIXTURE/Contents/MacOS/GreenmoleMicrophone" "$DESTINATION/Contents/MacOS/GreenmoleMicrophone"

# Regression: the old installer left its binary 0700, unreadable by _coreaudiod.
chmod 700 "$DESTINATION/Contents/MacOS/GreenmoleMicrophone"
assert_rejected
gm_finalize_driver "$DESTINATION"
chmod 600 "$DESTINATION/Contents/Info.plist"
assert_rejected
gm_finalize_driver "$DESTINATION"
chmod 700 "$DESTINATION/Contents/MacOS"
assert_rejected
gm_finalize_driver "$DESTINATION"
chmod 777 "$DESTINATION/Contents/_CodeSignature"
assert_rejected
gm_finalize_driver "$DESTINATION"

mv "$DESTINATION/Contents/_CodeSignature/CodeResources" "$TEST_DIR/original seal"
ln -s "$TEST_DIR/original seal" "$DESTINATION/Contents/_CodeSignature/CodeResources"
assert_rejected
rm "$DESTINATION/Contents/_CodeSignature/CodeResources"
mv "$TEST_DIR/original seal" "$DESTINATION/Contents/_CodeSignature/CodeResources"
gm_verify_driver_modes "$DESTINATION"
echo 'microphone installer staging, published modes and unsafe-mode rejection tests passed'

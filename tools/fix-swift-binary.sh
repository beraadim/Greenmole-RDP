#!/bin/bash
set -euo pipefail
BINARY="${1:?Pass a Mach-O executable path}"
[[ -f "$BINARY" ]] || { echo "Missing executable: $BINARY" >&2; exit 1; }

# macOS 14+ supplies this library through the dyld cache, even without an
# on-disk stub. Loading the Command Line Tools copy alongside it is unsafe.
if otool -L "$BINARY" | grep -q '@rpath/libswift_Concurrency.dylib'; then
  install_name_tool -change '@rpath/libswift_Concurrency.dylib' \
    '/usr/lib/swift/libswift_Concurrency.dylib' "$BINARY"
fi
while IFS= read -r rpath; do
  case "$rpath" in
    */CommandLineTools/usr/lib/swift*|*/Toolchains/*/usr/lib/swift*)
      install_name_tool -delete_rpath "$rpath" "$BINARY" ;;
  esac
done < <(otool -l "$BINARY" | awk '$1 == "cmd" && $2 == "LC_RPATH" { r=1; next } r && $1 == "path" { print $2; r=0 }')
codesign --force --sign - "$BINARY"
codesign --verify --strict "$BINARY"

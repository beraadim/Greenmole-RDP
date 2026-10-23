#!/bin/bash
# Derive application icons from the unchanged Greenmole mascot artwork.
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
SOURCE="$ROOT/branding/greenmole.png"
OUTPUT="$ROOT/macrdp-ui/src-tauri/icons"
STAGING="$(mktemp -d "${TMPDIR:-/tmp}/greenmole-icons.XXXXXX")"
trap 'rm -rf "$STAGING"' EXIT
mkdir -p "$STAGING/Greenmole.iconset" "$OUTPUT"

for size in 16 32 128 256 512; do
  sips -z "$size" "$size" "$SOURCE" --out "$STAGING/Greenmole.iconset/icon_${size}x${size}.png" >/dev/null
  double=$((size * 2))
  sips -z "$double" "$double" "$SOURCE" --out "$STAGING/Greenmole.iconset/icon_${size}x${size}@2x.png" >/dev/null
done
iconutil -c icns "$STAGING/Greenmole.iconset" -o "$OUTPUT/icon.icns"
cp "$OUTPUT/icon.icns" "$ROOT/icon.icns"
cp "$STAGING/Greenmole.iconset/icon_32x32.png" "$OUTPUT/32x32.png"
cp "$STAGING/Greenmole.iconset/icon_128x128.png" "$OUTPUT/128x128.png"
cp "$STAGING/Greenmole.iconset/icon_128x128@2x.png" "$OUTPUT/128x128@2x.png"
cp "$STAGING/Greenmole.iconset/icon_512x512@2x.png" "$OUTPUT/icon.png"
cp "$OUTPUT/32x32.png" "$OUTPUT/tray.png"

# ICO supports PNG payloads. Keep a real multi-resolution ICO for other Tauri
# targets, rather than renaming a PNG with an .ico extension.
python3 - "$STAGING/Greenmole.iconset" "$OUTPUT/icon.ico" <<'PY'
from pathlib import Path
import struct
import sys

icons = Path(sys.argv[1])
frames = [(size, (icons / name).read_bytes()) for size, name in [
    (16, "icon_16x16.png"), (32, "icon_32x32.png"),
    (64, "icon_32x32@2x.png"), (128, "icon_128x128.png"),
    (256, "icon_128x128@2x.png"),
]]
header = struct.pack("<HHH", 0, 1, len(frames))
entries = bytearray()
offset = len(header) + 16 * len(frames)
for size, payload in frames:
    entries.extend(struct.pack("<BBBBHHII", size % 256, size % 256, 0, 0,
                               1, 32, len(payload), offset))
    offset += len(payload)
Path(sys.argv[2]).write_bytes(header + entries + b"".join(p for _, p in frames))
PY
chmod 644 "$ROOT/icon.icns" "$OUTPUT/"*.png "$OUTPUT/icon.icns" "$OUTPUT/icon.ico"
echo "Greenmole RDP icons rebuilt from branding/greenmole.png"

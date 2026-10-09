#!/bin/bash
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
BINARY="${1:?Pass the test executable}"
shift
"$ROOT/tools/fix-swift-binary.sh" "$BINARY"
exec "$BINARY" "$@"

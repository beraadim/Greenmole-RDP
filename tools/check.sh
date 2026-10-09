#!/bin/bash
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"
export CARGO_TARGET_AARCH64_APPLE_DARWIN_RUNNER="$ROOT/tools/run-native-test.sh"
export CARGO_TARGET_X86_64_APPLE_DARWIN_RUNNER="$ROOT/tools/run-native-test.sh"
cargo test --workspace --lib --bins --locked
cargo test --manifest-path macrdp-ui/src-tauri/Cargo.toml --lib --locked
cargo build -p macrdp-server --locked
(cd macrdp-ui && npm run build)
echo "All checks passed."

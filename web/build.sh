#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROJECT_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
WASM_CRATE="$PROJECT_DIR/crates/stet-wasm"
PKG_DIR="$SCRIPT_DIR/pkg"

echo "Building stet-wasm..."
wasm-pack build --target web --release "$WASM_CRATE"

echo "Copying pkg to web/pkg/..."
rm -rf "$PKG_DIR"
mkdir -p "$PKG_DIR"
cp "$WASM_CRATE/pkg/stet_wasm_bg.wasm" \
   "$WASM_CRATE/pkg/stet_wasm.js" \
   "$PKG_DIR/"

echo "Done. Serve with: cd web && python3 serve.py"

#!/bin/sh
# Build the browser version into web/ (index.html + pkg/). Serve web/ over HTTP to play, e.g.
#   python3 -m http.server -d web 8080
# Needs: rustup target add wasm32-unknown-unknown; cargo install wasm-bindgen-cli (same version
# as the wasm-bindgen crate in Cargo.lock); web/manifests.txt from scripts/web-manifests.sh.
set -e
cd "$(dirname "$0")/.."
export CARGO_TARGET_DIR=${CARGO_TARGET_DIR:-target-web}
nice -n 19 cargo build --profile web -p riptide --target wasm32-unknown-unknown
wasm-bindgen --target web --no-typescript --out-dir web/pkg "$CARGO_TARGET_DIR/wasm32-unknown-unknown/web/riptide.wasm"
ls -lh web/pkg/riptide_bg.wasm

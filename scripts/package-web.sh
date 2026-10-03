#!/bin/sh
# Stage only the browser engine and page, never the player's game files.
# Usage: scripts/package-web.sh [output directory]
set -eu
cd "$(dirname "$0")/.."
dest=${1:-out/site}
test -f web/pkg/riptide.js
test -f web/pkg/riptide_bg.wasm
mkdir -p "$dest/pkg"
build_id=$(sha256sum web/pkg/riptide_bg.wasm | cut -c1-16)
sed -e "s|./pkg/riptide.js\"|./pkg/riptide.js?v=$build_id\"|" \
    -e "s|./engine.js\"|./engine.js?v=$build_id\"|" web/index.html > "$dest/index.html"
cp web/engine.js "$dest/"
cp web/pkg/riptide.js "$dest/pkg/"
gzip -9 -n -c web/pkg/riptide_bg.wasm > "$dest/pkg/riptide_bg.wasm.gz"
touch "$dest/.nojekyll"
printf 'Static site ready in %s\n' "$dest"
ls -lh "$dest/pkg/riptide_bg.wasm.gz"

#!/bin/sh
# Launch Riptide (the newest of the release / dev builds). Override data paths with
# RIPTIDE_LUX / RIPTIDE_GDI.
cd "$(dirname "$0")" || exit 1
bin=target/release/riptide
[ target-dev/release/riptide -nt "$bin" ] && bin=target-dev/release/riptide
exec "$bin" "$@"

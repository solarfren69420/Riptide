#!/bin/sh
# Run a heavy job without lagging the desktop, the stream or a game being played: pinned to the
# last LIGHT_CORES cores (default 2), lowest CPU priority, idle disk priority, and cargo limited to
# that many jobs. Child processes (Ghidra's decompilers, rustc) inherit all of it.
n=${LIGHT_CORES:-2}
last=$(($(nproc) - 1))
first=$((last - n + 1))
export CARGO_BUILD_JOBS=$n
exec taskset -c "$first-$last" nice -n 19 ionice -c 3 "$@"

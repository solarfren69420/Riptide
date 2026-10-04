#!/bin/sh
# Two headless autopilot games race each other in real time (20 frames of 0.05 s a second) through
# a local relay (riptide-server).
#   scripts/online-check.sh [track_id]     (default lost_island)
# Output: each game's ONLINE lines (room, go, results) and RESULT line. Logs in out/online-check/.
cd "$(dirname "$0")/.." || exit 1
out=${OUT:-out/online-check}
mkdir -p "$out"
track=${1:-lost_island}
port=${PORT:-3121}
icd=$(ls /usr/share/vulkan/icd.d/lvp_icd*.json 2>/dev/null | head -1)
RIPTIDE_RELAY_ADDR=127.0.0.1:$port "${SERVER:-target/release/riptide-server}" > "$out/server.log" 2>&1 &
server=$!
sleep 0.5
game() { # name mode [boat] (default: the first boat, like the course chart)
    RIPTIDE_SERVER=ws://127.0.0.1:$port RIPTIDE_NAME=$1 RIPTIDE_ONLINE=$2 RIPTIDE_BOAT=$3 \
    VK_ICD_FILENAMES="$icd" LP_NUM_THREADS=2 XDG_CONFIG_HOME="$out/cfg-$1" \
    RIPTIDE_SHOT="$out/$1" RIPTIDE_SHOT_SIZE=320x180 RIPTIDE_SIM_DT=0.05 RIPTIDE_EXIT_ON_FINISH=1 \
    RIPTIDE_TRACK="$track" RIPTIDE_SHOT_FRAMES="${FRAMES:-24000}" \
        timeout 1800 nice -n 19 "${BIN:-target/release/riptide}" > "$out/$1.log" 2>&1
}
game host host:2 "$HOST_BOAT" &
host=$!
i=0
until code=$(grep -o 'ONLINE room [A-Z]*' "$out/host.log" 2>/dev/null | awk '{print $3}') && [ -n "$code" ]; do
    i=$((i + 1)); [ $i -gt 600 ] && { echo "no room"; kill $host $server; exit 1; }; sleep 0.5
done
game guest "join:$code" "$GUEST_BOAT" &
guest=$!
wait $host; wait $guest
kill $server
for g in host guest; do grep -ho 'ONLINE.*\|RESULT.*' "$out/$g.log" | sed "s/^/$g: /"; done

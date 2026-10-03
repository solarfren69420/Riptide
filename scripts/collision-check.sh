#!/bin/sh
# Collision check: the player's boat drives the centre of each course's racing line (no time
# limit, headless, silent) and RIPTIDE_PROBE reports how far it got, wall contacts, seconds
# stalled, launches over physics.probe_launch, the highest air, and wall crossings.
#   scripts/collision-check.sh classic|parry [track_id ...]   (BIN, OUT, FRAMES override)
cd "$(dirname "$0")/.." || exit 1
mode=${1:-classic}; shift
out=${OUT:-out/check-$mode}
mkdir -p "$out/cfg/riptide"
printf 'no_time_limit\n' > "$out/cfg/riptide/cheats.txt"
tracks="$*"
[ -n "$tracks" ] || tracks=$(awk -F, 'NR>2 && $7=="ok" && $8=="ok" {print $1}' sheets/tracks.csv)
icd=$(ls /usr/share/vulkan/icd.d/lvp_icd*.json 2>/dev/null | head -1)
for t in $tracks; do
    log="$out/$t.log"
    RIPTIDE_PROBE=1 RIPTIDE_COLLISION="$mode" RIPTIDE_TEST_LANE=0.5 VK_ICD_FILENAMES="$icd" LP_NUM_THREADS=2 \
    XDG_CONFIG_HOME="$out/cfg" RIPTIDE_SHOT="$out/$t" RIPTIDE_SHOT_SIZE=160x90 RIPTIDE_SIM_DT=0.05 \
    RIPTIDE_EXIT_ON_FINISH=1 RIPTIDE_TRACK="$t" RIPTIDE_SHOT_FRAMES="${FRAMES:-3000}" \
        timeout 900 nice -n 19 "${BIN:-target/release/riptide}" > "$log" 2>&1
    printf '%-20s %s\n' "$t" "$(grep -a -o 'PROBE.*' "$log" | tail -1 | sed 's/^PROBE //')"
done

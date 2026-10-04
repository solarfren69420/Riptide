#!/bin/sh
# Race every playable track (sheets/tracks.csv: racing_line and geometry ok) under the
# autopilot, headless on lavapipe at low priority, and report each result.
#   scripts/race-all.sh [track_id ...]      (default: all playable tracks)
# BIN=path overrides the game binary. Output: <track> FINISHED <place> <time> | DNF <furthest segment>   (logs in out/race-all/)
cd "$(dirname "$0")/.." || exit 1
out=${OUT:-out/race-all}
mkdir -p "$out"
tracks="$*"
[ -n "$tracks" ] || tracks=$(awk -F, 'NR>2 && $7=="ok" && $8=="ok" {print $1}' sheets/tracks.csv)
icd=$(ls /usr/share/vulkan/icd.d/lvp_icd*.json 2>/dev/null | head -1)
for t in $tracks; do
    log="$out/$t.log"
    RIPTIDE_DEBUG=1 VK_ICD_FILENAMES="$icd" LP_NUM_THREADS=2 XDG_CONFIG_HOME="$out/cfg" \
    RIPTIDE_SHOT="$out/$t" RIPTIDE_SHOT_SIZE=320x180 RIPTIDE_SIM_DT=0.05 RIPTIDE_EXIT_ON_FINISH=1 \
    RIPTIDE_TRACK="$t" RIPTIDE_SHOT_FRAMES="${FRAMES:-9000}" \
        timeout 1800 scripts/light.sh "${BIN:-target/release/riptide}" > "$log" 2>&1
    if grep -q RESULT "$log"; then
        echo "$t $(grep -o 'RESULT.*' "$log" | head -1)"
    else
        echo "$t DNF furthest seg $(grep -o 'seg [0-9]*' "$log" | awk '{if ($2 > m) m = $2} END {print m + 0}')"
    fi
done

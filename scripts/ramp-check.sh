#!/bin/sh
# Drive the player over every ramp on a course (headless, autopilot) and report each one.
#   scripts/ramp-check.sh <track> [first] [last]
# Output per ramp: RAMP TEST <i>: rise, wall contacts, clipped (units below the surface),
# launch climb rate, peak over the top, airtime, speed, passed / did not get past.
cd "$(dirname "$0")/.." || exit 1
t=$1; first=${2:-0}
out=out/ramp-check/$t; mkdir -p "$out"
icd=$(ls /usr/share/vulkan/icd.d/lvp_icd*.json 2>/dev/null | head -1)
run() {
    RIPTIDE_TEST_RAMP=$1 RIPTIDE_RAMPS=1 VK_ICD_FILENAMES="$icd" XDG_CONFIG_HOME="$out/cfg" RIPTIDE_SHOT="$out/x" \
    RIPTIDE_SHOT_SIZE=64x36 RIPTIDE_SIM_DT=0.05 RIPTIDE_SHOT_FRAMES=100000 RIPTIDE_TRACK="$t" \
        LP_NUM_THREADS=2 timeout 300 scripts/light.sh "${BIN:-target/release/riptide}" 2>&1 | sed 's/\x1b\[[0-9;]*m//g'
}
count=$(run 99999 | grep -c 'RAMP [0-9]')
last=${3:-$((count - 1))}
for i in $(seq "$first" "$last"); do
    run "$i" | grep -o 'RAMP TEST.*'
done

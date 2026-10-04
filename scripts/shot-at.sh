#!/bin/sh
# Render the view from a spot on a course (e.g. a STUCK line from the probe), boat parked there.
#   scripts/shot-at.sh <track> <x> <y> <z> <forward_x> <forward_z> [out.png]
# Yaw comes from the forward vector the probe prints. Output: out/shot-at/<track>_*.png
cd "$(dirname "$0")/.." || exit 1
mkdir -p out/shot-at
yaw=$(awk -v fx="$5" -v fz="$6" 'BEGIN { printf "%.1f", atan2(-fx, -fz) * 180 / 3.14159265 }')
icd=$(ls /usr/share/vulkan/icd.d/lvp_icd*.json 2>/dev/null | head -1)
RIPTIDE_SHOT_AT="$2,$3,$4,$yaw" VK_ICD_FILENAMES="$icd" XDG_CONFIG_HOME=out/shot-at/cfg \
RIPTIDE_SHOT="out/shot-at/$1" RIPTIDE_SHOT_SIZE=640x360 RIPTIDE_SHOT_FRAMES=${SHOT_FRAME:-20},$((${SHOT_FRAME:-20}+20)) RIPTIDE_TRACK="$1" \
    timeout 600 nice -n 19 "${BIN:-target/release/riptide}" > out/shot-at/$1.log 2>&1
ls out/shot-at/$1_${SHOT_FRAME:-20}.png

#!/bin/sh
# Record which triton.lux entries each course (and each boat) reads, for the browser build:
# web/manifests.txt holds "<track id> <entry>" lines and "boats <entry>" lines. Only entry
# names are written, never game data. Runs headless on lavapipe at low priority.
#   scripts/web-manifests.sh            (BIN=path overrides the game binary)
cd "$(dirname "$0")/.." || exit 1
bin=${BIN:-target/release/riptide}
tmp=$(mktemp -d)
icd=$(ls /usr/share/vulkan/icd.d/lvp_icd*.json 2>/dev/null | head -1)
run() { # run <record file> <env...>
    out=$1; shift
    env RIPTIDE_RECORD="$out" VK_ICD_FILENAMES="$icd" LP_NUM_THREADS=4 XDG_CONFIG_HOME="$tmp/cfg" \
        RIPTIDE_SHOT="$tmp/shot" RIPTIDE_SHOT_SIZE=160x90 RIPTIDE_SIM_DT=0.05 "$@" \
        timeout 900 nice -n 19 "$bin" > /dev/null 2>&1
}
: > web/manifests.txt.new
for t in $(awk -F, 'NR>2 && $7=="ok" && $8=="ok" {print $1}' sheets/tracks.csv); do
    run "$tmp/$t" RIPTIDE_TRACK="$t" RIPTIDE_SHOT_FRAMES=160
    sed "s/^/$t /" "$tmp/$t" >> web/manifests.txt.new
    echo "$t: $(wc -l < "$tmp/$t") entries"
done
awk -F, 'NR>2 && $11=="ok" {print $3}' sheets/boats.csv | while read -r b; do
    run "$tmp/boat" RIPTIDE_SHOT_MENU=1 RIPTIDE_BOAT="$b" RIPTIDE_SHOT_FRAMES=40
    cat "$tmp/boat" >> "$tmp/boats"; echo >> "$tmp/boats"
done
grep -v '^$' "$tmp/boats" | sort -u | sed 's/^/boats /' >> web/manifests.txt.new
mv web/manifests.txt.new web/manifests.txt
echo "boats: $(grep -c '^boats ' web/manifests.txt) entries; total $(wc -l < web/manifests.txt) lines"
rm -rf "$tmp"

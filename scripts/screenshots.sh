#!/usr/bin/env bash
# The README's screenshots of the demo session: the Editing, Mixing, MIDI
# and Mastering workspaces, the nine skins and Lumen.
#
#   cargo build --release && scripts/screenshots.sh
#
# Opens FaderFrame (maximized, on the monitor GNOME/KDE put it on: its size
# is the screenshots' size) with settings of its own (portable mode in a
# temporary folder, nothing of yours is read or changed), the dummy audio
# device and the demo session playing; then quits. Needs ImageMagick 7
# (`magick`), and pngquant and oxipng to make the files small.
# The other screenshots need imported files or hand-made set-ups.
set -euo pipefail

root=$(cd "$(dirname "$0")/.." && pwd)
dest="$root/docs/screenshots"
bin="$root/target/release/faderframe"
[ -x "$bin" ] || { echo "build first: cargo build --release" >&2; exit 1; }

work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
mkdir -p "$work/app/FaderFrame Data" "$work/shots"
cp "$bin" "$work/app/"

run() { # <screenshot pattern> <actions>
    (cd "$work/app" && FADERFRAME_SCREENSHOT="$1" FADERFRAME_STARTUP_ACTIONS="$2" \
        timeout 180 ./faderframe --demo --backend dummy >> "$work/log" 2>&1)
}

start="wait:3000,maximize,wait:1500"
editing="workspace-2,wait:800,zoom:fit,select-track:Pad,view-modulators"
run "$work/shots/ws{n}.png" "$start,$editing,play,wait:5000,screenshot,\
workspace-3,wait:2500,screenshot,workspace-4,wait:2500,screenshot,\
workspace-5,wait:3000,screenshot,stop,wait:500,quit-discard"

ids=(studio vintage daylight midnight frost fjord neon contrast lumen)
names=("Studio" "Vintage Console" "Daylight" "Midnight" "Frost" "Fjord" "Neon" "High Contrast" "Lumen")
skins="$start,workspace-3,wait:800,zoom:fit,play,wait:3000"
for id in "${ids[@]}"; do skins="$skins,theme:$id,wait:2500,screenshot"; done
# Lumen's Editing workspace last.
run "$work/shots/skin{n}.png" "$skins,$editing,wait:2500,screenshot,stop,wait:300,quit-discard"

n=$(ls "$work/shots" | wc -l)
[ "$n" -eq 14 ] || { echo "only $n screenshots; see $work/log" >&2; cat "$work/log" >&2; exit 1; }

cp "$work/shots/ws1.png" "$dest/editing.png"
cp "$work/shots/ws2.png" "$dest/mixing.png"
cp "$work/shots/ws3.png" "$dest/piano-roll.png"
cp "$work/shots/ws4.png" "$dest/mastering.png"
tiles=()
for i in "${!ids[@]}"; do
    magick "$work/shots/skin$((i + 1)).png" -resize 1000x -background '#111214' \
        -gravity south -splice 0x56 -font DejaVu-Sans -pointsize 30 -fill '#e8e4dc' \
        -annotate +0+10 "${names[$i]}" "$work/tile$i.png"
    tiles+=("$work/tile$i.png")
done
magick montage "${tiles[@]}" -tile 3x3 -geometry +12+12 -background '#111214' -depth 8 \
    "$dest/themes.png"
magick "$work/shots/skin10.png" "$work/shots/skin9.png" -resize 1282x +append -depth 8 \
    "$dest/lumen.png"

cd "$dest"
files=(editing.png mixing.png piano-roll.png mastering.png themes.png lumen.png)
pngquant --force --ext .png --quality 80-95 --speed 1 "${files[@]}"
oxipng -q -o 4 --strip safe "${files[@]}"
ls -l "${files[@]}"

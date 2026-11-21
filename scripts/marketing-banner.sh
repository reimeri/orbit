#!/usr/bin/env bash
# Compose the README banner from the supplied artwork.
set -euo pipefail
if (( $# > 2 )); then
    echo "Usage: $0 [SCREENSHOT.png [BANNER.png]]" >&2
    exit 2
fi
shot=${1:-target/orbit-marketing.png}
output=${2:-assets/marketing/orbit-banner.png}
art=assets/marketing

if [[ ! -f "$shot" ]]; then
    echo "Missing screenshot: $shot. Run 'just marketing-shot' or pass an existing capture." >&2
    exit 1
fi

# Round the resized window before making its shadow, so the shadow follows its outline.
round_window='roundrectangle 0,0 %[fx:w-1],%[fx:h-1] 18,18'
magick -respect-parentheses "$art/marketing-bg.png" \
    -resize '1672x1041^' -gravity center -extent 1672x1041 +repage \
    \( "$art/title.png" -resize 420x150 \) -gravity north -geometry +0+48 -composite +geometry \
    \( "$shot" -resize 1228x768 +repage \
       \( +clone -alpha off -fill black -colorize 100 -fill white -draw "$round_window" \) \
       -alpha off -compose CopyOpacity -composite -compose Over \
       -fill none -stroke '#81909c' -strokewidth 1 -draw "$round_window" \
       \( +clone -background black -shadow 60x12+0+10 \) \
       +swap -background none -layers merge +repage \) \
    -gravity north -geometry +0+196 -composite +geometry \
    \( +clone -alpha off -fill black -colorize 100 -fill white \
       -draw 'roundrectangle 0,0 %[fx:w-1],%[fx:h-1] 44,44' \) \
    -alpha off -compose CopyOpacity -composite -depth 8 -strip "PNG32:$output"
echo "Wrote $output"

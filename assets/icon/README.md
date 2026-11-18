# Pico app icon — Vesper

The master, `pico-evo.svg`, uses a smooth Bézier outline in the approved 30°
clockwise pose. A color field embedded in the SVG supplies the pearlescent
shading; the clip path, not the embedded pixels, defines the silhouette.
The midnight backplate has rounded corners for the desktop launcher.

- `pico-evo-1024.png`: opaque, square 1024 × 1024 artwork for store use. No corner mask.
- `pico-evo-48.png`: transparent-corner desktop fallback. The package installs this and the scalable SVG in the hicolor theme under the Wayland app ID, `pico-evo`.

To re-export from the master with resvg and ImageMagick:

```sh
resvg --width 48 pico-evo.svg pico-evo-48.png
resvg --width 4096 --background '#01012e' pico-evo.svg /tmp/vesper-square.png
magick /tmp/vesper-square.png -filter Mitchell -resize 1024x1024 -alpha off PNG24:pico-evo-1024.png
```

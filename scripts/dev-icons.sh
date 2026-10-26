#!/usr/bin/env bash
# Regenerates src-tauri/icons/dev/ (the "DEV" icon dev builds show) from assets/icon-dev.svg.
# Needs rsvg-convert, ImageMagick and (for the .icns) macOS's iconutil.
set -euo pipefail
cd "$(dirname "$0")/.."
src=assets/icon-dev.svg
out=src-tauri/icons/dev
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
mkdir -p "$out" "$tmp/icon.iconset"

png() { rsvg-convert -w "$1" -h "$1" "$src" -o "$2"; }

png 512 "$out/icon.png"
png 32 "$out/32x32.png"
png 128 "$out/128x128.png"
png 256 "$out/128x128@2x.png"
for s in 16 32 128 256 512; do
  png "$s" "$tmp/icon.iconset/icon_${s}x${s}.png"
  png $((s * 2)) "$tmp/icon.iconset/icon_${s}x${s}@2x.png"
done
iconutil -c icns "$tmp/icon.iconset" -o "$out/icon.icns"
for s in 16 24 32 48 64 256; do png "$s" "$tmp/ico-$s.png"; done
magick "$tmp"/ico-{16,24,32,48,64,256}.png "$out/icon.ico"
echo "wrote $out"

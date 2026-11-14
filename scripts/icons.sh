#!/usr/bin/env bash
# Regenerates every DevCat icon from assets/icon-source.png (the square artwork, any size >= 1024):
# assets/icon.png (the master: the artwork as an 892px rounded tile on a 1024 canvas, macOS's grid)
# the app's (src-tauri/icons, and src-tauri/icons/dev with the DEV band from assets/dev-band.svg),
# DevCat Mobile's, the window's and the website's (public/icon.png, copied by site/build.sh) and the
# promo video's. Needs ImageMagick, rsvg-convert and (for the .icns) macOS's iconutil.
set -euo pipefail
cd "$(dirname "$0")/.."
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT

# The master: crop to the artwork's tile, round its corners, pad to 1024.
magick assets/icon-source.png -filter Lanczos -resize 892x892! -alpha set \
  \( -size 892x892 xc:none -fill white -draw "roundrectangle 0,0 891,891 200,200" \) \
  -compose DstIn -composite -compose over \
  -background none -gravity center -extent 1024x1024 -strip assets/icon.png
rsvg-convert -w 1024 -h 1024 assets/dev-band.svg -o "$tmp/band.png"
magick assets/icon.png "$tmp/band.png" -compose over -composite -strip "$tmp/dev.png"

png() { magick "$1" -filter Lanczos -resize "$2x$2" -strip -define png:compression-level=9 "$3"; }
webp() { magick "$1" -filter Lanczos -resize "$2x$2" -strip -quality 90 -define webp:alpha-quality=90 "$3"; }

icons() { # <master> <dir>: the set tauri.conf.json lists, plus the .icns and .ico
  local src=$1 out=$2 set="$tmp/$(basename "$2").iconset"
  mkdir -p "$out" "$set"
  png "$src" 512 "$out/icon.png"
  png "$src" 32 "$out/32x32.png"
  png "$src" 128 "$out/128x128.png"
  png "$src" 256 "$out/128x128@2x.png"
  for s in 16 32 128 256 512; do
    png "$src" "$s" "$set/icon_${s}x${s}.png"
    png "$src" $((s * 2)) "$set/icon_${s}x${s}@2x.png"
  done
  iconutil -c icns "$set" -o "$out/icon.icns"
  for s in 16 24 32 48 64 256; do png "$src" "$s" "$set/ico-$s.png"; done
  magick "$set"/ico-{16,24,32,48,64,256}.png "$out/icon.ico"
}

icons assets/icon.png src-tauri/icons
png assets/icon.png 64 src-tauri/icons/64x64.png
for s in 30 44 71 89 107 142 150 284 310; do png assets/icon.png "$s" "src-tauri/icons/Square${s}x${s}Logo.png"; done
png assets/icon.png 50 src-tauri/icons/StoreLogo.png
icons "$tmp/dev.png" src-tauri/icons/dev
icons assets/icon.png mobile/src-tauri/icons

# Full bleed: the artwork with its black corners filled with the tile's own colour, for places that
# cut their own shape (iOS home screen, Android's maskable icon, GitHub avatars).
bg='#090114'
magick assets/icon-source.png -fuzz 4% -fill "$bg" \
  -draw 'color 0,0 floodfill' -draw 'color 1253,0 floodfill' \
  -draw 'color 0,1253 floodfill' -draw 'color 1253,1253 floodfill' \
  -filter Lanczos -resize 1024x1024 -strip "$tmp/bleed.png"
# Maskable: the artwork in Android's safe zone (the middle 80%) on the tile colour.
magick "$tmp/bleed.png" -resize 820x820 -background "$bg" -gravity center -extent 1024x1024 "$tmp/maskable.png"

# The desktop window (local, so a plain PNG; the window's favicon and the Welcome / setup screens).
mkdir -p public
png assets/icon.png 256 public/icon.png

# The website (site/build.sh copies site/img): a favicon.ico for every browser, a PNG icon, the
# logo as WebP (a tenth of the PNG's size), and the iOS home-screen icon.
mkdir -p site/img
for s in 16 32 48; do png assets/icon.png "$s" "$tmp/fav-$s.png"; done
magick "$tmp"/fav-{16,32,48}.png site/img/favicon.ico
png assets/icon.png 192 site/img/icon-192.png
webp assets/icon.png 128 site/img/icon.webp
png "$tmp/bleed.png" 180 site/img/apple-touch-icon.png

# DevCat Mobile (served over the network to the phone): the same, plus the manifest's icons.
cp site/img/favicon.ico mobile/public/favicon.ico
png assets/icon.png 192 mobile/public/icon-192.png
png assets/icon.png 512 mobile/public/icon-512.png
png "$tmp/maskable.png" 512 mobile/public/icon-maskable-512.png
png "$tmp/bleed.png" 180 mobile/public/apple-touch-icon.png
webp assets/icon.png 128 mobile/public/icon.webp

# Elsewhere: the promo video, and the images to upload by hand (GitHub avatar, the repo's social
# preview: Settings → Social preview, 1280×640).
cp assets/icon.png promo/public/icon.png
mkdir -p assets/brand
png "$tmp/bleed.png" 1024 assets/brand/avatar.png
rsvg-convert -w 1280 -h 640 assets/brand/social-preview.svg -o "$tmp/social-text.png"
magick "$tmp/social-text.png" \( assets/icon.png -filter Lanczos -resize 440x440 \) \
  -gravity west -geometry +90+0 -compose over -composite -strip \
  -define png:compression-level=9 assets/brand/social-preview.png
echo "icons regenerated from assets/icon-source.png"

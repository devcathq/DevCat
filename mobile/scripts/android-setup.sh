#!/usr/bin/env bash
# What the generated Android project (src-tauri/gen/android, made by `tauri android init` and
# not in git) needs from us. Run it after every init; CI does (.github/workflows/mobile.yml).
#
# - The DevCat launcher icon: init fills the project with Tauri's placeholder and
#   tauri.conf.json's icons don't reach it. Source: src-tauri/icons/android-icon.png, the
#   maskable icon (public/icon-maskable-512.png) at 76% on its own background (#090114), so
#   round launcher masks don't cut the ring off.
# - R8 keep rules for release builds (src-tauri/android/proguard-devcat.pro): without them
#   the QR code scanner crashes. The app's build reads every .pro file in app/.
set -euo pipefail
cd "$(dirname "$0")/.."
app=src-tauri/gen/android/app
res=$app/src/main/res
[ -d "$res" ] || { echo "no $res: run 'pnpm tauri android init' first" >&2; exit 1; }
out=$(mktemp -d)
trap 'rm -rf "$out"' EXIT
pnpm tauri icon src-tauri/icons/android-icon.png -o "$out" >/dev/null 2>&1
# The adaptive icon's background layer: Tauri writes white, the icon's own color fits.
sed -i.bak 's/#FFFFFF/#090114/' "$out/android/values/ic_launcher_background.xml"
rm "$out/android/values/ic_launcher_background.xml.bak"
cp -R "$out/android/." "$res/"
cp src-tauri/android/proguard-devcat.pro "$app/"
echo "Android project set up: launcher icon, R8 rules"

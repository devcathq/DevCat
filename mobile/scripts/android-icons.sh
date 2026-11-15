#!/usr/bin/env bash
# Puts the DevCat launcher icon into the generated Android project. `tauri android init`
# fills src-tauri/gen/android with Tauri's placeholder icon and tauri.conf.json's icons don't
# reach it, so run this after every init (CI does, in .github/workflows/mobile.yml).
#
# Source: src-tauri/icons/android-icon.png, the maskable icon (public/icon-maskable-512.png)
# at 76% on its own background (#090114), so round launcher masks don't cut the ring off.
set -euo pipefail
cd "$(dirname "$0")/.."
res=src-tauri/gen/android/app/src/main/res
[ -d "$res" ] || { echo "no $res: run 'pnpm tauri android init' first" >&2; exit 1; }
out=$(mktemp -d)
trap 'rm -rf "$out"' EXIT
pnpm tauri icon src-tauri/icons/android-icon.png -o "$out" >/dev/null 2>&1
# The adaptive icon's background layer: Tauri writes white, the icon's own color fits.
sed -i.bak 's/#FFFFFF/#090114/' "$out/android/values/ic_launcher_background.xml"
rm "$out/android/values/ic_launcher_background.xml.bak"
cp -R "$out/android/." "$res/"
echo "Android launcher icons written to $res"

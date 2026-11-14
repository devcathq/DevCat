#!/usr/bin/env sh
# Assemble the landing page and the docs into $1 (GitHub Pages serves it at /home/): the page itself,
# the docs (site/docs, built into $1/docs), the README screenshots and the icons (site/img, from scripts/icons.sh), and the bundled
# JetBrains Mono (OFL) for the headings. Needs `pnpm install` first (the docs use `marked`).
set -eu
out=${1:?usage: site/build.sh <out-dir>}
root=$(cd "$(dirname "$0")/.." && pwd)
mkdir -p "$out/img" "$out/fonts"
cp "$root/site/index.html" "$root/site/style.css" "$root/site/main.js" "$out/"
cp "$root"/site/img/* "$out/img/"
cp "$root"/assets/readme/*.png "$out/img/"
cp "$root/node_modules/@fontsource-variable/jetbrains-mono/files/jetbrains-mono-latin-wght-normal.woff2" "$out/fonts/jetbrains-mono.woff2"
node "$root/site/docs/build.mjs" "$out/docs"

#!/bin/bash
# A demo video → ../assets/readme/demo.gif (palette per video, 8 fps, 880 px wide, 128 colors:
# about 8 MB for 30 s). Usage: ./make-gif.sh demo.mp4
set -euo pipefail
cd "$(dirname "$0")"
mkdir -p ../assets/readme
IN="${1:?usage: ./make-gif.sh <video.mp4>}"
W="${GIF_WIDTH:-880}"; FPS="${GIF_FPS:-8}"
ffmpeg -loglevel error -y -i "$IN" -vf "fps=$FPS,scale=$W:-1:flags=lanczos,palettegen=max_colors=${GIF_COLORS:-128}:stats_mode=diff" /tmp/devcat-palette.png
ffmpeg -loglevel error -y -i "$IN" -i /tmp/devcat-palette.png \
  -lavfi "fps=$FPS,scale=$W:-1:flags=lanczos[x];[x][1:v]paletteuse=dither=sierra2_4a:diff_mode=rectangle" ../assets/readme/demo.gif
ls -lh ../assets/readme/demo.gif

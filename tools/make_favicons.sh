#!/usr/bin/env bash
# Favicons from the wedding emblem (templates/art/emblem.svg). Rerun after redrawing the emblem.
# Needs rsvg-convert (librsvg2-bin).
set -euo pipefail
cd "$(dirname "$0")/.."
tmp=$(mktemp -d); trap 'rm -rf "$tmp"' EXIT
# Square crop around the 600x520 emblem.
sed '0,/<svg[^>]*>/s//<svg xmlns="http:\/\/www.w3.org\/2000\/svg" viewBox="0 -40 600 600" width="600" height="600">/' \
  templates/art/emblem.svg > "$tmp/square.svg"
# Paper-backed, padded version for the iOS home screen (it fills transparent areas with black).
sed '0,/<svg[^>]*>/s//<svg xmlns="http:\/\/www.w3.org\/2000\/svg" viewBox="-60 -100 720 720" width="720" height="720"><rect x="-60" y="-100" width="720" height="720" fill="#fbf8f1"\/>/' \
  templates/art/emblem.svg > "$tmp/touch.svg"
rsvg-convert -w 32 -h 32 "$tmp/square.svg" -o static/favicon-32.png
rsvg-convert -w 192 -h 192 "$tmp/square.svg" -o static/favicon-192.png
rsvg-convert -w 180 -h 180 "$tmp/touch.svg" -o static/apple-touch-icon.png
ls -la static/*.png

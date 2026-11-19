#!/usr/bin/env bash
# Favicons: just the bow from the wedding emblem (templates/art/emblem.svg), cropped tight.
# Rerun after redrawing the emblem. Needs rsvg-convert (librsvg2-bin).
set -euo pipefail
cd "$(dirname "$0")/.."
tmp=$(mktemp -d); trap 'rm -rf "$tmp"' EXIT
# The bow is the cord (stroke #8c1c2b) and knot (fill #480e16) drawn last in the emblem.
bow=$(grep -E 'stroke="#8c1c2b"|fill="#480e16"' templates/art/emblem.svg)
[ -n "$bow" ] || { echo "no bow found in emblem.svg (colours changed?)" >&2; exit 1; }
# Square crop around the bow (it spans roughly x 249-351, y 405-515).
printf '<svg xmlns="http://www.w3.org/2000/svg" viewBox="244 403 112 112" width="112" height="112">%s</svg>' "$bow" > "$tmp/bow.svg"
# Paper-backed and padded for the iOS home screen (it fills transparent areas with black).
printf '<svg xmlns="http://www.w3.org/2000/svg" viewBox="226 385 148 148" width="148" height="148"><rect x="226" y="385" width="148" height="148" fill="#fbf8f1"/>%s</svg>' "$bow" > "$tmp/touch.svg"
rsvg-convert -w 32 -h 32 "$tmp/bow.svg" -o static/favicon-32.png
rsvg-convert -w 192 -h 192 "$tmp/bow.svg" -o static/favicon-192.png
rsvg-convert -w 180 -h 180 "$tmp/touch.svg" -o static/apple-touch-icon.png

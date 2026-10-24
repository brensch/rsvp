#!/usr/bin/env bash
# Provision the VM (idempotent). Run from your laptop:
#   PROJECT=.. ZONE=.. VM=rsvp DOMAIN=wedding.example.com LS_BUCKET=..-rsvp-litestream ./deploy/provision.sh
set -euo pipefail
: "${PROJECT:?}" "${ZONE:?}" "${VM:?}" "${DOMAIN:?}" "${LS_BUCKET:?}"
HERE="$(cd "$(dirname "$0")" && pwd)"

SSH=(gcloud --project "$PROJECT" compute ssh "$VM" --zone "$ZONE" --tunnel-through-iap --command)
SCP=(gcloud --project "$PROJECT" compute scp --zone "$ZONE" --tunnel-through-iap)

"${SCP[@]}" --recurse "$HERE/systemd" "$HERE/agent" "$HERE/Caddyfile" "$HERE/litestream.yml" "$HERE/remote-provision.sh" "$VM:/tmp/"
"${SSH[@]}" "sudo DOMAIN='$DOMAIN' LS_BUCKET='$LS_BUCKET' bash /tmp/remote-provision.sh"

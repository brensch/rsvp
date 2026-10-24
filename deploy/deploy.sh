#!/usr/bin/env bash
# Build locally and ship to the VM. Your laptop needs a glibc no newer than the
# VM's (Ubuntu 24.04 → glibc 2.39); building on Ubuntu 24.04 or in the
# rust:1-bookworm container both work.
#   PROJECT=.. ZONE=.. VM=rsvp ./deploy/deploy.sh
set -euo pipefail
: "${PROJECT:?}" "${ZONE:?}" "${VM:?}"
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
CONFIG="${CONFIG:-$ROOT/deploy/wedding.toml}"
[ -f "$CONFIG" ] || { echo "missing $CONFIG (copy config/wedding.example.toml)"; exit 1; }

cd "$ROOT"
cargo test --release
cargo build --release
BIN=target/release/rsvp
"$BIN" --help >/dev/null

SSH=(gcloud --project "$PROJECT" compute ssh "$VM" --zone "$ZONE" --tunnel-through-iap --command)
SCP=(gcloud --project "$PROJECT" compute scp --zone "$ZONE" --tunnel-through-iap)

"${SCP[@]}" "$BIN" "$CONFIG" "$VM:/tmp/"
"${SSH[@]}" 'set -e
  sudo install -m 0755 /tmp/rsvp /usr/local/bin/rsvp.new
  sudo install -o root -g rsvp -m 0640 /tmp/wedding.toml /etc/rsvp/wedding.toml
  # Validate before swapping: config parses, templates compile, DB + journal consistent.
  sudo -u rsvp RSVP_ENV_FILE=/etc/rsvp/env /usr/local/bin/rsvp.new check
  sudo mv /usr/local/bin/rsvp.new /usr/local/bin/rsvp
  sudo systemctl restart litestream rsvp
  sleep 2
  systemctl is-active rsvp litestream
  curl -fsS http://127.0.0.1:8080/healthz && echo " healthy"'

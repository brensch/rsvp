#!/usr/bin/env bash
# Deploy to an existing Linux host that already runs Caddy with `import Caddyfile.d/*.caddyfile`
# (e.g. a small VM that already hosts other sites). Builds for the host's arch, installs the binary, config, systemd
# unit and a Caddy site, validates with `rsvp check`, then restarts.
#
#   HOST=user@203.0.113.10 DOMAIN=rsvp.example.com CONFIG=deploy/mysite/wedding.toml ./deploy/to-host.sh
#
# Idempotent. The first run generates /etc/rsvp/env (RSVP_SECRET) on the host.
set -euo pipefail
: "${HOST:?e.g. opc@1.2.3.4}" "${DOMAIN:?e.g. rsvp.example.com}" "${CONFIG:?path to wedding.toml}"
PORT="${PORT:-$(grep -E '^bind' "$CONFIG" | sed -E 's/.*:([0-9]+)".*/\1/')}"
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

ARCH=$(ssh -o BatchMode=yes "$HOST" uname -m)
GLIBC=$(ssh -o BatchMode=yes "$HOST" "ldd --version | head -1 | grep -oE '[0-9]+\.[0-9]+$'")
TARGET="${ARCH}-unknown-linux-gnu"
echo "== building for $TARGET (glibc $GLIBC)"
cargo test --release -q
if [ "$ARCH" = "$(uname -m)" ] && [ -z "${FORCE_ZIG:-}" ]; then
  cargo build --release
  BIN=target/release/rsvp
else
  # cargo-zigbuild pins the glibc version so the binary runs on older hosts.
  PATH="$HOME/.local/share/zigenv/bin:$PATH" cargo zigbuild --release --target "$TARGET.$GLIBC"
  BIN="target/$TARGET/release/rsvp"
fi

echo "== shipping"
scp -q "$BIN" "$CONFIG" deploy/systemd/rsvp.service "$HOST:/tmp/"
ssh "$HOST" "sudo DOMAIN='$DOMAIN' PORT='$PORT' CONFIG_NAME='$(basename "$CONFIG")' bash -s" <<'REMOTE'
set -euo pipefail
getent group rsvp >/dev/null || groupadd --system rsvp
id rsvp >/dev/null 2>&1 || useradd --system --gid rsvp --home-dir /var/lib/rsvp --shell /sbin/nologin rsvp
install -d -o rsvp -g rsvp -m 2770 /var/lib/rsvp /var/lib/rsvp/backups
install -d -o root -g rsvp -m 0750 /etc/rsvp
if [ ! -f /etc/rsvp/env ]; then
  (umask 027; printf '# RSVP_SECRET signs every guest link; keep a copy in your password manager.\nRSVP_SECRET=%s\nDISCORD_WEBHOOK_URL=\nRESEND_API_KEY=\n' \
    "$(head -c 32 /dev/urandom | od -An -tx1 | tr -d ' \n')" > /etc/rsvp/env)
  chown root:rsvp /etc/rsvp/env; chmod 0640 /etc/rsvp/env
  echo "generated /etc/rsvp/env"
fi
install -o root -g rsvp -m 0640 "/tmp/$CONFIG_NAME" /etc/rsvp/wedding.toml
install -m 0755 /tmp/rsvp /usr/local/bin/rsvp.new
command -v restorecon >/dev/null && restorecon /usr/local/bin/rsvp.new /etc/rsvp/* || true

# Validate the new binary against the real config + data before swapping it in.
sudo -u rsvp RSVP_ENV_FILE=/etc/rsvp/env RSVP_CONFIG=/etc/rsvp/wedding.toml RSVP_DATA=/var/lib/rsvp /usr/local/bin/rsvp.new check >/dev/null
mv /usr/local/bin/rsvp.new /usr/local/bin/rsvp
command -v restorecon >/dev/null && restorecon -R /usr/local/bin/rsvp /var/lib/rsvp || true

install -m 0644 /tmp/rsvp.service /etc/systemd/system/rsvp.service
systemctl daemon-reload
systemctl enable -q rsvp
systemctl restart rsvp

cat > /tmp/rsvp.caddyfile <<EOF
# rsvp (wedding RSVP portal). Installed by rsvp/deploy/to-host.sh. Security headers come from the app.
$DOMAIN {
	encode zstd gzip
	request_body {
		max_size 1MB
	}
	header -Server
	reverse_proxy 127.0.0.1:$PORT
}
EOF
install -m 0644 /tmp/rsvp.caddyfile /etc/caddy/Caddyfile.d/rsvp.caddyfile
command -v restorecon >/dev/null && restorecon /etc/caddy/Caddyfile.d/rsvp.caddyfile || true
# Validate as the caddy user so permission problems surface before the reload.
sudo -u caddy caddy validate --config /etc/caddy/Caddyfile --adapter caddyfile >/dev/null
systemctl reload caddy
sleep 2
systemctl is-active rsvp
curl -fsS "http://127.0.0.1:$PORT/healthz" && echo " (healthz)"
REMOTE
echo "== deployed: https://$DOMAIN"

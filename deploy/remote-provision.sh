#!/usr/bin/env bash
# Runs ON the VM as root (via provision.sh). Idempotent.
set -euo pipefail
: "${DOMAIN:?}" "${LS_BUCKET:?}"
LITESTREAM_VERSION=0.3.13
export DEBIAN_FRONTEND=noninteractive

echo "== packages"
apt-get update -q
apt-get install -yq debian-keyring debian-archive-keyring apt-transport-https curl gnupg sqlite3 jq unattended-upgrades tmux git ca-certificates
dpkg-reconfigure -f noninteractive unattended-upgrades

echo "== swap (2G)"
if ! swapon --show | grep -q /swapfile; then
  fallocate -l 2G /swapfile && chmod 600 /swapfile && mkswap /swapfile && swapon /swapfile
  grep -q /swapfile /etc/fstab || echo '/swapfile none swap sw 0 0' >> /etc/fstab
fi

echo "== caddy"
if ! command -v caddy >/dev/null; then
  curl -1sLf 'https://dl.cloudsmith.io/public/caddy/stable/gpg.key' | gpg --dearmor -o /usr/share/keyrings/caddy-stable-archive-keyring.gpg
  curl -1sLf 'https://dl.cloudsmith.io/public/caddy/stable/debian.deb.txt' > /etc/apt/sources.list.d/caddy-stable.list
  apt-get update -q && apt-get install -yq caddy
fi
sed "s/{DOMAIN}/$DOMAIN/g" /tmp/Caddyfile > /etc/caddy/Caddyfile
systemctl enable --now caddy && systemctl reload caddy

echo "== litestream"
if ! command -v litestream >/dev/null; then
  curl -fsSL -o /tmp/litestream.deb "https://github.com/benbjohnson/litestream/releases/download/v${LITESTREAM_VERSION}/litestream-v${LITESTREAM_VERSION}-linux-amd64.deb"
  dpkg -i /tmp/litestream.deb
fi
sed "s/{LS_BUCKET}/$LS_BUCKET/g" /tmp/litestream.yml > /etc/litestream.yml

echo "== users and directories"
getent group rsvp >/dev/null || groupadd --system rsvp
id rsvp >/dev/null 2>&1 || useradd --system --gid rsvp --home-dir /var/lib/rsvp --shell /usr/sbin/nologin rsvp
id rsvp-agent >/dev/null 2>&1 || useradd --create-home --home-dir /home/rsvp-agent --shell /bin/bash --groups rsvp rsvp-agent
install -d -o rsvp -g rsvp -m 2770 /var/lib/rsvp /var/lib/rsvp/backups
install -d -o root -g rsvp -m 0750 /etc/rsvp
if [ ! -f /etc/rsvp/env ]; then
  umask 027
  cat > /etc/rsvp/env <<EOF
# Secrets for the RSVP service. Readable by root and the rsvp group only.
# RSVP_SECRET signs every guest link. Back it up somewhere safe (password manager):
# without it, existing links and QR codes stop working after a restore.
RSVP_SECRET=$(head -c 32 /dev/urandom | od -An -tx1 | tr -d ' \n')
DISCORD_WEBHOOK_URL=
RESEND_API_KEY=
EOF
  chown root:rsvp /etc/rsvp/env && chmod 0640 /etc/rsvp/env
  echo ">>> Generated /etc/rsvp/env. Add DISCORD_WEBHOOK_URL and RESEND_API_KEY, and copy RSVP_SECRET to your password manager."
fi

echo "== systemd units"
install -m 0644 /tmp/systemd/rsvp.service /tmp/systemd/litestream.service /tmp/systemd/claude-agent.service /etc/systemd/system/
systemctl daemon-reload
systemctl enable rsvp litestream

echo "== claude agent workspace"
install -d -o rsvp-agent -g rsvp-agent -m 0750 /srv/rsvp-agent /srv/rsvp-agent/.claude /srv/rsvp-agent/exports
install -o rsvp-agent -g rsvp-agent -m 0644 /tmp/agent/CLAUDE.md /srv/rsvp-agent/CLAUDE.md
install -o rsvp-agent -g rsvp-agent -m 0644 /tmp/agent/.claude/settings.json /srv/rsvp-agent/.claude/settings.json
if [ ! -x /home/rsvp-agent/.local/bin/claude ]; then
  sudo -u rsvp-agent -H bash -c 'curl -fsSL https://claude.ai/install.sh | bash'
fi
systemctl enable claude-agent

cat <<EOF

Provisioned. Remaining one-time steps (see README "First boot"):
  - edit /etc/rsvp/env (Discord webhook, Resend key)
  - deploy the binary + wedding.toml with ./deploy/deploy.sh
  - log Claude in:  sudo -iu rsvp-agent  then  cd /srv/rsvp-agent && claude   (/login, trust folder, /exit)
  - systemctl start claude-agent
EOF

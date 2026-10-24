# Wedding RSVP portal

One Rust binary that runs the whole thing:

- **Guest pages**: every household gets a private link + QR code. It shows the save the date first,
  then (when you flip the phase) event details and an RSVP form: per-person, per-event attendance,
  meal choice, dietary needs, named plus-ones, song request, accommodation, shuttle and a note.
- **Stationery generated in code**: Typst templates compiled in-process to print-ready PDFs
  (3 mm bleed + TrimBox for a print shop, or 2-up home-print sheets with crop marks), PNGs for email,
  envelopes, Avery address labels, place cards and an RSVP report for the caterer. No AI imagery: the
  laurel wreath, rules and monogram are drawn procedurally.
- **Wedding HQ**: admin portal, passkey-only. Approved team members edit the list directly; owners
  also handle sending, the team and settings. Households, people, stationery, email sending, team,
  settings, and a complete activity log.
- **Email** via Resend (personalised save the dates / invitations with the card inline).
- **Discord** notification on every RSVP, address and sign-up.
- **A `rsvp` CLI** that the Claude agent on the box (or you, over SSH) uses to manage the list.

```
                 ┌────────────── e2-small VM (Ubuntu 24.04) ──────────────┐
 guests ──443──► │ Caddy (TLS) ─► rsvp serve :8080 ─► SQLite (WAL, FULL)   │──► Litestream ─► gs://…-litestream (1 s)
 you    ──443──► │                    │  ├─ journal.jsonl (fsync, first)    │──► daily snapshot ─► gs://…-snapshots (retention-locked)
                 │                    │  └─ outbox ─► Discord / Resend      │──► daily disk snapshots
 Claude app ───► │ claude remote-control ─► `rsvp` CLI ─► same SQLite       │
                 └──────────────────────────────────────────────────────────┘
```

## Why you won't lose an RSVP

1. The form is validated, then the submission is **appended to `journal.jsonl` and fsync'd** before
   anything else.
2. One SQLite transaction (WAL, `synchronous=FULL`) records the raw submission (an **append-only**
   table; triggers reject UPDATE/DELETE), the current answers, an audit row, and the Discord
   notification in a **transactional outbox**.
3. Only then does the guest see "thank you". If step 2 fails they see an error and the journal still
   has it; the server **replays missing journal entries at startup**, the dashboard alerts if any are
   missing, and `rsvp journal verify|replay` exist for humans.
4. **Litestream** ships every WAL frame to GCS within ~1 s. A **daily snapshot** (`VACUUM INTO`,
   integrity-checked, gzipped with the journal) goes to a second bucket with a retention policy, so
   it can't be deleted, not even by the server. The boot disk is also snapshotted daily.
5. Notifications retry with backoff until delivered; emails carry an idempotency key so retries
   never double-send.

## Security

- **Guest links**: `HMAC-SHA256(secret, household:id:version)` → 128 bits, base32. Only a SHA-256 of
  the token is stored, so a leaked database doesn't leak working links. Rotating a link bumps the
  version. Bad tokens are rate-limited per IP (lockout after 15 misses).
- **Admin**: passkeys only (WebAuthn, discoverable credentials, user verification required). No
  passwords, no email login, no username prompt. New sign-ups are `pending` and see nothing until an
  owner approves them (or they used a one-time invite link). Sessions: random 160-bit ids in a
  `__Host-` cookie, `Secure; HttpOnly; SameSite=Strict`, stored hashed, 14-day expiry, per-session
  CSRF token on every form, plus an Origin check on every write.
- **Headers**: strict CSP (`default-src 'none'`, no inline script/style, no third-party anything),
  `frame-ancestors 'none'`, `Referrer-Policy: no-referrer` (links carry secrets), HSTS, COOP/CORP,
  `noindex`, `no-store` on dynamic pages.
- **Infra**: SSH only through Identity-Aware Proxy (no public port 22), OS Login, Shielded VM,
  app bound to loopback behind Caddy, hardened systemd sandboxes, a dedicated service account with
  bucket-level permissions only (create-only on the snapshot bucket), unattended security upgrades.
- **The agent**: runs as its own Unix user with a permission allowlist (only `rsvp` guest-list
  commands; destructive ones require your approval in the session; no sqlite, no secrets, no sudo).
  Every change it makes is audited as `claude`.

## Local development

```bash
cp config/wedding.example.toml dev.toml
sed -i 's|https://wedding.example.com|http://localhost:8080|; s|rp_id = "wedding.example.com"|rp_id = "localhost"|; s|^bucket = .*|bucket = ""|' dev.toml
mkdir -p data && echo "RSVP_SECRET=$(cargo run -q -- gen-secret)" > data/env
export RSVP_CONFIG=dev.toml RSVP_DATA=data RSVP_ENV_FILE=data/env

cargo run -- household add --json '{"guests":[{"first_name":"Jane"},{"first_name":"John"}]}'
cargo run -- invite --role owner      # open the printed link and create your passkey
cargo run -- serve                    # http://localhost:8080/admin
cargo test
```

Passkeys work on `http://localhost` in all major browsers.

## Deploying

```bash
# 1. Infrastructure (once)
PROJECT=my-gcp-project DOMAIN=wedding.example.com ./deploy/gcp-setup.sh
#    → point an A record for the domain at the printed IP

# 2. Config
cp config/wedding.example.toml deploy/wedding.toml   # edit names, dates, venues, meals, bucket…

# 3. Provision the VM (once; idempotent)
PROJECT=… ZONE=australia-southeast1-a VM=rsvp DOMAIN=wedding.example.com \
  LS_BUCKET=my-gcp-project-rsvp-litestream ./deploy/provision.sh

# 4. Ship it (every release): tests, release build, `rsvp check` on the box, atomic swap, restart
PROJECT=… ZONE=… VM=rsvp ./deploy/deploy.sh
```

### First boot

```bash
gcloud compute ssh rsvp --tunnel-through-iap
sudo nano /etc/rsvp/env                  # add DISCORD_WEBHOOK_URL, RESEND_API_KEY
                                         # and copy RSVP_SECRET into your password manager
sudo systemctl restart rsvp
sudo -u rsvp RSVP_ENV_FILE=/etc/rsvp/env rsvp invite --role owner   # → open link, create passkey
sudo -u rsvp RSVP_ENV_FILE=/etc/rsvp/env rsvp invite --role owner   # → one for your partner
```

Then at **Team** you can create helper invites, or let family sign up at `/admin/register` and approve
them by name.

- **Discord**: Server settings → Integrations → Webhooks → New webhook → copy URL.
- **Resend**: add your domain (e.g. `mail.wedding.example.com`), add the DNS records it gives you
  (SPF/DKIM), create an API key, set `[email] from` to an address on that domain. Use
  **Send → Send a test** first.

### The Claude agent

```bash
gcloud compute ssh rsvp --tunnel-through-iap
sudo -iu rsvp-agent
cd /srv/rsvp-agent && claude      # /login with your Claude account, trust the folder, then /exit
exit
sudo systemctl start claude-agent
journalctl -u claude-agent -f     # shows the session name
```

Open **claude.ai/code** (or the Claude app) → the `wedding-guest-list` session, and talk to it:
"Add my cousin Mia and her partner Theo, they live in Melbourne, mum's side", "who hasn't
replied from Sam's side?", "move everyone tagged uni to tier 2".
Its instructions live in `deploy/agent/CLAUDE.md`; its permissions in `deploy/agent/.claude/settings.json`.

## Running it

| Task | How |
|---|---|
| Switch from save-the-date to RSVPs | Settings → Phase (or `rsvp phase rsvp-open`) |
| Print save the dates | Stationery → Save the date → Print shop / Home print |
| Email everyone their save the date | Send → Save the dates (only unsent households with an email) |
| See who's coming | Overview, People (filter by event/answer), `rsvp rsvps` |
| Caterer numbers | Stationery → RSVP report, or People → Export CSV |
| Someone lost their link | Their household page → copy link (or scan the QR on screen) |
| A link leaked | Household → Replace link (old link + printed QR stop working) |
| Tweak a design | Copy `templates/x.typ` to `/var/lib/rsvp/templates/` on the box and edit; no redeploy |

### Disaster recovery

```bash
sudo systemctl stop rsvp litestream
sudo -u rsvp litestream restore -config /etc/litestream.yml -o /var/lib/rsvp/rsvp.db.restored /var/lib/rsvp/rsvp.db
# or: gcloud storage cp gs://…-snapshots/snapshots/YYYYMMDD/rsvp-*.db.gz . && gunzip
sudo -u rsvp mv /var/lib/rsvp/rsvp.db.restored /var/lib/rsvp/rsvp.db
sudo -u rsvp RSVP_ENV_FILE=/etc/rsvp/env rsvp journal replay   # re-applies anything newer from the journal
sudo -u rsvp RSVP_ENV_FILE=/etc/rsvp/env rsvp check
sudo systemctl start litestream rsvp
```

On a brand-new VM, restore `/etc/rsvp/env` with the **same `RSVP_SECRET`** (that's what makes the
printed QR codes keep working), then run `rsvp rehash-tokens` once.

## Layout

```
src/
  main.rs        startup, env file loading, background tasks
  cli.rs         `rsvp` command line
  config.rs      wedding.toml + secrets
  db.rs          SQLite setup, migrations, nestable transactions
  store.rs       households, guests, users, audit, outbox
  rsvp.rs        form model, validation, durable submit, journal replay
  journal.rs     fsync'd JSONL journal
  tokens.rs      link tokens, secrets
  assets/        Typst world, PDF/PNG/SVG output, QR codes, formal date wording
  email.rs       Resend emails
  notify.rs      outbox worker (Discord + email)
  backup.rs      daily snapshots to GCS
  web/           axum: security middleware, guest pages, passkey auth, admin portal
templates/       Typst stationery (lib.typ = shared design system)
static/          CSS + small JS (no frameworks, no CDNs)
fonts/           Cormorant Garamond, EB Garamond, Pinyon Script (OFL)
deploy/          GCP setup, provisioning, systemd units, Caddy, Litestream, agent workspace
```

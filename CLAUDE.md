# Developing the RSVP portal

Rust (edition 2024), axum 0.8, rusqlite, maud for HTML, Typst 0.15 embedded for stationery,
webauthn-rs for passkeys. See README.md for the architecture and the durability/security model.

## Commands

```bash
cargo test                       # unit + end-to-end RSVP/journal tests
cargo build --release
RSVP_CONFIG=dev.toml RSVP_DATA=data RSVP_ENV_FILE=data/env cargo run -- serve
cargo run -- render save-the-date --format png -o /tmp/std.png   # quick template iteration
```

## Guest data never goes in git

Names, emails, addresses and RSVPs exist only on the server (SQLite in `/var/lib/rsvp`, the
journal, snapshots/backups, logs). Import files, CSVs, exports and real `deploy/*/wedding.toml`
configs are gitignored, and `.githooks/pre-commit` blocks them (`git config core.hooksPath .githooks`).
Tests and docs use made-up names only. Logs record household ids, not names.

## Rules of the codebase

- The RSVP submit path (`rsvp::submit`) must stay: validate → journal fsync → single DB
  transaction (submission + responses + audit + outbox) → respond. Never notify or email
  outside the outbox.
- Every mutation in `store.rs` takes an `actor` and writes an audit row in the same transaction.
  Use `db::begin` (nestable) rather than `unchecked_transaction`.
- `submissions` and `audit` are append-only (DB triggers). Schema changes go in a new entry in
  `db::MIGRATIONS`; never edit an existing migration.
- No inline `<script>`/`<style>` and no third-party origins; the CSP forbids them. Put JS in
  `static/*.js` and register new files in `web/ui.rs` `STATIC`.
- Admin POST handlers check CSRF (`F::csrf`) first, then role (`require_owner`) where needed.
  Household/guest edits go through `apply_change`.
- Templates read `/data.json`; its shape is built in `assets::household_data` / `report_data`.
  After changing a template, render it to PNG and look at it.

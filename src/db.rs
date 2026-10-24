//! SQLite storage: connection setup, schema migrations, async access.
//!
//! Durability settings: WAL + synchronous=FULL means a commit is on disk before
//! we answer the request. Litestream streams the WAL to GCS continuously.

use std::path::Path;
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result};
use rusqlite::{Connection, OpenFlags};

#[derive(Clone)]
pub struct Db {
    conn: Arc<Mutex<Connection>>,
}

impl Db {
    pub fn open(path: &Path) -> Result<Self> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let conn = Connection::open_with_flags(
            path,
            OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_CREATE | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )
        .with_context(|| format!("opening {}", path.display()))?;
        conn.busy_timeout(std::time::Duration::from_secs(10))?;
        conn.execute_batch(
            "PRAGMA journal_mode = WAL;
             PRAGMA synchronous = FULL;
             PRAGMA foreign_keys = ON;
             PRAGMA trusted_schema = OFF;",
        )?;
        // Default autocheckpointing is fine alongside Litestream: it holds a read
        // transaction so checkpoints never overtake WAL frames it hasn't shipped.
        migrate(&conn)?;
        Ok(Self { conn: Arc::new(Mutex::new(conn)) })
    }

    /// Run a closure against the connection on the blocking pool.
    pub async fn call<T, F>(&self, f: F) -> Result<T>
    where
        T: Send + 'static,
        F: FnOnce(&mut Connection) -> Result<T> + Send + 'static,
    {
        let conn = self.conn.clone();
        tokio::task::spawn_blocking(move || {
            let mut guard = conn.lock().unwrap_or_else(|p| p.into_inner());
            f(&mut guard)
        })
        .await
        .context("db task panicked")?
    }

    /// Synchronous access, for the CLI.
    pub fn with<T>(&self, f: impl FnOnce(&mut Connection) -> Result<T>) -> Result<T> {
        let mut guard = self.conn.lock().unwrap_or_else(|p| p.into_inner());
        f(&mut guard)
    }
}

const MIGRATIONS: &[&str] = &[
    // 1: initial schema
    r#"
    CREATE TABLE settings (
        key   TEXT PRIMARY KEY,
        value TEXT NOT NULL
    ) STRICT;

    CREATE TABLE households (
        id              INTEGER PRIMARY KEY,
        name            TEXT NOT NULL,              -- internal label, "Smith family"
        greeting        TEXT NOT NULL,              -- shown to guests, "Jane & John"
        envelope        TEXT NOT NULL DEFAULT '',   -- formal address line, "Mr & Mrs J. Smith"
        email           TEXT NOT NULL DEFAULT '',
        phone           TEXT NOT NULL DEFAULT '',
        addr1           TEXT NOT NULL DEFAULT '',
        addr2           TEXT NOT NULL DEFAULT '',
        city            TEXT NOT NULL DEFAULT '',
        region          TEXT NOT NULL DEFAULT '',
        postcode        TEXT NOT NULL DEFAULT '',
        country         TEXT NOT NULL DEFAULT '',
        side            TEXT NOT NULL DEFAULT 'both' CHECK (side IN ('a','b','both')),
        tags            TEXT NOT NULL DEFAULT '',
        events          TEXT NOT NULL DEFAULT '[]', -- JSON array of event slugs
        notes           TEXT NOT NULL DEFAULT '',
        token_version   INTEGER NOT NULL DEFAULT 1,
        token_hash      TEXT UNIQUE,
        std_sent_at     TEXT,
        invite_sent_at  TEXT,
        first_viewed_at TEXT,
        last_viewed_at  TEXT,
        rsvp_at         TEXT,
        rsvp_submission TEXT,
        rsvp_message    TEXT NOT NULL DEFAULT '',
        rsvp_song       TEXT NOT NULL DEFAULT '',
        rsvp_accommodation TEXT NOT NULL DEFAULT '',
        rsvp_transport  TEXT NOT NULL DEFAULT '',
        created_at      TEXT NOT NULL,
        updated_at      TEXT NOT NULL,
        archived_at     TEXT
    ) STRICT;

    CREATE TABLE guests (
        id               INTEGER PRIMARY KEY,
        household_id     INTEGER NOT NULL REFERENCES households(id),
        first_name       TEXT NOT NULL,
        last_name        TEXT NOT NULL DEFAULT '',
        email            TEXT NOT NULL DEFAULT '',
        phone            TEXT NOT NULL DEFAULT '',
        is_child         INTEGER NOT NULL DEFAULT 0,
        plus_one_allowed INTEGER NOT NULL DEFAULT 0,
        plus_one_of      INTEGER REFERENCES guests(id),
        dietary          TEXT NOT NULL DEFAULT '',
        notes            TEXT NOT NULL DEFAULT '',
        sort             INTEGER NOT NULL DEFAULT 0,
        created_at       TEXT NOT NULL,
        updated_at       TEXT NOT NULL,
        archived_at      TEXT
    ) STRICT;
    CREATE INDEX guests_household ON guests(household_id);

    CREATE TABLE relationships (
        id         INTEGER PRIMARY KEY,
        a          INTEGER NOT NULL REFERENCES guests(id),
        b          INTEGER NOT NULL REFERENCES guests(id),
        kind       TEXT NOT NULL,
        note       TEXT NOT NULL DEFAULT '',
        created_at TEXT NOT NULL,
        UNIQUE (a, b, kind),
        CHECK (a <> b)
    ) STRICT;

    -- Every RSVP form submission, verbatim. Append-only: enforced by triggers.
    CREATE TABLE submissions (
        id           TEXT PRIMARY KEY,
        household_id INTEGER NOT NULL REFERENCES households(id),
        received_at  TEXT NOT NULL,
        payload      TEXT NOT NULL,
        ip_hash      TEXT NOT NULL DEFAULT '',
        user_agent   TEXT NOT NULL DEFAULT ''
    ) STRICT;
    CREATE INDEX submissions_household ON submissions(household_id, received_at);
    CREATE TRIGGER submissions_no_update BEFORE UPDATE ON submissions
        BEGIN SELECT RAISE(ABORT, 'submissions are append-only'); END;
    CREATE TRIGGER submissions_no_delete BEFORE DELETE ON submissions
        BEGIN SELECT RAISE(ABORT, 'submissions are append-only'); END;

    -- Current answer per guest per event (derived from the latest submission).
    CREATE TABLE responses (
        guest_id      INTEGER NOT NULL REFERENCES guests(id),
        event_slug    TEXT NOT NULL,
        attending     INTEGER NOT NULL,
        meal          TEXT NOT NULL DEFAULT '',
        submission_id TEXT NOT NULL REFERENCES submissions(id),
        updated_at    TEXT NOT NULL,
        PRIMARY KEY (guest_id, event_slug)
    ) STRICT;

    CREATE TABLE users (
        id            TEXT PRIMARY KEY,
        name          TEXT NOT NULL UNIQUE COLLATE NOCASE,
        role          TEXT NOT NULL CHECK (role IN ('pending','helper','owner','disabled')),
        created_at    TEXT NOT NULL,
        approved_by   TEXT,
        approved_at   TEXT,
        last_login_at TEXT
    ) STRICT;

    CREATE TABLE credentials (
        cred_id      TEXT PRIMARY KEY,
        user_id      TEXT NOT NULL REFERENCES users(id),
        passkey      TEXT NOT NULL,
        created_at   TEXT NOT NULL,
        last_used_at TEXT
    ) STRICT;

    CREATE TABLE sessions (
        id_hash      TEXT PRIMARY KEY,
        user_id      TEXT NOT NULL REFERENCES users(id),
        csrf         TEXT NOT NULL,
        created_at   TEXT NOT NULL,
        expires_at   TEXT NOT NULL,
        last_seen_at TEXT NOT NULL,
        ip           TEXT NOT NULL DEFAULT ''
    ) STRICT;

    CREATE TABLE invites (
        code_hash  TEXT PRIMARY KEY,
        role       TEXT NOT NULL CHECK (role IN ('helper','owner')),
        created_by TEXT NOT NULL,
        created_at TEXT NOT NULL,
        expires_at TEXT NOT NULL,
        used_by    TEXT,
        used_at    TEXT
    ) STRICT;

    CREATE TABLE proposals (
        id          INTEGER PRIMARY KEY,
        author      TEXT NOT NULL,
        kind        TEXT NOT NULL,
        target_id   INTEGER,
        payload     TEXT NOT NULL,
        note        TEXT NOT NULL DEFAULT '',
        status      TEXT NOT NULL DEFAULT 'open' CHECK (status IN ('open','approved','rejected')),
        reviewer    TEXT,
        reviewed_at TEXT,
        review_note TEXT NOT NULL DEFAULT '',
        created_at  TEXT NOT NULL
    ) STRICT;

    CREATE TABLE audit (
        id        INTEGER PRIMARY KEY,
        at        TEXT NOT NULL,
        actor     TEXT NOT NULL,
        action    TEXT NOT NULL,
        entity    TEXT NOT NULL DEFAULT '',
        entity_id TEXT NOT NULL DEFAULT '',
        detail    TEXT NOT NULL DEFAULT '{}'
    ) STRICT;
    CREATE INDEX audit_at ON audit(at);
    CREATE TRIGGER audit_no_update BEFORE UPDATE ON audit
        BEGIN SELECT RAISE(ABORT, 'audit is append-only'); END;
    CREATE TRIGGER audit_no_delete BEFORE DELETE ON audit
        BEGIN SELECT RAISE(ABORT, 'audit is append-only'); END;

    -- Transactional outbox: notifications and emails are queued in the same
    -- transaction as the change that caused them, then delivered with retries.
    CREATE TABLE outbox (
        id              INTEGER PRIMARY KEY,
        kind            TEXT NOT NULL,
        payload         TEXT NOT NULL,
        created_at      TEXT NOT NULL,
        attempts        INTEGER NOT NULL DEFAULT 0,
        next_attempt_at TEXT NOT NULL,
        sent_at         TEXT,
        failed_at       TEXT,
        last_error      TEXT NOT NULL DEFAULT ''
    ) STRICT;
    CREATE INDEX outbox_pending ON outbox(next_attempt_at) WHERE sent_at IS NULL AND failed_at IS NULL;
    "#,
    // 2: per-person event override (e.g. kids not invited to an adults-only wedding).
    // NULL = invited to everything the household is.
    r#"
    ALTER TABLE guests ADD COLUMN events TEXT;
    "#,
    // 3: invitation tiers (1 = definitely invited, 2/3 = later rounds as space allows).
    r#"
    ALTER TABLE households ADD COLUMN tier INTEGER NOT NULL DEFAULT 1 CHECK (tier IN (1, 2, 3));
    "#,
    // 4: households based in Australia see Australia-specific travel info.
    r#"
    ALTER TABLE households ADD COLUMN australia INTEGER NOT NULL DEFAULT 0;
    "#,
    // 5: guest-supplied mailing addresses.
    r#"
    ALTER TABLE households ADD COLUMN address_status TEXT NOT NULL DEFAULT '';
    ALTER TABLE households ADD COLUMN address_at TEXT;
    ALTER TABLE submissions ADD COLUMN kind TEXT NOT NULL DEFAULT 'rsvp';
    "#,
    // 6: we don't collect phone numbers.
    r#"
    ALTER TABLE households DROP COLUMN phone;
    ALTER TABLE guests DROP COLUMN phone;
    "#,
    // 7: households are the only grouping; no person-to-person relationships.
    r#"
    DROP TABLE relationships;
    "#,
    // 8: no proposals; approved team members edit directly (everything is in the audit log).
    r#"
    DROP TABLE proposals;
    "#,
    // 9: "all correct" check: someone confirmed this household's names and details.
    r#"
    ALTER TABLE households ADD COLUMN checked_at TEXT;
    ALTER TABLE households ADD COLUMN checked_by TEXT;
    "#,
];

fn migrate(conn: &Connection) -> Result<()> {
    let version: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
    for (i, sql) in MIGRATIONS.iter().enumerate() {
        let target = i as i64 + 1;
        if version < target {
            let tx = conn.unchecked_transaction()?;
            tx.execute_batch(sql).with_context(|| format!("migration {target}"))?;
            tx.execute_batch(&format!("PRAGMA user_version = {target}"))?;
            tx.commit()?;
            tracing::info!(target, "applied migration");
        }
    }
    Ok(())
}

pub fn now() -> String {
    time::OffsetDateTime::now_utc()
        .format(&time::format_description::well_known::Rfc3339)
        .unwrap()
}

/// Nestable write transaction. Outermost: BEGIN IMMEDIATE (takes the write lock
/// up front so concurrent CLI + server writers queue on busy_timeout instead of
/// deadlocking). Nested: a SAVEPOINT. Rolls back on drop unless committed.
pub struct Tx<'a> {
    conn: &'a Connection,
    outer: bool,
    done: bool,
}

pub fn begin(conn: &Connection) -> Result<Tx<'_>> {
    let outer = conn.is_autocommit();
    conn.execute_batch(if outer { "BEGIN IMMEDIATE" } else { "SAVEPOINT nested" })?;
    Ok(Tx { conn, outer, done: false })
}

impl Tx<'_> {
    pub fn commit(mut self) -> Result<()> {
        self.conn.execute_batch(if self.outer { "COMMIT" } else { "RELEASE nested" })?;
        self.done = true;
        Ok(())
    }
}

impl Drop for Tx<'_> {
    fn drop(&mut self) {
        if !self.done {
            let sql = if self.outer { "ROLLBACK" } else { "ROLLBACK TO nested; RELEASE nested" };
            if let Err(e) = self.conn.execute_batch(sql) {
                tracing::error!(error = %e, "rollback failed");
            }
        }
    }
}

impl std::ops::Deref for Tx<'_> {
    type Target = Connection;
    fn deref(&self) -> &Connection {
        self.conn
    }
}

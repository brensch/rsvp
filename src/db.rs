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
    // 10: guest website pages and photo gallery.
    r#"
    CREATE TABLE pages (
        slug       TEXT PRIMARY KEY,
        title      TEXT NOT NULL,
        body       TEXT NOT NULL DEFAULT '',
        audience   TEXT NOT NULL DEFAULT 'all' CHECK (audience IN ('all','australia','us')),
        published  INTEGER NOT NULL DEFAULT 0,
        sort       INTEGER NOT NULL DEFAULT 0,
        updated_at TEXT NOT NULL,
        updated_by TEXT NOT NULL DEFAULT ''
    ) STRICT;
    CREATE TABLE photos (
        id          INTEGER PRIMARY KEY,
        file        TEXT NOT NULL UNIQUE,
        caption     TEXT NOT NULL DEFAULT '',
        width       INTEGER NOT NULL,
        height      INTEGER NOT NULL,
        sort        INTEGER NOT NULL DEFAULT 0,
        uploaded_by TEXT NOT NULL,
        created_at  TEXT NOT NULL,
        deleted_at  TEXT
    ) STRICT;
    "#,
    // 11: who added each household; households requested by helpers wait for approval;
    // team members can be a partner's parent.
    r#"
    ALTER TABLE households ADD COLUMN added_by TEXT NOT NULL DEFAULT '';
    ALTER TABLE households ADD COLUMN request_status TEXT NOT NULL DEFAULT '';
    ALTER TABLE users ADD COLUMN relation TEXT NOT NULL DEFAULT '';
    ALTER TABLE invites ADD COLUMN relation TEXT NOT NULL DEFAULT '';
    "#,
    // 12: one name per household (how we address them). `name` mirrors `greeting`;
    // the separate envelope name is gone.
    r#"
    UPDATE households SET name = greeting, envelope = '';
    "#,
    // 13: plus-ones are their own kind of person in the household ("Ben's plus one") instead of a
    // per-person tick. A ticked +1 on someone living alone becomes a slot; with a second person it's dropped.
    r#"
    ALTER TABLE guests ADD COLUMN plus_one_label TEXT NOT NULL DEFAULT '';
    INSERT INTO guests (household_id, first_name, last_name, plus_one_label, sort, created_at, updated_at)
        SELECT g.household_id, '', '', g.first_name || '''s plus one', 1000,
               strftime('%Y-%m-%dT%H:%M:%SZ', 'now'), strftime('%Y-%m-%dT%H:%M:%SZ', 'now')
        FROM guests g
        WHERE g.plus_one_allowed = 1 AND g.archived_at IS NULL
          AND (SELECT COUNT(*) FROM guests o
               WHERE o.household_id = g.household_id AND o.archived_at IS NULL AND o.plus_one_of IS NULL) = 1;
    ALTER TABLE guests DROP COLUMN plus_one_allowed;
    "#,
    // 14: events are invited per household, not per person. Where everyone in a household had
    // the same exception (e.g. wedding only), the household takes it; other exceptions are dropped.
    r#"
    UPDATE households SET events = (
            SELECT MIN(g.events) FROM guests g WHERE g.household_id = households.id AND g.archived_at IS NULL)
        WHERE EXISTS (SELECT 1 FROM guests g WHERE g.household_id = households.id AND g.archived_at IS NULL)
          AND NOT EXISTS (SELECT 1 FROM guests g WHERE g.household_id = households.id AND g.archived_at IS NULL AND g.events IS NULL)
          AND (SELECT COUNT(DISTINCT g.events) FROM guests g WHERE g.household_id = households.id AND g.archived_at IS NULL) = 1
          AND (SELECT MIN(g.events) FROM guests g WHERE g.household_id = households.id AND g.archived_at IS NULL) <> '[]';
    ALTER TABLE guests DROP COLUMN events;
    "#,
    // 15: a household we think probably won't come (for planning numbers).
    r#"
    ALTER TABLE households ADD COLUMN unlikely INTEGER NOT NULL DEFAULT 0;
    "#,
    // 16: one row per email sent to one address, and whether its card picture was loaded.
    r#"
    CREATE TABLE email_sends (
        id             TEXT PRIMARY KEY,           -- random; appears in the card picture's URL
        household_id   INTEGER NOT NULL REFERENCES households(id),
        address        TEXT NOT NULL,
        kind           TEXT NOT NULL,              -- save_the_date | invitation
        test           INTEGER NOT NULL DEFAULT 0,
        queued_at      TEXT NOT NULL,
        sent_at        TEXT,
        provider_id    TEXT NOT NULL DEFAULT '',
        opened_at      TEXT,
        last_opened_at TEXT,
        opens          INTEGER NOT NULL DEFAULT 0
    ) STRICT;
    CREATE INDEX email_sends_household ON email_sends(household_id);
    -- Set when we've pinged Discord that an opened invitation has gone 48 hours without a reply.
    ALTER TABLE households ADD COLUMN nudged_at TEXT;
    "#,
    // 17: the "From Australia" page becomes a section of Travel that only Australian
    // households see (between `::: australia` and `:::`).
    r#"
    UPDATE pages SET
        body = body || char(10) || '::: australia' || char(10)
               || (SELECT body FROM pages WHERE slug = 'travel-from-australia') || char(10) || ':::' || char(10),
        updated_at = strftime('%Y-%m-%dT%H:%M:%SZ', 'now'), updated_by = 'system'
        WHERE slug = 'travel' AND EXISTS (SELECT 1 FROM pages WHERE slug = 'travel-from-australia');
    DELETE FROM pages WHERE slug = 'travel-from-australia';
    "#,
    // 18: each "Add to calendar" download, with the version of every event it contained, so we
    // can tell a household when their calendar copy is out of date.
    r#"
    CREATE TABLE calendar_downloads (
        id           INTEGER PRIMARY KEY,
        household_id INTEGER NOT NULL REFERENCES households(id),
        at           TEXT NOT NULL,
        sequences    TEXT NOT NULL DEFAULT '{}'   -- {"wedding": 2, ...}
    ) STRICT;
    CREATE INDEX calendar_downloads_household ON calendar_downloads(household_id, at);
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plus_one_ticks_become_slots() {
        let conn = Connection::open_in_memory().unwrap();
        for sql in &MIGRATIONS[..12] {
            conn.execute_batch(sql).unwrap();
        }
        conn.execute_batch("PRAGMA user_version = 12").unwrap();
        let ts = "2026-01-01T00:00:00Z";
        for (hid, people) in [(1, &[("Ann", 1)][..]), (2, &[("Bo", 1), ("Cy", 0)][..]), (3, &[("Di", 0)][..])] {
            conn.execute("INSERT INTO households (id, name, greeting, events, created_at, updated_at) VALUES (?1, 'h', 'h', '[]', ?2, ?2)", rusqlite::params![hid, ts]).unwrap();
            for (name, p1) in people {
                conn.execute(
                    "INSERT INTO guests (household_id, first_name, plus_one_allowed, created_at, updated_at) VALUES (?1, ?2, ?3, ?4, ?4)",
                    rusqlite::params![hid, name, p1, ts],
                )
                .unwrap();
            }
        }
        migrate(&conn).unwrap();
        let slots: Vec<(i64, String, String)> = conn
            .prepare("SELECT household_id, first_name, plus_one_label FROM guests WHERE plus_one_label <> ''")
            .unwrap()
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();
        assert_eq!(slots, vec![(1, String::new(), "Ann's plus one".to_string())]);
        let n: i64 = conn.query_row("SELECT COUNT(*) FROM guests", [], |r| r.get(0)).unwrap();
        assert_eq!(n, 5);
    }

    #[test]
    fn shared_event_exceptions_move_to_the_household() {
        let conn = Connection::open_in_memory().unwrap();
        for sql in &MIGRATIONS[..13] {
            conn.execute_batch(sql).unwrap();
        }
        conn.execute_batch("PRAGMA user_version = 13").unwrap();
        let ts = "2026-01-01T00:00:00Z";
        let both = r#"["rehearsal-dinner","wedding"]"#;
        // 1: everyone wedding-only -> household wedding-only. 2: only the kid had an exception -> dropped.
        for (hid, people) in [(1, vec![Some(r#"["wedding"]"#), Some(r#"["wedding"]"#)]), (2, vec![None, Some(r#"["rehearsal-dinner"]"#)])] {
            conn.execute("INSERT INTO households (id, name, greeting, events, created_at, updated_at) VALUES (?1, 'h', 'h', ?2, ?3, ?3)", rusqlite::params![hid, both, ts]).unwrap();
            for ev in people {
                conn.execute(
                    "INSERT INTO guests (household_id, first_name, events, created_at, updated_at) VALUES (?1, 'x', ?2, ?3, ?3)",
                    rusqlite::params![hid, ev, ts],
                )
                .unwrap();
            }
        }
        migrate(&conn).unwrap();
        let ev = |id: i64| conn.query_row("SELECT events FROM households WHERE id = ?1", [id], |r| r.get::<_, String>(0)).unwrap();
        assert_eq!(ev(1), r#"["wedding"]"#);
        assert_eq!(ev(2), both);
        let cols: i64 = conn.query_row("SELECT COUNT(*) FROM pragma_table_info('guests') WHERE name = 'events'", [], |r| r.get(0)).unwrap();
        assert_eq!(cols, 0);
    }
}

//! Data access layer shared by the web server and the CLI.
//!
//! Every mutating function takes an `actor` and writes an audit row in the
//! same transaction as the change.

use std::collections::{BTreeMap, HashMap};

use anyhow::{Result, anyhow, bail};
use rusqlite::{Connection, OptionalExtension, Row, params};
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::db::now;
use crate::tokens;

// ---------------------------------------------------------------------------
// Households

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Household {
    pub id: i64,
    pub name: String,
    pub greeting: String,
    pub envelope: String,
    pub email: String,
    pub addr1: String,
    pub addr2: String,
    pub city: String,
    pub region: String,
    pub postcode: String,
    pub country: String,
    pub side: String,
    /// Invitation tier: 1 = first round, 2 and 3 = later rounds.
    pub tier: i64,
    /// Lives in Australia: sees the Australia travel information.
    pub australia: bool,
    /// We think they probably won't come.
    pub unlikely: bool,
    /// When Discord was told they opened their invitation 48+ hours ago without replying.
    pub nudged_at: Option<String>,
    pub tags: String,
    pub events: Vec<String>,
    pub notes: String,
    pub token_version: i64,
    pub std_sent_at: Option<String>,
    pub invite_sent_at: Option<String>,
    pub first_viewed_at: Option<String>,
    pub last_viewed_at: Option<String>,
    pub rsvp_at: Option<String>,
    pub rsvp_message: String,
    pub rsvp_song: String,
    pub rsvp_accommodation: String,
    pub rsvp_transport: String,
    /// "verified" / "guest-confirmed" / "unverified" when the guest supplied it; "" if entered by us.
    pub address_status: String,
    pub address_at: Option<String>,
    /// Someone confirmed the names and details are all correct.
    pub checked_at: Option<String>,
    pub checked_by: Option<String>,
    /// Who put this household on the list, e.g. "Sam" or "Jo (Alex's parent)".
    pub added_by: String,
    /// "" for normal households; "pending" while a helper's request awaits approval.
    pub request_status: String,
    pub created_at: String,
    pub updated_at: String,
    pub archived_at: Option<String>,
}

impl Household {
    fn from_row(r: &Row) -> rusqlite::Result<Self> {
        let events: String = r.get("events")?;
        Ok(Self {
            id: r.get("id")?,
            name: r.get("name")?,
            greeting: r.get("greeting")?,
            envelope: r.get("envelope")?,
            email: r.get("email")?,
            addr1: r.get("addr1")?,
            addr2: r.get("addr2")?,
            city: r.get("city")?,
            region: r.get("region")?,
            postcode: r.get("postcode")?,
            country: r.get("country")?,
            side: r.get("side")?,
            tier: r.get("tier")?,
            australia: r.get("australia")?,
            unlikely: r.get("unlikely")?,
            nudged_at: r.get("nudged_at")?,
            tags: r.get("tags")?,
            events: serde_json::from_str(&events).unwrap_or_default(),
            notes: r.get("notes")?,
            token_version: r.get("token_version")?,
            std_sent_at: r.get("std_sent_at")?,
            invite_sent_at: r.get("invite_sent_at")?,
            first_viewed_at: r.get("first_viewed_at")?,
            last_viewed_at: r.get("last_viewed_at")?,
            rsvp_at: r.get("rsvp_at")?,
            rsvp_message: r.get("rsvp_message")?,
            rsvp_song: r.get("rsvp_song")?,
            rsvp_accommodation: r.get("rsvp_accommodation")?,
            rsvp_transport: r.get("rsvp_transport")?,
            address_status: r.get("address_status")?,
            address_at: r.get("address_at")?,
            checked_at: r.get("checked_at")?,
            checked_by: r.get("checked_by")?,
            added_by: r.get("added_by")?,
            request_status: r.get("request_status")?,
            created_at: r.get("created_at")?,
            updated_at: r.get("updated_at")?,
            archived_at: r.get("archived_at")?,
        })
    }

    /// Mailing address as display lines.
    pub fn address_lines(&self) -> Vec<String> {
        let mut out = vec![];
        for l in [&self.addr1, &self.addr2] {
            if !l.trim().is_empty() {
                out.push(l.trim().to_string());
            }
        }
        let locality = [self.city.trim(), self.region.trim(), self.postcode.trim()]
            .iter()
            .filter(|s| !s.is_empty())
            .cloned()
            .collect::<Vec<_>>()
            .join(" ");
        if !locality.is_empty() {
            out.push(locality);
        }
        if !self.country.trim().is_empty() {
            out.push(self.country.trim().to_string());
        }
        out
    }

    pub fn envelope_name(&self) -> &str {
        if self.envelope.trim().is_empty() { &self.greeting } else { &self.envelope }
    }

    pub fn tag_list(&self) -> Vec<&str> {
        self.tags.split(',').map(str::trim).filter(|t| !t.is_empty()).collect()
    }
}

/// Editable household fields. `None` = leave unchanged.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HouseholdInput {
    pub name: Option<String>,
    pub greeting: Option<String>,
    pub envelope: Option<String>,
    pub email: Option<String>,
    pub addr1: Option<String>,
    pub addr2: Option<String>,
    pub city: Option<String>,
    pub region: Option<String>,
    pub postcode: Option<String>,
    pub country: Option<String>,
    pub side: Option<String>,
    pub tier: Option<i64>,
    pub australia: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unlikely: Option<bool>,
    pub tags: Option<String>,
    pub events: Option<Vec<String>>,
    pub notes: Option<String>,
    /// Only honoured on create.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub guests: Vec<GuestInput>,
    /// Only honoured on create: who added it (defaults to the actor).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub added_by: Option<String>,
}

impl HouseholdInput {
    fn fields(&self) -> Vec<(&'static str, Option<String>)> {
        vec![
            // `name`/`greeting` are ignored: the household's name is always built from its people.
            ("email", self.email.clone()),
            ("addr1", self.addr1.clone()),
            ("addr2", self.addr2.clone()),
            ("city", self.city.clone()),
            ("region", self.region.clone()),
            ("postcode", self.postcode.clone()),
            ("country", self.country.clone()),
            ("side", self.side.clone()),
            ("tier", self.tier.map(|t| t.to_string())),
            ("australia", self.australia.map(|a| (a as i64).to_string())),
            ("unlikely", self.unlikely.map(|a| (a as i64).to_string())),
            ("tags", self.tags.clone()),
            ("events", self.events.as_ref().map(|e| serde_json::to_string(e).unwrap())),
            ("notes", self.notes.clone()),
        ]
    }

    pub fn validate(&self, valid_events: &[String]) -> Result<()> {
        if let Some(side) = &self.side {
            if !matches!(side.as_str(), "a" | "b" | "both") {
                bail!("side must be one of a, b, both");
            }
        }
        if let Some(t) = self.tier {
            if !(1..=3).contains(&t) {
                bail!("tier must be 1, 2 or 3");
            }
        }
        if let Some(evs) = &self.events {
            for e in evs {
                if !valid_events.contains(e) {
                    bail!("unknown event '{e}' (valid: {})", valid_events.join(", "));
                }
            }
        }
        if let Some(email) = &self.email {
            if !email.is_empty() && !plausible_email(email) {
                bail!("'{email}' does not look like an email address");
            }
        }
        for (k, v) in self.fields() {
            if let Some(v) = v {
                if v.len() > 2000 {
                    bail!("{k} is too long");
                }
            }
        }
        for g in &self.guests {
            g.validate()?;
        }
        Ok(())
    }
}

pub fn plausible_email(s: &str) -> bool {
    let s = s.trim();
    let Some((local, domain)) = s.split_once('@') else { return false };
    !local.is_empty() && domain.contains('.') && !s.contains(char::is_whitespace) && s.len() < 255
}

pub fn list_households(conn: &Connection, include_archived: bool) -> Result<Vec<Household>> {
    // The everyday list excludes archived households and requests still awaiting approval.
    let sql = if include_archived {
        "SELECT * FROM households ORDER BY name COLLATE NOCASE"
    } else {
        "SELECT * FROM households WHERE archived_at IS NULL AND request_status = '' ORDER BY name COLLATE NOCASE"
    };
    let mut st = conn.prepare(sql)?;
    let rows = st.query_map([], Household::from_row)?.collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

pub fn get_household(conn: &Connection, id: i64) -> Result<Option<Household>> {
    Ok(conn
        .query_row("SELECT * FROM households WHERE id = ?1", [id], Household::from_row)
        .optional()?)
}

pub fn must_household(conn: &Connection, id: i64) -> Result<Household> {
    get_household(conn, id)?.ok_or_else(|| anyhow!("household {id} not found"))
}

pub fn household_by_token(conn: &Connection, token: &str) -> Result<Option<Household>> {
    Ok(conn
        .query_row(
            "SELECT * FROM households WHERE token_hash = ?1 AND archived_at IS NULL AND request_status = ''",
            [tokens::hash_token(token)],
            Household::from_row,
        )
        .optional()?)
}

pub fn create_household(
    conn: &Connection,
    actor: &str,
    secret: &[u8],
    input: &HouseholdInput,
    default_events: &[String],
) -> Result<i64> {
    create_household_as(conn, actor, actor, false, secret, input, default_events)
}

/// Create a household recording who added it. `pending` = a request awaiting approval.
pub fn create_household_as(
    conn: &Connection,
    actor: &str,
    added_by: &str,
    pending: bool,
    secret: &[u8],
    input: &HouseholdInput,
    default_events: &[String],
) -> Result<i64> {
    if !input.guests.iter().any(|g| g.first_name.as_deref().is_some_and(|n| !n.trim().is_empty()) && g.plus_one_label.as_deref().is_none_or(str::is_empty)) {
        bail!("a household needs at least one named person");
    }
    let ts = now();
    let events = input.events.clone().unwrap_or_else(|| default_events.to_vec());
    let tx = crate::db::begin(conn)?;
    tx.execute(
        "INSERT INTO households (name, greeting, events, created_at, updated_at) VALUES ('', '', ?1, ?2, ?2)",
        params![serde_json::to_string(&events)?, ts],
    )?;
    let id = tx.last_insert_rowid();
    let mut rest = input.clone();
    rest.name = None;
    rest.greeting = None;
    rest.events = None;
    apply_household_fields(&tx, id, &rest)?;
    let token = tokens::household_token(secret, id, 1);
    tx.execute("UPDATE households SET token_hash = ?1 WHERE id = ?2", params![tokens::hash_token(&token), id])?;
    let added = input.added_by.clone().filter(|a| !a.trim().is_empty()).unwrap_or_else(|| added_by.to_string());
    tx.execute(
        "UPDATE households SET added_by = ?1, request_status = ?2 WHERE id = ?3",
        params![added.trim(), if pending { "pending" } else { "" }, id],
    )?;
    for (i, g) in input.guests.iter().enumerate() {
        let mut g = g.clone();
        g.sort.get_or_insert(i as i64);
        insert_guest(&tx, id, &g)?;
    }
    let name = household_name(&guests_for(&tx, id)?);
    tx.execute("UPDATE households SET name = ?1, greeting = ?1 WHERE id = ?2", params![name, id])?;
    audit(&tx, actor, if pending { "household.request" } else { "household.create" }, "household", &id.to_string(), &serde_json::to_value(input)?)?;
    tx.commit()?;
    Ok(id)
}

/// A household's name ("Dear ..."): its people's first names, adults then children, in their
/// listed order, like "Ben, Sarah & Tom". Plus ones count once we know their name.
pub fn household_name(guests: &[Guest]) -> String {
    let named = |child: bool| {
        guests
            .iter()
            .filter(move |g| g.archived_at.is_none() && g.is_child == child && !g.unnamed_plus_one())
            .map(|g| g.first_name.trim())
            .filter(|n| !n.is_empty())
    };
    let names: Vec<&str> = named(false).chain(named(true)).collect();
    match names.as_slice() {
        [] => String::new(),
        [one] => one.to_string(),
        [init @ .., last] => format!("{} & {}", init.join(", "), last),
    }
}

/// Re-derive a household's name after its people change. Leaves it alone if nobody is named.
pub fn refresh_name(conn: &Connection, actor: &str, household_id: i64) -> Result<()> {
    let name = household_name(&guests_for(conn, household_id)?);
    let old: String = conn.query_row("SELECT name FROM households WHERE id = ?1", [household_id], |r| r.get(0))?;
    if name.is_empty() || name == old {
        return Ok(());
    }
    conn.execute("UPDATE households SET name = ?1, greeting = ?1, envelope = '' WHERE id = ?2", params![name, household_id])?;
    audit(conn, actor, "household.rename", "household", &household_id.to_string(), &json!({"from": old, "to": name}))?;
    Ok(())
}

/// Bring every household's name in line with its people (run at startup).
pub fn refresh_all_names(conn: &Connection) -> Result<usize> {
    let ids: Vec<i64> = conn.prepare("SELECT id FROM households")?.query_map([], |r| r.get(0))?.collect::<rusqlite::Result<_>>()?;
    let before: i64 = conn.query_row("SELECT COUNT(*) FROM audit WHERE action = 'household.rename'", [], |r| r.get(0))?;
    let tx = crate::db::begin(conn)?;
    for id in ids {
        refresh_name(&tx, "system", id)?;
    }
    tx.commit()?;
    let after: i64 = conn.query_row("SELECT COUNT(*) FROM audit WHERE action = 'household.rename'", [], |r| r.get(0))?;
    Ok((after - before) as usize)
}

fn apply_household_fields(conn: &Connection, id: i64, input: &HouseholdInput) -> Result<BTreeMap<String, String>> {
    let mut changed = BTreeMap::new();
    for (col, val) in input.fields() {
        if let Some(v) = val {
            let v = v.trim().to_string();
            conn.execute(&format!("UPDATE households SET {col} = ?1 WHERE id = ?2"), params![v, id])?;
            changed.insert(col.to_string(), v);
        }
    }
    if !changed.is_empty() {
        conn.execute("UPDATE households SET updated_at = ?1 WHERE id = ?2", params![now(), id])?;
    }
    Ok(changed)
}

pub fn update_household(conn: &Connection, actor: &str, id: i64, input: &HouseholdInput) -> Result<()> {
    must_household(conn, id)?;
    if !input.guests.is_empty() {
        bail!("use guest commands to change guests of an existing household");
    }
    let tx = crate::db::begin(conn)?;
    let changed = apply_household_fields(&tx, id, input)?;
    if !changed.is_empty() {
        audit(&tx, actor, "household.update", "household", &id.to_string(), &json!(changed))?;
    }
    tx.commit()?;
    Ok(())
}

pub fn set_household_archived(conn: &Connection, actor: &str, id: i64, archived: bool) -> Result<()> {
    must_household(conn, id)?;
    let tx = crate::db::begin(conn)?;
    let val = archived.then(now);
    tx.execute("UPDATE households SET archived_at = ?1, updated_at = ?2 WHERE id = ?3", params![val, now(), id])?;
    audit(&tx, actor, if archived { "household.archive" } else { "household.restore" }, "household", &id.to_string(), &json!({}))?;
    tx.commit()?;
    Ok(())
}

/// Households requested by helpers, waiting for an owner.
pub fn pending_requests(conn: &Connection) -> Result<Vec<Household>> {
    let mut st = conn.prepare("SELECT * FROM households WHERE request_status = 'pending' AND archived_at IS NULL ORDER BY created_at")?;
    Ok(st.query_map([], Household::from_row)?.collect::<rusqlite::Result<Vec<_>>>()?)
}

/// Approve (add to the list) or decline (archive) a requested household.
pub fn review_request(conn: &Connection, actor: &str, id: i64, approve: bool) -> Result<()> {
    let h = must_household(conn, id)?;
    if h.request_status != "pending" {
        bail!("{} isn't waiting for approval", h.name);
    }
    let tx = crate::db::begin(conn)?;
    if approve {
        tx.execute("UPDATE households SET request_status = '', updated_at = ?1 WHERE id = ?2", params![now(), id])?;
    } else {
        tx.execute("UPDATE households SET request_status = 'declined', archived_at = ?1, updated_at = ?1 WHERE id = ?2", params![now(), id])?;
    }
    audit(&tx, actor, if approve { "household.request_approved" } else { "household.request_declined" }, "household", &id.to_string(), &json!({"added_by": h.added_by}))?;
    tx.commit()?;
    Ok(())
}

/// Mark (or unmark) a household as checked: names and details are all correct.
pub fn set_checked(conn: &Connection, actor: &str, who: &str, id: i64, checked: bool) -> Result<()> {
    must_household(conn, id)?;
    let tx = crate::db::begin(conn)?;
    if checked {
        tx.execute("UPDATE households SET checked_at = ?1, checked_by = ?2 WHERE id = ?3", params![now(), who, id])?;
    } else {
        tx.execute("UPDATE households SET checked_at = NULL, checked_by = NULL WHERE id = ?1", [id])?;
    }
    audit(&tx, actor, if checked { "household.checked" } else { "household.unchecked" }, "household", &id.to_string(), &json!({}))?;
    tx.commit()?;
    Ok(())
}

pub fn rotate_token(conn: &Connection, actor: &str, secret: &[u8], id: i64) -> Result<String> {
    let h = must_household(conn, id)?;
    let v = h.token_version + 1;
    let token = tokens::household_token(secret, id, v);
    let tx = crate::db::begin(conn)?;
    tx.execute(
        "UPDATE households SET token_version = ?1, token_hash = ?2, updated_at = ?3 WHERE id = ?4",
        params![v, tokens::hash_token(&token), now(), id],
    )?;
    audit(&tx, actor, "household.rotate_link", "household", &id.to_string(), &json!({"version": v}))?;
    tx.commit()?;
    Ok(token)
}

/// Re-derive every token hash from the current secret (after restoring onto a new box, say).
pub fn rehash_all_tokens(conn: &Connection, secret: &[u8]) -> Result<usize> {
    let tx = crate::db::begin(conn)?;
    let rows: Vec<(i64, i64)> = tx
        .prepare("SELECT id, token_version FROM households")?
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<rusqlite::Result<_>>()?;
    for (id, v) in &rows {
        tx.execute(
            "UPDATE households SET token_hash = ?1 WHERE id = ?2",
            params![tokens::hash_token(&tokens::household_token(secret, *id, *v)), id],
        )?;
    }
    tx.commit()?;
    Ok(rows.len())
}

pub fn household_token(secret: &[u8], h: &Household) -> String {
    tokens::household_token(secret, h.id, h.token_version)
}

pub fn mark_viewed(conn: &Connection, id: i64) -> Result<()> {
    let ts = now();
    conn.execute(
        "UPDATE households SET first_viewed_at = COALESCE(first_viewed_at, ?1), last_viewed_at = ?1 WHERE id = ?2",
        params![ts, id],
    )?;
    Ok(())
}

pub fn mark_sent(conn: &Connection, id: i64, which: &str) -> Result<()> {
    let col = match which {
        "save_the_date" => "std_sent_at",
        "invitation" => "invite_sent_at",
        _ => bail!("unknown send kind {which}"),
    };
    conn.execute(&format!("UPDATE households SET {col} = ?1 WHERE id = ?2"), params![now(), id])?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Calendar versions

/// The iCalendar SEQUENCE for each event: it goes up by one whenever the event's details
/// (time, place, dress, notes) change, so calendars that see the event again update it in
/// place rather than keeping the old one. Stored as "seq:fingerprint" in settings.
pub fn calendar_sequences(conn: &Connection, events: &[crate::config::Event]) -> Result<HashMap<String, i64>> {
    let mut out = HashMap::new();
    for e in events {
        let fp = tokens::sha256_hex(&format!(
            "{}|{:?}|{:?}|{}|{}|{:?}|{:?}",
            e.name, e.starts, e.ends, e.venue, e.address, e.dress, e.note
        ))[..16]
            .to_string();
        let key = format!("ics_sequence:{}", e.slug);
        let stored = setting(conn, &key)?;
        let (seq, old_fp) = stored
            .as_deref()
            .and_then(|v| v.split_once(':'))
            .map(|(n, f)| (n.parse::<i64>().unwrap_or(0), f.to_string()))
            .unwrap_or((0, String::new()));
        let seq = if stored.is_none() {
            0
        } else if old_fp == fp {
            seq
        } else {
            seq + 1
        };
        if old_fp != fp {
            set_setting(conn, "system", &key, &format!("{seq}:{fp}"))?;
        }
        out.insert(e.slug.clone(), seq);
    }
    Ok(out)
}

/// Record an "Add to calendar" download and which version of each event it held.
pub fn record_calendar_download(conn: &Connection, household_id: i64, sequences: &BTreeMap<String, i64>) -> Result<()> {
    conn.execute(
        "INSERT INTO calendar_downloads (household_id, at, sequences) VALUES (?1, ?2, ?3)",
        params![household_id, now(), serde_json::to_string(sequences)?],
    )?;
    Ok(())
}

#[derive(Debug, Clone, Default)]
pub struct CalendarDownloads {
    pub count: i64,
    pub last_at: Option<String>,
    /// Event versions in the latest download.
    pub last: BTreeMap<String, i64>,
}

pub fn calendar_downloads(conn: &Connection, household_id: i64) -> Result<CalendarDownloads> {
    let count: i64 = conn.query_row("SELECT COUNT(*) FROM calendar_downloads WHERE household_id = ?1", [household_id], |r| r.get(0))?;
    let last: Option<(String, String)> = conn
        .query_row(
            "SELECT at, sequences FROM calendar_downloads WHERE household_id = ?1 ORDER BY at DESC, id DESC LIMIT 1",
            [household_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    Ok(CalendarDownloads {
        count,
        last: last.as_ref().and_then(|(_, s)| serde_json::from_str(s).ok()).unwrap_or_default(),
        last_at: last.map(|(at, _)| at),
    })
}

/// When each household last added its events to a calendar.
pub fn calendar_last_downloads(conn: &Connection) -> Result<HashMap<i64, String>> {
    let mut st = conn.prepare("SELECT household_id, MAX(at) FROM calendar_downloads GROUP BY household_id")?;
    let rows = st.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?.collect::<rusqlite::Result<HashMap<_, _>>>()?;
    Ok(rows)
}

// ---------------------------------------------------------------------------
// Email sends and opens

#[derive(Debug, Clone, Serialize)]
pub struct EmailSend {
    pub id: String,
    pub household_id: i64,
    pub address: String,
    pub kind: String,
    pub test: bool,
    pub queued_at: String,
    pub sent_at: Option<String>,
    pub opened_at: Option<String>,
    pub last_opened_at: Option<String>,
    pub opens: i64,
}

impl EmailSend {
    fn from_row(r: &Row) -> rusqlite::Result<Self> {
        Ok(Self {
            id: r.get("id")?,
            household_id: r.get("household_id")?,
            address: r.get("address")?,
            kind: r.get("kind")?,
            test: r.get("test")?,
            queued_at: r.get("queued_at")?,
            sent_at: r.get("sent_at")?,
            opened_at: r.get("opened_at")?,
            last_opened_at: r.get("last_opened_at")?,
            opens: r.get("opens")?,
        })
    }
}

pub fn create_send(conn: &Connection, household_id: i64, address: &str, kind: &str, test: bool) -> Result<String> {
    let id = tokens::random_secret(16);
    conn.execute(
        "INSERT INTO email_sends (id, household_id, address, kind, test, queued_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![id, household_id, address, kind, test, now()],
    )?;
    Ok(id)
}

pub fn get_send(conn: &Connection, id: &str) -> Result<Option<EmailSend>> {
    Ok(conn.query_row("SELECT * FROM email_sends WHERE id = ?1", [id], EmailSend::from_row).optional()?)
}

pub fn mark_send_sent(conn: &Connection, id: &str, provider_id: &str) -> Result<()> {
    conn.execute("UPDATE email_sends SET sent_at = ?1, provider_id = ?2 WHERE id = ?3", params![now(), provider_id, id])?;
    Ok(())
}

/// The card picture was loaded. Returns the send if it exists.
pub fn record_open(conn: &Connection, id: &str) -> Result<Option<EmailSend>> {
    let ts = now();
    let n = conn.execute(
        "UPDATE email_sends SET opens = opens + 1, last_opened_at = ?1, opened_at = COALESCE(opened_at, ?1)
         WHERE id = ?2 AND sent_at IS NOT NULL",
        params![ts, id],
    )?;
    if n == 0 {
        return Ok(None);
    }
    get_send(conn, id)
}

pub fn sends_for(conn: &Connection, household_id: i64) -> Result<Vec<EmailSend>> {
    let mut st = conn.prepare("SELECT * FROM email_sends WHERE household_id = ?1 ORDER BY queued_at DESC")?;
    let rows = st.query_map([household_id], EmailSend::from_row)?.collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

/// Per household and kind (real sends only): how many were opened, and the first open.
#[derive(Debug, Clone, Default)]
pub struct MailStat {
    pub opened: i64,
    pub first_opened_at: Option<String>,
}

pub fn mail_stats(conn: &Connection) -> Result<HashMap<(i64, String), MailStat>> {
    let mut st = conn.prepare(
        "SELECT household_id, kind, COUNT(opened_at), MIN(opened_at) FROM email_sends
         WHERE test = 0 AND sent_at IS NOT NULL GROUP BY household_id, kind",
    )?;
    let rows = st
        .query_map([], |r| Ok(((r.get(0)?, r.get(1)?), MailStat { opened: r.get(2)?, first_opened_at: r.get(3)? })))?
        .collect::<rusqlite::Result<HashMap<_, _>>>()?;
    Ok(rows)
}

/// Households that opened their invitation before `cutoff`, still haven't replied, and haven't
/// been flagged yet: flag them and queue a Discord message, in one transaction each.
pub fn flag_unanswered_invitations(conn: &Connection, cutoff: &str, base_url: &str) -> Result<usize> {
    let due: Vec<(i64, String, String)> = conn
        .prepare(
            "SELECT h.id, h.name, MIN(s.opened_at) FROM households h JOIN email_sends s ON s.household_id = h.id
             WHERE s.kind = 'invitation' AND s.test = 0 AND s.opened_at IS NOT NULL
               AND h.rsvp_at IS NULL AND h.nudged_at IS NULL AND h.archived_at IS NULL
             GROUP BY h.id HAVING MIN(s.opened_at) <= ?1",
        )?
        .query_map([cutoff], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
        .collect::<rusqlite::Result<_>>()?;
    for (hid, name, opened) in &due {
        let tx = crate::db::begin(conn)?;
        tx.execute("UPDATE households SET nudged_at = ?1 WHERE id = ?2", params![now(), hid])?;
        audit(&tx, "system", "household.invite_unanswered", "household", &hid.to_string(), &json!({"opened_at": opened}))?;
        enqueue(
            &tx,
            "discord",
            &json!({"type": "nudge", "text": format!("👀 **{name}** opened their invitation over 2 days ago and hasn't replied yet.\n{base_url}/admin/households/{hid}")}),
        )?;
        tx.commit()?;
    }
    Ok(due.len())
}

// ---------------------------------------------------------------------------
// Guests

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Guest {
    pub id: i64,
    pub household_id: i64,
    pub first_name: String,
    pub last_name: String,
    pub email: String,
    pub is_child: bool,
    /// Legacy: plus-ones created by the old RSVP flow.
    pub plus_one_of: Option<i64>,
    /// Non-empty marks a plus-one slot ("Ben's plus one"). Their name stays empty until the
    /// household tells us who is coming; the slot stays a plus-one after that.
    pub plus_one_label: String,
    pub dietary: String,
    pub notes: String,
    pub sort: i64,
    pub archived_at: Option<String>,
}

impl Guest {
    fn from_row(r: &Row) -> rusqlite::Result<Self> {
        Ok(Self {
            id: r.get("id")?,
            household_id: r.get("household_id")?,
            first_name: r.get("first_name")?,
            last_name: r.get("last_name")?,
            email: r.get("email")?,
            is_child: r.get("is_child")?,
            plus_one_of: r.get("plus_one_of")?,
            plus_one_label: r.get("plus_one_label")?,
            dietary: r.get("dietary")?,
            notes: r.get("notes")?,
            sort: r.get("sort")?,
            archived_at: r.get("archived_at")?,
        })
    }

    /// Is this person invited to `event`? Events are invited per household.
    pub fn invited(&self, h: &Household, event: &str) -> bool {
        h.events.iter().any(|e| e == event)
    }

    pub fn full_name(&self) -> String {
        let n = format!("{} {}", self.first_name, self.last_name).trim().to_string();
        if n.is_empty() { self.plus_one_label.clone() } else { n }
    }

    pub fn is_plus_one(&self) -> bool {
        !self.plus_one_label.is_empty()
    }

    /// A plus-one slot whose person we don't know yet.
    pub fn unnamed_plus_one(&self) -> bool {
        self.is_plus_one() && self.first_name.trim().is_empty()
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GuestInput {
    pub first_name: Option<String>,
    pub last_name: Option<String>,
    pub email: Option<String>,
    pub is_child: Option<bool>,
    /// Makes this person a plus-one slot, e.g. "Ben's plus one" (first name may then be empty).
    pub plus_one_label: Option<String>,
    pub dietary: Option<String>,
    pub notes: Option<String>,
    pub sort: Option<i64>,
}

impl GuestInput {
    pub fn validate(&self) -> Result<()> {
        if let Some(e) = &self.email {
            if !e.is_empty() && !plausible_email(e) {
                bail!("'{e}' does not look like an email address");
            }
        }
        for v in [&self.first_name, &self.last_name, &self.plus_one_label, &self.dietary, &self.notes].into_iter().flatten() {
            if v.len() > 2000 {
                bail!("field too long");
            }
        }
        Ok(())
    }

    fn fields(&self) -> Vec<(&'static str, Option<rusqlite::types::Value>)> {
        use rusqlite::types::Value as V;
        let s = |o: &Option<String>| o.as_ref().map(|v| V::Text(v.trim().to_string()));
        let b = |o: &Option<bool>| o.map(|v| V::Integer(v as i64));
        vec![
            ("first_name", s(&self.first_name)),
            ("last_name", s(&self.last_name)),
            ("email", s(&self.email)),
            ("is_child", b(&self.is_child)),
            ("plus_one_label", s(&self.plus_one_label)),
            ("dietary", s(&self.dietary)),
            ("notes", s(&self.notes)),
            ("sort", self.sort.map(V::Integer)),
        ]
    }
}

pub fn guests_for(conn: &Connection, household_id: i64) -> Result<Vec<Guest>> {
    let mut st = conn.prepare(
        "SELECT * FROM guests WHERE household_id = ?1 AND archived_at IS NULL ORDER BY plus_one_of IS NOT NULL, sort, id",
    )?;
    let rows = st.query_map([household_id], Guest::from_row)?.collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

pub fn all_guests(conn: &Connection) -> Result<Vec<Guest>> {
    let mut st = conn.prepare(
        "SELECT g.* FROM guests g JOIN households h ON h.id = g.household_id
         WHERE g.archived_at IS NULL AND h.archived_at IS NULL
         ORDER BY h.name COLLATE NOCASE, g.plus_one_of IS NOT NULL, g.sort, g.id",
    )?;
    let rows = st.query_map([], Guest::from_row)?.collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

pub fn get_guest(conn: &Connection, id: i64) -> Result<Option<Guest>> {
    Ok(conn.query_row("SELECT * FROM guests WHERE id = ?1", [id], Guest::from_row).optional()?)
}

pub fn must_guest(conn: &Connection, id: i64) -> Result<Guest> {
    get_guest(conn, id)?.ok_or_else(|| anyhow!("guest {id} not found"))
}

fn insert_guest(conn: &Connection, household_id: i64, g: &GuestInput) -> Result<i64> {
    let first = g.first_name.as_deref().map(str::trim).filter(|s| !s.is_empty());
    let slot = g.plus_one_label.as_deref().is_some_and(|l| !l.trim().is_empty());
    let first = match first {
        Some(f) => f,
        None if slot => "",
        None => bail!("guest first_name is required"),
    };
    let ts = now();
    conn.execute(
        "INSERT INTO guests (household_id, first_name, created_at, updated_at) VALUES (?1, ?2, ?3, ?3)",
        params![household_id, first, ts],
    )?;
    let id = conn.last_insert_rowid();
    let mut rest = g.clone();
    rest.first_name = None;
    apply_guest_fields(conn, id, &rest)?;
    Ok(id)
}

fn apply_guest_fields(conn: &Connection, id: i64, g: &GuestInput) -> Result<BTreeMap<String, serde_json::Value>> {
    let mut changed = BTreeMap::new();
    for (col, val) in g.fields() {
        if let Some(v) = val {
            if col == "first_name" && matches!(&v, rusqlite::types::Value::Text(t) if t.is_empty()) {
                let label: String = conn.query_row("SELECT plus_one_label FROM guests WHERE id = ?1", [id], |r| r.get(0))?;
                if label.is_empty() && g.plus_one_label.as_deref().is_none_or(str::is_empty) {
                    bail!("first_name cannot be empty");
                }
            }
            let jv = match &v {
                rusqlite::types::Value::Text(t) if col == "events" => serde_json::from_str(t).unwrap_or(json!(t)),
                rusqlite::types::Value::Text(t) => json!(t),
                rusqlite::types::Value::Integer(i) => json!(i),
                _ => json!(null),
            };
            conn.execute(&format!("UPDATE guests SET {col} = ?1 WHERE id = ?2"), params![v, id])?;
            changed.insert(col.to_string(), jv);
        }
    }
    if !changed.is_empty() {
        conn.execute("UPDATE guests SET updated_at = ?1 WHERE id = ?2", params![now(), id])?;
    }
    Ok(changed)
}

pub fn create_guest(conn: &Connection, actor: &str, household_id: i64, g: &GuestInput) -> Result<i64> {
    must_household(conn, household_id)?;
    g.validate()?;
    let tx = crate::db::begin(conn)?;
    let mut g2 = g.clone();
    if g2.sort.is_none() {
        let n: i64 = tx.query_row("SELECT COUNT(*) FROM guests WHERE household_id = ?1", [household_id], |r| r.get(0))?;
        g2.sort = Some(n);
    }
    let id = insert_guest(&tx, household_id, &g2)?;
    audit(&tx, actor, "guest.create", "guest", &id.to_string(), &json!({"household_id": household_id, "fields": g}))?;
    refresh_name(&tx, actor, household_id)?;
    tx.commit()?;
    Ok(id)
}

pub fn update_guest(conn: &Connection, actor: &str, id: i64, g: &GuestInput) -> Result<()> {
    let hid = must_guest(conn, id)?.household_id;
    g.validate()?;
    let tx = crate::db::begin(conn)?;
    let changed = apply_guest_fields(&tx, id, g)?;
    if !changed.is_empty() {
        audit(&tx, actor, "guest.update", "guest", &id.to_string(), &json!(changed))?;
        refresh_name(&tx, actor, hid)?;
    }
    tx.commit()?;
    Ok(())
}

pub fn move_guest(conn: &Connection, actor: &str, id: i64, household_id: i64) -> Result<()> {
    let from = must_guest(conn, id)?.household_id;
    must_household(conn, household_id)?;
    let tx = crate::db::begin(conn)?;
    tx.execute("UPDATE guests SET household_id = ?1, updated_at = ?2 WHERE id = ?3", params![household_id, now(), id])?;
    audit(&tx, actor, "guest.move", "guest", &id.to_string(), &json!({"household_id": household_id}))?;
    refresh_name(&tx, actor, from)?;
    refresh_name(&tx, actor, household_id)?;
    tx.commit()?;
    Ok(())
}

pub fn archive_guest(conn: &Connection, actor: &str, id: i64) -> Result<()> {
    let hid = must_guest(conn, id)?.household_id;
    let tx = crate::db::begin(conn)?;
    tx.execute("UPDATE guests SET archived_at = ?1, updated_at = ?1 WHERE id = ?2", params![now(), id])?;
    audit(&tx, actor, "guest.archive", "guest", &id.to_string(), &json!({}))?;
    refresh_name(&tx, actor, hid)?;
    tx.commit()?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Responses

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Response {
    pub guest_id: i64,
    pub event_slug: String,
    pub attending: bool,
    pub meal: String,
    pub updated_at: String,
}

pub fn responses_for(conn: &Connection, household_id: i64) -> Result<Vec<Response>> {
    let mut st = conn.prepare(
        "SELECT r.guest_id, r.event_slug, r.attending, r.meal, r.updated_at FROM responses r
         JOIN guests g ON g.id = r.guest_id WHERE g.household_id = ?1",
    )?;
    let rows = st
        .query_map([household_id], |r| {
            Ok(Response { guest_id: r.get(0)?, event_slug: r.get(1)?, attending: r.get(2)?, meal: r.get(3)?, updated_at: r.get(4)? })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

pub fn all_responses(conn: &Connection) -> Result<HashMap<(i64, String), Response>> {
    let mut st = conn.prepare("SELECT guest_id, event_slug, attending, meal, updated_at FROM responses")?;
    let rows = st.query_map([], |r| {
        Ok(Response { guest_id: r.get(0)?, event_slug: r.get(1)?, attending: r.get(2)?, meal: r.get(3)?, updated_at: r.get(4)? })
    })?;
    let mut out = HashMap::new();
    for r in rows {
        let r = r?;
        out.insert((r.guest_id, r.event_slug.clone()), r);
    }
    Ok(out)
}

#[derive(Debug, Clone, Serialize)]
pub struct SubmissionRow {
    pub id: String,
    pub household_id: i64,
    pub received_at: String,
    pub payload: serde_json::Value,
}

pub fn submissions(conn: &Connection, household_id: Option<i64>, limit: i64) -> Result<Vec<SubmissionRow>> {
    let mut st = conn.prepare(
        "SELECT id, household_id, received_at, payload FROM submissions
         WHERE (?1 IS NULL OR household_id = ?1) ORDER BY received_at DESC LIMIT ?2",
    )?;
    let rows = st
        .query_map(params![household_id, limit], |r| {
            let p: String = r.get(3)?;
            Ok(SubmissionRow {
                id: r.get(0)?,
                household_id: r.get(1)?,
                received_at: r.get(2)?,
                payload: serde_json::from_str(&p).unwrap_or(json!(null)),
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

pub fn submission_exists(conn: &Connection, id: &str) -> Result<bool> {
    Ok(conn.query_row("SELECT 1 FROM submissions WHERE id = ?1", [id], |_| Ok(())).optional()?.is_some())
}

/// Household RSVP status relative to what they were invited to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    NoReply,
    Attending,
    Partial,
    Declined,
}

impl Status {
    pub fn label(self) -> &'static str {
        match self {
            Status::NoReply => "No reply",
            Status::Attending => "Attending",
            Status::Partial => "Partly attending",
            Status::Declined => "Declined",
        }
    }
}

pub fn household_status(h: &Household, guests: &[Guest], resp: &HashMap<(i64, String), Response>) -> Status {
    if h.rsvp_at.is_none() {
        return Status::NoReply;
    }
    let (mut yes, mut no) = (0, 0);
    for g in guests.iter().filter(|g| g.plus_one_of.is_none()) {
        for e in h.events.iter().filter(|e| g.invited(h, e)) {
            match resp.get(&(g.id, e.clone())) {
                Some(r) if r.attending => yes += 1,
                Some(_) => no += 1,
                None => {}
            }
        }
    }
    match (yes, no) {
        (0, 0) => Status::NoReply,
        (_, 0) => Status::Attending,
        (0, _) => Status::Declined,
        _ => Status::Partial,
    }
}

// ---------------------------------------------------------------------------
// Settings

pub fn setting(conn: &Connection, key: &str) -> Result<Option<String>> {
    Ok(conn.query_row("SELECT value FROM settings WHERE key = ?1", [key], |r| r.get(0)).optional()?)
}

pub fn set_setting(conn: &Connection, actor: &str, key: &str, value: &str) -> Result<()> {
    let tx = crate::db::begin(conn)?;
    tx.execute(
        "INSERT INTO settings (key, value) VALUES (?1, ?2) ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        params![key, value],
    )?;
    if actor != "system" {
        audit(&tx, actor, "setting.set", "setting", key, &json!({"value": value}))?;
    }
    tx.commit()?;
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Phase {
    SaveTheDate,
    RsvpOpen,
    RsvpClosed,
}

impl Phase {
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "save-the-date" => Some(Self::SaveTheDate),
            "rsvp-open" => Some(Self::RsvpOpen),
            "rsvp-closed" => Some(Self::RsvpClosed),
            _ => None,
        }
    }
    pub fn as_str(self) -> &'static str {
        match self {
            Self::SaveTheDate => "save-the-date",
            Self::RsvpOpen => "rsvp-open",
            Self::RsvpClosed => "rsvp-closed",
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            Self::SaveTheDate => "Save the date",
            Self::RsvpOpen => "RSVPs open",
            Self::RsvpClosed => "RSVPs closed",
        }
    }
}

pub fn phase(conn: &Connection) -> Result<Phase> {
    Ok(setting(conn, "phase")?.as_deref().and_then(Phase::parse).unwrap_or(Phase::SaveTheDate))
}

// ---------------------------------------------------------------------------
// Audit + outbox

pub fn audit(conn: &Connection, actor: &str, action: &str, entity: &str, entity_id: &str, detail: &serde_json::Value) -> Result<()> {
    conn.execute(
        "INSERT INTO audit (at, actor, action, entity, entity_id, detail) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![now(), actor, action, entity, entity_id, detail.to_string()],
    )?;
    Ok(())
}

#[derive(Debug, Clone, Serialize)]
pub struct AuditRow {
    pub id: i64,
    pub at: String,
    pub actor: String,
    pub action: String,
    pub entity: String,
    pub entity_id: String,
    pub detail: String,
}

pub fn audit_log(conn: &Connection, limit: i64, entity: Option<(&str, &str)>) -> Result<Vec<AuditRow>> {
    let (e, eid) = entity.unwrap_or(("", ""));
    let mut st = conn.prepare(
        "SELECT id, at, actor, action, entity, entity_id, detail FROM audit
         WHERE (?1 = '' OR (entity = ?1 AND entity_id = ?2)) ORDER BY id DESC LIMIT ?3",
    )?;
    let rows = st
        .query_map(params![e, eid, limit], |r| {
            Ok(AuditRow {
                id: r.get(0)?,
                at: r.get(1)?,
                actor: r.get(2)?,
                action: r.get(3)?,
                entity: r.get(4)?,
                entity_id: r.get(5)?,
                detail: r.get(6)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

pub fn enqueue(conn: &Connection, kind: &str, payload: &serde_json::Value) -> Result<i64> {
    let ts = now();
    conn.execute(
        "INSERT INTO outbox (kind, payload, created_at, next_attempt_at) VALUES (?1, ?2, ?3, ?3)",
        params![kind, payload.to_string(), ts],
    )?;
    Ok(conn.last_insert_rowid())
}

// ---------------------------------------------------------------------------
// Users, credentials, sessions, invites

#[derive(Debug, Clone, Serialize)]
pub struct User {
    pub id: String,
    pub name: String,
    pub role: String,
    pub created_at: String,
    pub approved_by: Option<String>,
    pub last_login_at: Option<String>,
    /// "" (helper/owner), "a_parent" or "b_parent": one of the couple's parents.
    pub relation: String,
}

impl User {
    fn from_row(r: &Row) -> rusqlite::Result<Self> {
        Ok(Self {
            id: r.get("id")?,
            name: r.get("name")?,
            role: r.get("role")?,
            created_at: r.get("created_at")?,
            approved_by: r.get("approved_by")?,
            last_login_at: r.get("last_login_at")?,
            relation: r.get("relation")?,
        })
    }
    pub fn is_owner(&self) -> bool {
        self.role == "owner"
    }
    /// Team access as one value: owner, a_parent, b_parent, helper, pending or disabled.
    pub fn access(&self) -> &str {
        if self.role == "helper" && !self.relation.is_empty() { &self.relation } else { &self.role }
    }

    pub fn is_active(&self) -> bool {
        matches!(self.role.as_str(), "owner" | "helper")
    }
    /// Audit actor string.
    pub fn actor(&self) -> String {
        format!("user:{}", self.name)
    }
}

pub fn users(conn: &Connection) -> Result<Vec<User>> {
    let mut st = conn.prepare("SELECT * FROM users ORDER BY role = 'pending' DESC, name COLLATE NOCASE")?;
    let rows = st.query_map([], User::from_row)?.collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

pub fn get_user(conn: &Connection, id: &str) -> Result<Option<User>> {
    Ok(conn.query_row("SELECT * FROM users WHERE id = ?1", [id], User::from_row).optional()?)
}

pub fn user_by_name(conn: &Connection, name: &str) -> Result<Option<User>> {
    Ok(conn.query_row("SELECT * FROM users WHERE name = ?1", [name.trim()], User::from_row).optional()?)
}

pub fn set_user_role(conn: &Connection, actor: &str, user_id: &str, role: &str) -> Result<()> {
    if !matches!(role, "pending" | "helper" | "owner" | "disabled") {
        bail!("role must be pending, helper, owner or disabled");
    }
    let u = get_user(conn, user_id)?.ok_or_else(|| anyhow!("user not found"))?;
    if u.is_owner() && role != "owner" {
        let owners: i64 = conn.query_row("SELECT COUNT(*) FROM users WHERE role = 'owner'", [], |r| r.get(0))?;
        if owners <= 1 {
            bail!("refusing to demote the last owner");
        }
    }
    let tx = crate::db::begin(conn)?;
    tx.execute(
        "UPDATE users SET role = ?1, approved_by = COALESCE(approved_by, ?2), approved_at = COALESCE(approved_at, ?3) WHERE id = ?4",
        params![role, actor, now(), user_id],
    )?;
    if role == "disabled" || role == "pending" {
        tx.execute("DELETE FROM sessions WHERE user_id = ?1", [user_id])?;
    }
    audit(&tx, actor, "user.role", "user", user_id, &json!({"name": u.name, "from": u.role, "to": role}))?;
    tx.commit()?;
    Ok(())
}

/// Team access to (role, relation). Parents are helpers who belong to one side.
pub fn access_parts(access: &str) -> Result<(&'static str, &'static str)> {
    Ok(match access {
        "owner" => ("owner", ""),
        "helper" => ("helper", ""),
        "a_parent" => ("helper", "a_parent"),
        "b_parent" => ("helper", "b_parent"),
        "pending" => ("pending", ""),
        "disabled" => ("disabled", ""),
        _ => bail!("access must be owner, helper, a_parent, b_parent, pending or disabled"),
    })
}

pub fn set_user_access(conn: &Connection, actor: &str, user_id: &str, access: &str) -> Result<()> {
    let (role, relation) = access_parts(access)?;
    set_user_role(conn, actor, user_id, role)?;
    conn.execute("UPDATE users SET relation = ?1 WHERE id = ?2", params![relation, user_id])?;
    Ok(())
}

pub fn create_invite(conn: &Connection, actor: &str, access: &str, hours: i64) -> Result<String> {
    let (role, relation) = access_parts(access)?;
    if !matches!(role, "helper" | "owner") {
        bail!("invites are for owners, helpers or parents");
    }
    let code = tokens::random_secret(20);
    let exp = time::OffsetDateTime::now_utc() + time::Duration::hours(hours);
    let tx = crate::db::begin(conn)?;
    tx.execute(
        "INSERT INTO invites (code_hash, role, relation, created_by, created_at, expires_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![tokens::sha256_hex(&code), role, relation, actor, now(), exp.format(&time::format_description::well_known::Rfc3339)?],
    )?;
    audit(&tx, actor, "invite.create", "invite", "", &json!({"access": access, "hours": hours}))?;
    tx.commit()?;
    Ok(code)
}

/// Returns the invite's (role, relation) if the code is valid and unused.
pub fn check_invite(conn: &Connection, code: &str) -> Result<Option<(String, String)>> {
    let row: Option<(String, String, String, Option<String>)> = conn
        .query_row(
            "SELECT role, relation, expires_at, used_at FROM invites WHERE code_hash = ?1",
            [tokens::sha256_hex(code.trim())],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .optional()?;
    Ok(row.and_then(|(role, relation, exp, used)| (used.is_none() && exp > now()).then_some((role, relation))))
}

/// Permanently remove a disabled or pending account (audit history keeps their name).
pub fn delete_user(conn: &Connection, actor: &str, name: &str) -> Result<()> {
    let u = user_by_name(conn, name)?.ok_or_else(|| anyhow!("no user named {name}"))?;
    if u.is_active() {
        bail!("{} is {}; disable them first", u.name, u.role);
    }
    let tx = crate::db::begin(conn)?;
    tx.execute("DELETE FROM sessions WHERE user_id = ?1", [&u.id])?;
    tx.execute("DELETE FROM credentials WHERE user_id = ?1", [&u.id])?;
    tx.execute("DELETE FROM users WHERE id = ?1", [&u.id])?;
    audit(&tx, actor, "user.delete", "user", &u.id, &json!({"name": u.name}))?;
    tx.commit()?;
    Ok(())
}

pub fn consume_invite(conn: &Connection, code: &str, user_id: &str) -> Result<()> {
    conn.execute(
        "UPDATE invites SET used_by = ?1, used_at = ?2 WHERE code_hash = ?3 AND used_at IS NULL",
        params![user_id, now(), tokens::sha256_hex(code.trim())],
    )?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Tests

#[cfg(test)]
pub mod tests {
    use super::*;

    pub fn mem() -> crate::db::Db {
        let dir = std::env::temp_dir().join(format!("rsvp-test-{}", uuid::Uuid::new_v4()));
        crate::db::Db::open(&dir.join("t.db")).unwrap()
    }

    #[test]
    fn calendar_sequence_goes_up_only_when_an_event_changes() {
        let db = mem();
        db.with(|c| {
            let cfg: crate::config::Config = toml::from_str(include_str!("../config/wedding.example.toml")).unwrap();
            let mut events = cfg.events.clone();
            let slug = events[0].slug.clone();
            assert_eq!(calendar_sequences(c, &events)?[&slug], 0);
            assert_eq!(calendar_sequences(c, &events)?[&slug], 0, "unchanged: same number");
            events[0].venue = "Somewhere new".into();
            assert_eq!(calendar_sequences(c, &events)?[&slug], 1);
            assert_eq!(calendar_sequences(c, &events)?[&slug], 1);
            events[0].starts = Some("2027-06-12T16:00".into());
            assert_eq!(calendar_sequences(c, &events)?[&slug], 2);
            Ok(())
        })
        .unwrap();
    }

    #[test]
    fn email_opens_and_unanswered_invitations() {
        let db = mem();
        db.with(|c| {
            let input = HouseholdInput { guests: vec![GuestInput { first_name: Some("Ada".into()), ..Default::default() }], ..Default::default() };
            let hid = create_household(c, "t", &[1; 32], &input, &[])?;
            let test = create_send(c, hid, "me@example.com", "invitation", true)?;
            let real = create_send(c, hid, "ada@example.com", "invitation", false)?;
            assert!(record_open(c, &real)?.is_none(), "not sent yet, so not an open");
            mark_send_sent(c, &real, "g1")?;
            mark_send_sent(c, &test, "g2")?;
            record_open(c, &test)?;
            assert!(mail_stats(c)?.get(&(hid, "invitation".to_string())).is_none_or(|m| m.opened == 0), "tests don't count");
            let s = record_open(c, &real)?.unwrap();
            assert_eq!(s.opens, 1);
            let first = s.opened_at.clone().unwrap();
            assert_eq!(record_open(c, &real)?.unwrap().opened_at.unwrap(), first, "first open is kept");
            assert_eq!(mail_stats(c)?[&(hid, "invitation".to_string())].opened, 1);

            // Opened just now: not due. Pretend it was opened 3 days ago: flagged once, Discord queued.
            assert_eq!(flag_unanswered_invitations(c, "2000-01-01T00:00:00Z", "https://x")?, 0);
            c.execute("UPDATE email_sends SET opened_at = '2020-01-01T00:00:00Z' WHERE id = ?1", [&real])?;
            assert_eq!(flag_unanswered_invitations(c, &now(), "https://x")?, 1);
            assert_eq!(flag_unanswered_invitations(c, &now(), "https://x")?, 0, "only once");
            let n: i64 = c.query_row("SELECT COUNT(*) FROM outbox WHERE kind = 'discord' AND payload LIKE '%hasn''t replied%'", [], |r| r.get(0))?;
            assert_eq!(n, 1);
            Ok(())
        })
        .unwrap();
    }

    #[test]
    fn household_lifecycle() {
        let db = mem();
        db.with(|c| {
            let input = HouseholdInput {
                guests: vec![
                    GuestInput { first_name: Some("Jane".into()), last_name: Some("Smith".into()), ..Default::default() },
                    GuestInput { first_name: Some("John".into()), ..Default::default() },
                ],
                ..Default::default()
            };
            let id = create_household(c, "test", &[1; 32], &input, &["ceremony".into()])?;
            let h = must_household(c, id)?;
            assert_eq!(h.greeting, "Jane & John");
            assert_eq!(h.events, vec!["ceremony".to_string()]);
            let tok = household_token(&[1; 32], &h);
            assert_eq!(household_by_token(c, &tok)?.unwrap().id, id);
            let new = rotate_token(c, "test", &[1; 32], id)?;
            assert!(household_by_token(c, &tok)?.is_none());
            assert!(household_by_token(c, &new)?.is_some());
            assert_eq!(guests_for(c, id)?.len(), 2);
            assert!(c.execute("DELETE FROM audit", []).is_err(), "audit must be append-only");

            // A helper's household is a request: no working link and not listed until approved.
            let req = HouseholdInput { guests: vec![GuestInput { first_name: Some("Rex".into()), ..Default::default() }], ..Default::default() };
            let rid = create_household_as(c, "user:julie", "Julie (A's parent)", true, &[1; 32], &req, &["ceremony".into()])?;
            let rh = must_household(c, rid)?;
            assert_eq!((rh.added_by.as_str(), rh.request_status.as_str()), ("Julie (A's parent)", "pending"));
            assert!(household_by_token(c, &household_token(&[1; 32], &rh))?.is_none());
            assert!(!list_households(c, false)?.iter().any(|h| h.id == rid));
            assert_eq!(pending_requests(c)?.len(), 1);
            review_request(c, "user:brend", rid, true)?;
            assert!(household_by_token(c, &household_token(&[1; 32], &rh))?.is_some());
            assert!(list_households(c, false)?.iter().any(|h| h.id == rid));
            Ok(())
        })
        .unwrap();
    }
}

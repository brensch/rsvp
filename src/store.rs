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
    pub tags: Option<String>,
    pub events: Option<Vec<String>>,
    pub notes: Option<String>,
    /// Only honoured on create.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub guests: Vec<GuestInput>,
}

impl HouseholdInput {
    fn fields(&self) -> Vec<(&'static str, Option<String>)> {
        vec![
            ("name", self.name.clone()),
            ("greeting", self.greeting.clone()),
            ("envelope", self.envelope.clone()),
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
    let sql = if include_archived {
        "SELECT * FROM households ORDER BY name COLLATE NOCASE"
    } else {
        "SELECT * FROM households WHERE archived_at IS NULL ORDER BY name COLLATE NOCASE"
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
            "SELECT * FROM households WHERE token_hash = ?1 AND archived_at IS NULL",
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
    let name = input.name.clone().filter(|s| !s.trim().is_empty());
    let greeting = input.greeting.clone().filter(|s| !s.trim().is_empty());
    let (name, greeting) = match (name, greeting) {
        (Some(n), Some(g)) => (n, g),
        (Some(n), None) => (n.clone(), n),
        (None, Some(g)) => (g.clone(), g),
        (None, None) => {
            let derived = derive_greeting(&input.guests);
            if derived.is_empty() {
                bail!("a household needs a name, a greeting, or at least one guest");
            }
            (derived.clone(), derived)
        }
    };
    let ts = now();
    let events = input.events.clone().unwrap_or_else(|| default_events.to_vec());
    let tx = crate::db::begin(conn)?;
    tx.execute(
        "INSERT INTO households (name, greeting, events, created_at, updated_at) VALUES (?1, ?2, ?3, ?4, ?4)",
        params![name.trim(), greeting.trim(), serde_json::to_string(&events)?, ts],
    )?;
    let id = tx.last_insert_rowid();
    let mut rest = input.clone();
    rest.name = None;
    rest.greeting = None;
    rest.events = None;
    apply_household_fields(&tx, id, &rest)?;
    let token = tokens::household_token(secret, id, 1);
    tx.execute("UPDATE households SET token_hash = ?1 WHERE id = ?2", params![tokens::hash_token(&token), id])?;
    for (i, g) in input.guests.iter().enumerate() {
        let mut g = g.clone();
        g.sort.get_or_insert(i as i64);
        insert_guest(&tx, id, &g)?;
    }
    audit(&tx, actor, "household.create", "household", &id.to_string(), &serde_json::to_value(input)?)?;
    tx.commit()?;
    Ok(id)
}

fn derive_greeting(guests: &[GuestInput]) -> String {
    let names: Vec<&str> = guests.iter().filter_map(|g| g.first_name.as_deref()).collect();
    match names.as_slice() {
        [] => String::new(),
        [one] => one.to_string(),
        [init @ .., last] => format!("{} & {}", init.join(", "), last),
    }
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
// Guests

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Guest {
    pub id: i64,
    pub household_id: i64,
    pub first_name: String,
    pub last_name: String,
    pub email: String,
    pub is_child: bool,
    pub plus_one_allowed: bool,
    pub plus_one_of: Option<i64>,
    pub dietary: String,
    pub notes: String,
    pub sort: i64,
    /// Subset of the household's events this person is invited to; None = all of them.
    pub events: Option<Vec<String>>,
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
            plus_one_allowed: r.get("plus_one_allowed")?,
            plus_one_of: r.get("plus_one_of")?,
            dietary: r.get("dietary")?,
            notes: r.get("notes")?,
            sort: r.get("sort")?,
            events: r.get::<_, Option<String>>("events")?.and_then(|s| serde_json::from_str(&s).ok()),
            archived_at: r.get("archived_at")?,
        })
    }

    /// Is this person invited to `event`? Needs the household's invitation too.
    pub fn invited(&self, h: &Household, event: &str) -> bool {
        h.events.iter().any(|e| e == event) && self.events.as_ref().is_none_or(|own| own.iter().any(|e| e == event))
    }

    pub fn full_name(&self) -> String {
        format!("{} {}", self.first_name, self.last_name).trim().to_string()
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GuestInput {
    pub first_name: Option<String>,
    pub last_name: Option<String>,
    pub email: Option<String>,
    pub is_child: Option<bool>,
    pub plus_one_allowed: Option<bool>,
    pub dietary: Option<String>,
    pub notes: Option<String>,
    pub sort: Option<i64>,
    /// Restrict to these household events (`[]` = none of them). JSON `null` clears the
    /// restriction (invited to whatever the household is); omitted = unchanged.
    #[serde(default, deserialize_with = "double_option", skip_serializing_if = "Option::is_none")]
    pub events: Option<Option<Vec<String>>>,
}

fn double_option<'de, D: serde::Deserializer<'de>, T: Deserialize<'de>>(d: D) -> Result<Option<Option<T>>, D::Error> {
    Option::<T>::deserialize(d).map(Some)
}

impl GuestInput {
    pub fn validate(&self) -> Result<()> {
        if let Some(e) = &self.email {
            if !e.is_empty() && !plausible_email(e) {
                bail!("'{e}' does not look like an email address");
            }
        }
        for v in [&self.first_name, &self.last_name, &self.dietary, &self.notes].into_iter().flatten() {
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
            ("plus_one_allowed", b(&self.plus_one_allowed)),
            ("dietary", s(&self.dietary)),
            ("notes", s(&self.notes)),
            ("sort", self.sort.map(V::Integer)),
            (
                "events",
                self.events.as_ref().map(|e| match e {
                    None => V::Null,
                    Some(list) => V::Text(serde_json::to_string(list).unwrap()),
                }),
            ),
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
    let Some(first) = first else { bail!("guest first_name is required") };
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
                bail!("first_name cannot be empty");
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
    tx.commit()?;
    Ok(id)
}

pub fn update_guest(conn: &Connection, actor: &str, id: i64, g: &GuestInput) -> Result<()> {
    must_guest(conn, id)?;
    g.validate()?;
    let tx = crate::db::begin(conn)?;
    let changed = apply_guest_fields(&tx, id, g)?;
    if !changed.is_empty() {
        audit(&tx, actor, "guest.update", "guest", &id.to_string(), &json!(changed))?;
    }
    tx.commit()?;
    Ok(())
}

pub fn move_guest(conn: &Connection, actor: &str, id: i64, household_id: i64) -> Result<()> {
    must_guest(conn, id)?;
    must_household(conn, household_id)?;
    let tx = crate::db::begin(conn)?;
    tx.execute("UPDATE guests SET household_id = ?1, updated_at = ?2 WHERE id = ?3", params![household_id, now(), id])?;
    audit(&tx, actor, "guest.move", "guest", &id.to_string(), &json!({"household_id": household_id}))?;
    tx.commit()?;
    Ok(())
}

pub fn archive_guest(conn: &Connection, actor: &str, id: i64) -> Result<()> {
    must_guest(conn, id)?;
    let tx = crate::db::begin(conn)?;
    tx.execute("UPDATE guests SET archived_at = ?1, updated_at = ?1 WHERE id = ?2", params![now(), id])?;
    audit(&tx, actor, "guest.archive", "guest", &id.to_string(), &json!({}))?;
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
        })
    }
    pub fn is_owner(&self) -> bool {
        self.role == "owner"
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

pub fn create_invite(conn: &Connection, actor: &str, role: &str, hours: i64) -> Result<String> {
    if !matches!(role, "helper" | "owner") {
        bail!("invite role must be helper or owner");
    }
    let code = tokens::random_secret(20);
    let exp = time::OffsetDateTime::now_utc() + time::Duration::hours(hours);
    let tx = crate::db::begin(conn)?;
    tx.execute(
        "INSERT INTO invites (code_hash, role, created_by, created_at, expires_at) VALUES (?1, ?2, ?3, ?4, ?5)",
        params![tokens::sha256_hex(&code), role, actor, now(), exp.format(&time::format_description::well_known::Rfc3339)?],
    )?;
    audit(&tx, actor, "invite.create", "invite", "", &json!({"role": role, "hours": hours}))?;
    tx.commit()?;
    Ok(code)
}

/// Returns the invite's role if the code is valid and unused.
pub fn check_invite(conn: &Connection, code: &str) -> Result<Option<String>> {
    let row: Option<(String, String, Option<String>)> = conn
        .query_row(
            "SELECT role, expires_at, used_at FROM invites WHERE code_hash = ?1",
            [tokens::sha256_hex(code.trim())],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()?;
    Ok(row.and_then(|(role, exp, used)| (used.is_none() && exp > now()).then_some(role)))
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
    fn household_lifecycle() {
        let db = mem();
        db.with(|c| {
            let input = HouseholdInput {
                guests: vec![
                    GuestInput { first_name: Some("Jane".into()), last_name: Some("Smith".into()), ..Default::default() },
                    GuestInput { first_name: Some("John".into()), plus_one_allowed: Some(false), ..Default::default() },
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
            Ok(())
        })
        .unwrap();
    }
}

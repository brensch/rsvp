//! RSVP form model, validation, and the durable submit path.
//!
//! Submit order (the "never lose an RSVP" path):
//!   1. validate
//!   2. append to the fsync'd journal          <- the guest's answer is now on disk
//!   3. one SQLite transaction (synchronous=FULL): submission row, current
//!      responses, household extras, audit, outbox notification
//!   4. only then show the guest "thank you"
//! If step 3 fails the guest sees an error, but the journal still has it and
//! `rsvp journal verify` / `replay` will surface and restore it.

use std::collections::{BTreeMap, HashMap};

use anyhow::{Context, Result};
use rusqlite::{Connection, params};
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::config::{Config, Meal};
use crate::journal::{Entry, Journal};
use crate::store::{self, Guest, Household, Response};

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct RsvpForm {
    pub guests: Vec<GuestAnswer>,
    #[serde(default)]
    pub plus_ones: Vec<PlusOneAnswer>,
    #[serde(default)]
    pub message: String,
    #[serde(default)]
    pub song: String,
    #[serde(default)]
    pub accommodation: String,
    #[serde(default)]
    pub transport: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GuestAnswer {
    pub guest_id: i64,
    pub name: String,
    pub events: BTreeMap<String, EventAnswer>,
    #[serde(default)]
    pub dietary: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EventAnswer {
    pub attending: bool,
    #[serde(default)]
    pub meal: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlusOneAnswer {
    pub host_id: i64,
    pub first_name: String,
    pub last_name: String,
    /// Meal per meal-event the host is attending.
    pub meals: BTreeMap<String, String>,
    #[serde(default)]
    pub dietary: String,
}

pub const ACCOMMODATION_OPTIONS: &[(&str, &str)] = &[
    ("", "Choose one"),
    ("sorted", "We've sorted our own"),
    ("need-help", "We'd love recommendations"),
    ("local", "We live locally"),
];

pub const TRANSPORT_OPTIONS: &[(&str, &str)] = &[("", "Choose one"), ("yes", "Yes please"), ("no", "No thanks")];

/// Meals offered to a guest.
pub fn meals_for<'a>(cfg: &'a Config, is_child: bool) -> Vec<&'a Meal> {
    let kids: Vec<&Meal> = cfg.meals.iter().filter(|m| m.kids).collect();
    let adults: Vec<&Meal> = cfg.meals.iter().filter(|m| !m.kids).collect();
    if is_child && !kids.is_empty() { kids.into_iter().chain(adults).collect() } else { adults }
}

/// Field names used by the HTML form. Kept here so parsing and rendering agree.
pub mod field {
    pub fn att(gid: i64, ev: &str) -> String {
        format!("att.{gid}.{ev}")
    }
    pub fn meal(gid: i64, ev: &str) -> String {
        format!("meal.{gid}.{ev}")
    }
    pub fn diet(gid: i64) -> String {
        format!("diet.{gid}")
    }
    pub fn p1_first(host: i64) -> String {
        format!("p1first.{host}")
    }
    pub fn p1_last(host: i64) -> String {
        format!("p1last.{host}")
    }
    pub fn p1_meal(host: i64, ev: &str) -> String {
        format!("p1meal.{host}.{ev}")
    }
    pub fn p1_diet(host: i64) -> String {
        format!("p1diet.{host}")
    }
}

/// Primary (non plus-one) guests answer for themselves.
pub fn primary_guests(guests: &[Guest]) -> Vec<&Guest> {
    guests.iter().filter(|g| g.plus_one_of.is_none()).collect()
}

/// Pre-fill the form from what's already stored.
pub fn current_values(guests: &[Guest], responses: &[Response], h: &Household) -> HashMap<String, String> {
    let mut v = HashMap::new();
    let by_key: HashMap<(i64, &str), &Response> = responses.iter().map(|r| ((r.guest_id, r.event_slug.as_str()), r)).collect();
    for g in guests {
        match g.plus_one_of {
            None => {
                for ev in &h.events {
                    if let Some(r) = by_key.get(&(g.id, ev.as_str())) {
                        v.insert(field::att(g.id, ev), if r.attending { "yes" } else { "no" }.into());
                        if !r.meal.is_empty() {
                            v.insert(field::meal(g.id, ev), r.meal.clone());
                        }
                    }
                }
                v.insert(field::diet(g.id), g.dietary.clone());
            }
            Some(host) => {
                v.insert(field::p1_first(host), g.first_name.clone());
                v.insert(field::p1_last(host), g.last_name.clone());
                v.insert(field::p1_diet(host), g.dietary.clone());
                for ev in &h.events {
                    if let Some(r) = by_key.get(&(g.id, ev.as_str())) {
                        if !r.meal.is_empty() {
                            v.insert(field::p1_meal(host, ev), r.meal.clone());
                        }
                    }
                }
            }
        }
    }
    v.insert("message".into(), h.rsvp_message.clone());
    v.insert("song".into(), h.rsvp_song.clone());
    v.insert("accommodation".into(), h.rsvp_accommodation.clone());
    v.insert("transport".into(), h.rsvp_transport.clone());
    v
}

fn clip(s: Option<&String>, max: usize) -> String {
    let s = s.map(|s| s.trim()).unwrap_or("");
    s.chars().filter(|c| !c.is_control() || *c == '\n').take(max).collect()
}

/// Parse and validate submitted values. Errors are keyed by field name ("" = general).
pub fn parse(
    cfg: &Config,
    h: &Household,
    guests: &[Guest],
    values: &HashMap<String, String>,
) -> Result<RsvpForm, BTreeMap<String, String>> {
    let mut errors = BTreeMap::new();
    let mut form = RsvpForm::default();
    let events: Vec<_> = h.events.iter().filter_map(|e| cfg.event(e)).collect();

    for g in primary_guests(guests) {
        let mut answers = BTreeMap::new();
        for ev in events.iter().filter(|e| e.dated() && g.invited(h, &e.slug)) {
            let key = field::att(g.id, &ev.slug);
            let attending = match values.get(&key).map(String::as_str) {
                Some("yes") => true,
                Some("no") => false,
                _ => {
                    errors.insert(key, format!("Let us know if {} can make the {}.", g.first_name, ev.name.to_lowercase()));
                    continue;
                }
            };
            let mut meal = String::new();
            let offered = meals_for(cfg, g.is_child);
            if attending && ev.meal && !offered.is_empty() {
                let mk = field::meal(g.id, &ev.slug);
                meal = values.get(&mk).cloned().unwrap_or_default();
                if !offered.iter().any(|m| m.slug == meal) {
                    errors.insert(mk, format!("Choose a meal for {}.", g.first_name));
                }
            }
            answers.insert(ev.slug.clone(), EventAnswer { attending, meal });
        }
        let dietary = clip(values.get(&field::diet(g.id)), 500);

        if g.plus_one_allowed {
            let first = clip(values.get(&field::p1_first(g.id)), 80);
            let last = clip(values.get(&field::p1_last(g.id)), 80);
            let host_attending: Vec<_> = answers.iter().filter(|(_, a)| a.attending).map(|(k, _)| k.clone()).collect();
            if !first.is_empty() || !last.is_empty() {
                if first.is_empty() {
                    errors.insert(field::p1_first(g.id), "Your guest's first name, please.".into());
                }
                if host_attending.is_empty() {
                    errors.insert(field::p1_first(g.id), format!("{} isn't attending, so a plus-one can't either.", g.first_name));
                }
                let mut meals = BTreeMap::new();
                for slug in &host_attending {
                    let ev = cfg.event(slug).unwrap();
                    let offered = meals_for(cfg, false);
                    if ev.meal && !offered.is_empty() {
                        let mk = field::p1_meal(g.id, slug);
                        let m = values.get(&mk).cloned().unwrap_or_default();
                        if !offered.iter().any(|o| o.slug == m) {
                            errors.insert(mk, "Choose a meal for your guest.".into());
                        }
                        meals.insert(slug.clone(), m);
                    }
                }
                form.plus_ones.push(PlusOneAnswer {
                    host_id: g.id,
                    first_name: first,
                    last_name: last,
                    meals,
                    dietary: clip(values.get(&field::p1_diet(g.id)), 500),
                });
            }
        }
        form.guests.push(GuestAnswer { guest_id: g.id, name: g.full_name(), events: answers, dietary });
    }

    form.message = clip(values.get("message"), 2000);
    form.song = clip(values.get("song"), 200);
    form.accommodation = clip(values.get("accommodation"), 40);
    form.transport = clip(values.get("transport"), 40);
    if !ACCOMMODATION_OPTIONS.iter().any(|(k, _)| *k == form.accommodation) {
        form.accommodation.clear();
    }
    if !TRANSPORT_OPTIONS.iter().any(|(k, _)| *k == form.transport) {
        form.transport.clear();
    }

    if errors.is_empty() { Ok(form) } else { Err(errors) }
}

/// The durable submit path. Returns the submission id.
pub fn submit(
    conn: &Connection,
    journal: &Journal,
    cfg: &Config,
    h: &Household,
    form: &RsvpForm,
    ip_hash: &str,
    user_agent: &str,
) -> Result<String> {
    let entry = Entry {
        id: uuid::Uuid::now_v7().to_string(),
        kind: String::new(),
        household_id: h.id,
        received_at: crate::db::now(),
        ip_hash: ip_hash.to_string(),
        user_agent: user_agent.chars().take(300).collect(),
        payload: serde_json::to_value(form)?,
    };
    journal.append(&entry).context("writing RSVP journal")?;
    apply(conn, cfg, &entry, true).context("saving RSVP to database")?;
    Ok(entry.id)
}

/// Record a submission and update current state. Idempotent on submission id.
/// `notify` queues the Discord message (off during journal replay).
pub fn apply(conn: &Connection, cfg: &Config, entry: &Entry, notify: bool) -> Result<bool> {
    apply_entry(conn, Some(cfg), entry, notify)
}

/// Apply any journal entry (RSVP or address). `cfg` is only needed for RSVPs.
pub fn apply_entry(conn: &Connection, cfg: Option<&Config>, entry: &Entry, notify: bool) -> Result<bool> {
    if store::submission_exists(conn, &entry.id)? {
        return Ok(false);
    }
    let h = store::must_household(conn, entry.household_id)?;
    let tx = crate::db::begin(conn)?;
    let kind = if entry.kind.is_empty() { "rsvp" } else { entry.kind.as_str() };
    tx.execute(
        "INSERT INTO submissions (id, household_id, received_at, payload, ip_hash, user_agent, kind) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![entry.id, entry.household_id, entry.received_at, entry.payload.to_string(), entry.ip_hash, entry.user_agent, kind],
    )?;
    if kind == "address" {
        crate::address::apply(&tx, &h, entry, notify)?;
        tx.commit()?;
        return Ok(true);
    }
    let cfg = cfg.context("config needed to apply an RSVP")?;
    let form: RsvpForm = serde_json::from_value(entry.payload.clone()).context("journal payload")?;

    // Only move "current state" forward; an older submission replayed late is recorded but not applied.
    let is_latest = h.rsvp_at.as_deref().is_none_or(|prev| entry.received_at.as_str() >= prev);
    if is_latest {
        let ts = &entry.received_at;
        let upsert = |gid: i64, ev: &str, attending: bool, meal: &str| -> Result<()> {
            tx.execute(
                "INSERT INTO responses (guest_id, event_slug, attending, meal, submission_id, updated_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6)
                 ON CONFLICT(guest_id, event_slug) DO UPDATE SET attending = excluded.attending, meal = excluded.meal,
                 submission_id = excluded.submission_id, updated_at = excluded.updated_at",
                params![gid, ev, attending, meal, entry.id, ts],
            )?;
            Ok(())
        };
        for g in &form.guests {
            for (ev, a) in &g.events {
                upsert(g.guest_id, ev, a.attending, if a.attending { &a.meal } else { "" })?;
            }
            tx.execute("UPDATE guests SET dietary = ?1, updated_at = ?2 WHERE id = ?3", params![g.dietary, ts, g.guest_id])?;
        }

        // Plus-ones: one guest row per host, created/updated/archived to match the form.
        let hosts: Vec<i64> = store::guests_for(&tx, h.id)?.iter().filter(|g| g.plus_one_allowed).map(|g| g.id).collect();
        for host in hosts {
            let existing: Option<i64> = tx
                .query_row("SELECT id FROM guests WHERE plus_one_of = ?1 AND archived_at IS NULL", [host], |r| r.get(0))
                .ok();
            let host_answer = form.guests.iter().find(|g| g.guest_id == host);
            match (form.plus_ones.iter().find(|p| p.host_id == host), existing) {
                (Some(p), existing) => {
                    let pid = match existing {
                        Some(pid) => {
                            tx.execute(
                                "UPDATE guests SET first_name = ?1, last_name = ?2, dietary = ?3, updated_at = ?4 WHERE id = ?5",
                                params![p.first_name, p.last_name, p.dietary, ts, pid],
                            )?;
                            pid
                        }
                        None => {
                            tx.execute(
                                "INSERT INTO guests (household_id, first_name, last_name, dietary, plus_one_of, sort, created_at, updated_at)
                                 VALUES (?1, ?2, ?3, ?4, ?5, 1000, ?6, ?6)",
                                params![h.id, p.first_name, p.last_name, p.dietary, host, ts],
                            )?;
                            tx.last_insert_rowid()
                        }
                    };
                    for ev in &h.events {
                        let attending = host_answer.and_then(|a| a.events.get(ev)).is_some_and(|a| a.attending);
                        let meal = p.meals.get(ev).cloned().unwrap_or_default();
                        upsert(pid, ev, attending, &meal)?;
                    }
                }
                (None, Some(pid)) => {
                    tx.execute("UPDATE guests SET archived_at = ?1, updated_at = ?1 WHERE id = ?2", params![ts, pid])?;
                }
                (None, None) => {}
            }
        }

        tx.execute(
            "UPDATE households SET rsvp_at = ?1, rsvp_submission = ?2, rsvp_message = ?3, rsvp_song = ?4,
             rsvp_accommodation = ?5, rsvp_transport = ?6, updated_at = ?1 WHERE id = ?7",
            params![ts, entry.id, form.message, form.song, form.accommodation, form.transport, h.id],
        )?;
    }

    store::audit(&tx, &format!("guest:{}", h.id), "rsvp.submit", "household", &h.id.to_string(), &json!({"submission": entry.id, "applied": is_latest}))?;
    if notify {
        let first = h.rsvp_at.is_none();
        store::enqueue(&tx, "discord", &json!({"type": "rsvp", "text": summary(cfg, &h, &form, first)}))?;
    }
    tx.commit()?;
    Ok(true)
}

/// Human-readable summary (Discord + admin).
pub fn summary(cfg: &Config, h: &Household, form: &RsvpForm, first: bool) -> String {
    let mut out = format!("**{}** {} their RSVP\n", h.greeting, if first { "sent" } else { "updated" });
    let meal_name = |s: &str| cfg.meal(s).map(|m| m.name.clone()).unwrap_or_else(|| s.to_string());
    for g in &form.guests {
        let parts: Vec<String> = g
            .events
            .iter()
            .map(|(ev, a)| {
                let name = cfg.event(ev).map(|e| e.name.as_str()).unwrap_or(ev);
                if a.attending {
                    if a.meal.is_empty() { format!("✅ {name}") } else { format!("✅ {name} ({})", meal_name(&a.meal)) }
                } else {
                    format!("❌ {name}")
                }
            })
            .collect();
        out.push_str(&format!("• {}: {}\n", g.name, parts.join(" · ")));
        if !g.dietary.is_empty() {
            out.push_str(&format!("  ↳ dietary: {}\n", g.dietary));
        }
    }
    for p in &form.plus_ones {
        let meals: Vec<String> = p.meals.values().map(|m| meal_name(m)).collect();
        out.push_str(&format!("• ➕ {} {} {}\n", p.first_name, p.last_name, if meals.is_empty() { String::new() } else { format!("({})", meals.join(", ")) }));
    }
    if !form.song.is_empty() {
        out.push_str(&format!("🎵 {}\n", form.song));
    }
    if !form.accommodation.is_empty() {
        out.push_str(&format!("🏨 {}\n", form.accommodation));
    }
    if !form.transport.is_empty() {
        out.push_str(&format!("🚌 shuttle: {}\n", form.transport));
    }
    if !form.message.is_empty() {
        out.push_str(&format!("💌 “{}”\n", form.message));
    }
    out
}

/// Every journal entry that is missing from the database.
pub fn journal_missing(conn: &Connection, journal: &Journal) -> Result<Vec<Entry>> {
    let mut out = vec![];
    for e in journal.read_all()? {
        if !store::submission_exists(conn, &e.id)? {
            out.push(e);
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::{GuestInput, HouseholdInput};

    fn cfg() -> Config {
        let raw = include_str!("../config/wedding.example.toml");
        toml::from_str(raw).unwrap()
    }

    #[test]
    fn submit_journal_replay_and_plus_ones() {
        let cfg = cfg();
        let db = crate::store::tests::mem();
        let dir = std::env::temp_dir().join(format!("rsvp-j-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let journal = Journal::new(dir.join("journal.jsonl"));
        db.with(|c| {
            let input = HouseholdInput {
                guests: vec![
                    GuestInput { first_name: Some("Jane".into()), plus_one_allowed: Some(true), ..Default::default() },
                    GuestInput { first_name: Some("Tom".into()), is_child: Some(true), ..Default::default() },
                ],
                events: Some(vec!["ceremony".into(), "reception".into()]),
                ..Default::default()
            };
            let hid = store::create_household(c, "t", &[3; 32], &input, &[])?;
            let h = store::must_household(c, hid)?;
            let gs = store::guests_for(c, hid)?;
            let (jane, tom) = (gs[0].id, gs[1].id);

            // Missing answers are reported, not silently accepted.
            let empty = HashMap::new();
            let errs = parse(&cfg, &h, &gs, &empty).unwrap_err();
            assert_eq!(errs.len(), 4);

            let mut v: HashMap<String, String> = HashMap::new();
            for g in [jane, tom] {
                v.insert(field::att(g, "ceremony"), "yes".into());
                v.insert(field::att(g, "reception"), "yes".into());
            }
            v.insert(field::meal(jane, "reception"), "fish".into());
            v.insert(field::meal(tom, "reception"), "kids".into());
            v.insert(field::p1_first(jane), "Pat".into());
            v.insert(field::p1_meal(jane, "reception"), "beef".into());
            v.insert("message".into(), "Can't wait!".into());
            let form = parse(&cfg, &h, &gs, &v).expect("valid form");
            let id = submit(c, &journal, &cfg, &h, &form, "ip", "ua")?;

            let gs = store::guests_for(c, hid)?;
            assert_eq!(gs.len(), 3, "plus-one created");
            let pat = gs.iter().find(|g| g.plus_one_of == Some(jane)).unwrap();
            let resp = store::responses_for(c, hid)?;
            assert!(resp.iter().any(|r| r.guest_id == pat.id && r.event_slug == "reception" && r.attending && r.meal == "beef"));
            assert_eq!(store::must_household(c, hid)?.rsvp_message, "Can't wait!");
            assert!(c.execute("UPDATE submissions SET payload = '{}'", []).is_err(), "submissions are append-only");

            // Journal has it; replay is a no-op.
            let entries = journal.read_all()?;
            assert_eq!(entries.len(), 1);
            assert_eq!(entries[0].id, id);
            assert!(journal_missing(c, &journal)?.is_empty());
            assert!(!apply(c, &cfg, &entries[0], false)?);

            // Second submission: Jane declines the reception, drops the plus-one.
            let h = store::must_household(c, hid)?;
            let gs = store::guests_for(c, hid)?;
            v.insert(field::att(jane, "reception"), "no".into());
            v.remove(&field::p1_first(jane));
            let form = parse(&cfg, &h, &gs, &v).expect("valid");
            submit(c, &journal, &cfg, &h, &form, "ip", "ua")?;
            let gs = store::guests_for(c, hid)?;
            assert_eq!(gs.len(), 2, "plus-one archived");
            let resp = store::responses_for(c, hid)?;
            assert!(resp.iter().any(|r| r.guest_id == jane && r.event_slug == "reception" && !r.attending && r.meal.is_empty()));

            // Discord notifications were queued in the same transactions.
            let n: i64 = c.query_row("SELECT COUNT(*) FROM outbox WHERE kind = 'discord'", [], |r| r.get(0))?;
            assert_eq!(n, 2);
            Ok(())
        })
        .unwrap();
    }

    #[test]
    fn replay_restores_lost_rows() {
        let cfg = cfg();
        let db = crate::store::tests::mem();
        let dir = std::env::temp_dir().join(format!("rsvp-j-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let journal = Journal::new(dir.join("journal.jsonl"));
        db.with(|c| {
            let input = HouseholdInput { guests: vec![GuestInput { first_name: Some("Ann".into()), ..Default::default() }], events: Some(vec!["ceremony".into()]), ..Default::default() };
            let hid = store::create_household(c, "t", &[3; 32], &input, &[])?;
            let g = store::guests_for(c, hid)?[0].id;
            // Simulate a crash after the journal write but before the DB commit.
            let form = RsvpForm {
                guests: vec![GuestAnswer { guest_id: g, name: "Ann".into(), events: [("ceremony".to_string(), EventAnswer { attending: true, meal: String::new() })].into(), dietary: String::new() }],
                ..Default::default()
            };
            journal.append(&Entry { id: "lost-1".into(), kind: String::new(), household_id: hid, received_at: crate::db::now(), ip_hash: String::new(), user_agent: String::new(), payload: serde_json::to_value(&form)? })?;
            assert_eq!(journal_missing(c, &journal)?.len(), 1);
            for e in journal_missing(c, &journal)? {
                apply(c, &cfg, &e, false)?;
            }
            assert!(journal_missing(c, &journal)?.is_empty());
            assert!(store::responses_for(c, hid)?.iter().any(|r| r.attending));
            Ok(())
        })
        .unwrap();
    }
}

#[cfg(test)]
mod exclusion_tests {
    use super::*;
    use crate::store::{GuestInput, HouseholdInput};

    #[test]
    fn excluded_people_are_not_asked() {
        let cfg: Config = toml::from_str(include_str!("../config/wedding.example.toml")).unwrap();
        let db = crate::store::tests::mem();
        db.with(|c| {
            let kid: GuestInput = serde_json::from_str(r#"{"first_name":"Kid","is_child":true,"events":[]}"#)?;
            let input = HouseholdInput {
                guests: vec![GuestInput { first_name: Some("Mum".into()), ..Default::default() }, kid],
                events: Some(vec!["ceremony".into(), "reception".into()]),
                ..Default::default()
            };
            let hid = store::create_household(c, "t", &[3; 32], &input, &[])?;
            let h = store::must_household(c, hid)?;
            let gs = store::guests_for(c, hid)?;
            let errs = parse(&cfg, &h, &gs, &HashMap::new()).unwrap_err();
            assert_eq!(errs.len(), 2, "only Mum's two events are asked: {errs:?}");
            // Clearing the override with null re-invites the kid.
            let clear: GuestInput = serde_json::from_str(r#"{"events":null}"#)?;
            store::update_guest(c, "t", gs[1].id, &clear)?;
            let gs = store::guests_for(c, hid)?;
            assert_eq!(parse(&cfg, &h, &gs, &HashMap::new()).unwrap_err().len(), 4);
            Ok(())
        })
        .unwrap();
    }
}

#[cfg(test)]
mod address_tests {
    use super::*;
    use crate::address::{Address, Submission};
    use crate::store::{GuestInput, HouseholdInput};

    #[test]
    fn address_goes_through_journal_and_replays() {
        let db = crate::store::tests::mem();
        let dir = std::env::temp_dir().join(format!("rsvp-a-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let journal = Journal::new(dir.join("journal.jsonl"));
        db.with(|c| {
            let input = HouseholdInput { guests: vec![GuestInput { first_name: Some("Ann".into()), ..Default::default() }], ..Default::default() };
            let hid = store::create_household(c, "t", &[3; 32], &input, &["ceremony".into()])?;
            let h = store::must_household(c, hid)?;
            let a = Address { addr1: "1 Test St".into(), city: "Springfield".into(), region: "IL".into(), postcode: "62701".into(), country: "United States".into(), ..Default::default() };
            let sub = Submission { address: a, status: "verified".into() };
            crate::address::submit(c, &journal, &h, &sub, "ip", "ua")?;
            let h = store::must_household(c, hid)?;
            assert_eq!(h.addr1, "1 Test St");
            assert_eq!(h.address_status, "verified");
            assert!(h.rsvp_at.is_none(), "an address is not an RSVP");
            assert!(journal_missing(c, &journal)?.is_empty());
            let e = &journal.read_all()?[0];
            assert_eq!(e.kind, "address");
            assert!(!apply_entry(c, None, e, false)?, "replay is a no-op");
            Ok(())
        })
        .unwrap();
    }
}

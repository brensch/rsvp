//! Admin portal. Every approved team member edits directly; owners also manage
//! sending, the team and settings. All changes are recorded in the activity log.

use std::collections::HashMap;

use anyhow::anyhow;
use axum::extract::{Path, Query, State};
use axum::http::header;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Form, Router};
use maud::{Markup, PreEscaped, html};
use serde::Deserialize;
use serde_json::json;

use super::auth::{Member, Session};
use super::ui::{self, Nav, csrf_input, when, when_opt};
use super::{AppResult, AppState, redirect};
use crate::assets::{self, Kind, Layout};
use crate::config::Config;
use crate::store::{self, Guest, GuestInput, Household, HouseholdInput, Phase, Response as Resp, Status};

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/admin", get(dashboard))
        .route("/admin/households", get(households))
        .route("/admin/households/new", get(new_household).post(create_household))
        .route("/admin/households/{id}", get(household).post(update_household))
        .route("/admin/households/{id}/guests", post(add_guest))
        .route("/admin/households/{id}/quick", post(quick_update))
        .route("/admin/households/{id}/archive", post(archive_household))
        .route("/admin/households/{id}/rotate", post(rotate_link))
        .route("/admin/households/{id}/send", post(send_one))
        .route("/admin/guests", get(guests))
        .route("/admin/guests/{id}", post(update_guest))
        .route("/admin/guests/{id}/archive", post(archive_guest))
        .route("/admin/stationery", get(stationery))
        .route("/admin/stationery/{file}", get(stationery_file))
        .route("/admin/send", get(send_page).post(send_bulk))
        .route("/admin/send/test", post(send_test))
        .route("/admin/users", get(users).post(user_role))
        .route("/admin/users/invite", post(create_invite))
        .route("/admin/settings", get(settings).post(update_settings))
        .route("/admin/account", get(account))
        .route("/admin/account/signout-all", post(signout_all))
        .route("/admin/audit", get(audit))
        .route("/admin/export/guests.csv", get(export_csv))
}

// ---------------------------------------------------------------------------
// Helpers

#[derive(Deserialize, Default)]
struct Flash {
    m: Option<String>,
}

fn flash_text(code: Option<&str>) -> Option<&'static str> {
    Some(match code? {
        "saved" => "Saved.",
        "created" => "Created.",
        "archived" => "Archived.",
        "restored" => "Restored.",
        "queued" => "Queued. Emails go out within a minute. Progress is shown below.",
        "rotated" => "New link generated. The old link and QR code no longer work.",
        "role" => "Role updated.",
        "passkey" => "Passkey added.",
        "backup" => "Backup taken.",
        "phase" => "Phase updated.",
        "removed" => "Removed.",
        "replayed" => "Journal replayed.",
        "tested" => "Test notification queued.",
        "signedout" => "Other sessions signed out.",
        "noemail" => "That household has no email address.",
        "error" => "Something about that didn't work. Check the values and try again.",
        _ => return None,
    })
}

async fn page(st: &AppState, s: &Session, active: &str, title: &str, flash: &Flash, body: Markup) -> AppResult {
    let pending_users = st
        .db
        .call(|c| Ok(c.query_row("SELECT COUNT(*) FROM users WHERE role = 'pending'", [], |r| r.get::<_, i64>(0))?))
        .await?;
    let nav = Nav { user: s.user.clone(), csrf: s.csrf.clone(), pending_users };
    Ok(ui::admin_page(&st.cfg, &nav, active, title, flash_text(flash.m.as_deref()), body))
}

/// Submitted form fields.
struct F(HashMap<String, String>, Vec<(String, String)>);

impl F {
    fn new(pairs: Vec<(String, String)>) -> Self {
        Self(pairs.iter().cloned().collect(), pairs)
    }
    fn get(&self, k: &str) -> Option<String> {
        self.0.get(k).map(|v| v.trim().to_string())
    }
    fn s(&self, k: &str) -> String {
        self.get(k).unwrap_or_default()
    }
    fn check(&self, k: &str) -> bool {
        self.0.get(k).is_some_and(|v| v == "on" || v == "1" || v == "true")
    }
    fn all(&self, k: &str) -> Vec<String> {
        self.1.iter().filter(|(key, _)| key == k).map(|(_, v)| v.clone()).collect()
    }
    fn csrf(&self, s: &Session) -> Result<(), Response> {
        s.check_csrf(self.0.get("_csrf").map(String::as_str))
    }
}

macro_rules! guard {
    ($e:expr) => {
        if let Err(r) = $e {
            return Ok(r);
        }
    };
}

fn event_slugs(cfg: &Config) -> Vec<String> {
    cfg.events.iter().map(|e| e.slug.clone()).collect()
}

fn default_events(cfg: &Config) -> Vec<String> {
    cfg.events.iter().filter(|e| e.default_invite).map(|e| e.slug.clone()).collect()
}

fn status_badge(s: Status) -> Markup {
    let class = match s {
        Status::NoReply => "badge muted",
        Status::Attending => "badge good",
        Status::Partial => "badge warn",
        Status::Declined => "badge bad",
    };
    html! { span class=(class) { (s.label()) } }
}

fn household_input_from(f: &F, cfg: &Config) -> HouseholdInput {
    HouseholdInput {
        name: f.get("name"),
        greeting: f.get("greeting"),
        envelope: f.get("envelope"),
        email: f.get("email"),
        addr1: f.get("addr1"),
        addr2: f.get("addr2"),
        city: f.get("city"),
        region: f.get("region"),
        postcode: f.get("postcode"),
        country: f.get("country"),
        side: f.get("side"),
        tier: f.get("tier").and_then(|t| t.parse().ok()),
        australia: f.check("australia_present").then(|| f.check("australia")),
        tags: f.get("tags"),
        events: Some(f.all("events").into_iter().filter(|e| cfg.event(e).is_some()).collect()),
        notes: f.get("notes"),
        guests: vec![],
    }
}

/// `household_events`: the events this person could be invited to. Only read when the
/// form rendered per-person event checkboxes (marked by `{prefix}ev_present`).
fn guest_input_from(f: &F, prefix: &str, household_events: &[String]) -> GuestInput {
    let g = |k: &str| f.get(&format!("{prefix}{k}"));
    let events = f.check(&format!("{prefix}ev_present")).then(|| {
        let picked: Vec<String> = f.all(&format!("{prefix}ev")).into_iter().filter(|e| household_events.contains(e)).collect();
        // Everything ticked means "same as the household".
        if picked.len() == household_events.len() { None } else { Some(picked) }
    });
    GuestInput {
        events,
        first_name: g("first_name"),
        last_name: g("last_name"),
        email: g("email"),
        is_child: Some(f.check(&format!("{prefix}is_child"))),
        plus_one_allowed: Some(f.check(&format!("{prefix}plus_one_allowed"))),
        dietary: g("dietary"),
        notes: g("notes"),
        sort: None,
    }
}

/// Apply a household/guest change as the signed-in user. Returns (flash code, created id).
async fn apply_change(
    st: &AppState,
    s: &Session,
    kind: &'static str,
    target: Option<i64>,
    payload: serde_json::Value,
) -> anyhow::Result<(&'static str, Option<i64>)> {
    let user = s.user.clone();
    let (secret, defaults, valid) = (st.env.secret.clone(), default_events(&st.cfg), event_slugs(&st.cfg));
    st.db
        .call(move |c| {
            let actor = user.actor();
            let created = match kind {
                "household.create" => {
                    let input: HouseholdInput = serde_json::from_value(payload)?;
                    input.validate(&valid)?;
                    Some(store::create_household(c, &actor, &secret, &input, &defaults)?)
                }
                "household.update" => {
                    let input: HouseholdInput = serde_json::from_value(payload)?;
                    input.validate(&valid)?;
                    store::update_household(c, &actor, target.unwrap(), &input)?;
                    None
                }
                "guest.create" => Some(store::create_guest(c, &actor, target.unwrap(), &serde_json::from_value(payload)?)?),
                "guest.update" => {
                    store::update_guest(c, &actor, target.unwrap(), &serde_json::from_value(payload)?)?;
                    None
                }
                "guest.archive" => {
                    store::archive_guest(c, &actor, target.unwrap())?;
                    None
                }
                _ => return Err(anyhow!("unsupported change {kind}")),
            };
            Ok((if kind.ends_with("create") { "created" } else { "saved" }, created))
        })
        .await
}

struct Snapshot {
    households: Vec<Household>,
    guests: Vec<Guest>,
    responses: HashMap<(i64, String), Resp>,
}

impl Snapshot {
    async fn load(st: &AppState) -> anyhow::Result<Self> {
        st.db
            .call(|c| Ok(Snapshot { households: store::list_households(c, false)?, guests: store::all_guests(c)?, responses: store::all_responses(c)? }))
            .await
    }
    fn guests_of(&self, hid: i64) -> Vec<Guest> {
        self.guests.iter().filter(|g| g.household_id == hid).cloned().collect()
    }
    fn status(&self, h: &Household) -> Status {
        store::household_status(h, &self.guests_of(h.id), &self.responses)
    }
}

// ---------------------------------------------------------------------------
// Dashboard

async fn dashboard(State(st): State<AppState>, Member(s): Member, Query(fl): Query<Flash>) -> AppResult {
    let snap = Snapshot::load(&st).await?;
    let (phase, recent, outbox_failed, last_backup, missing_journal) = {
        let journal = st.journal.clone();
        st.db
            .call(move |c| {
                let failed: i64 = c.query_row("SELECT COUNT(*) FROM outbox WHERE failed_at IS NOT NULL", [], |r| r.get(0))?;
                let missing = crate::rsvp::journal_missing(c, &journal)?.len();
                Ok((store::phase(c)?, store::submissions(c, None, 8)?, failed, store::setting(c, "last_backup_at")?, missing))
            })
            .await?
    };
    let cfg = &st.cfg;
    let total = snap.households.len();
    let replied = snap.households.iter().filter(|h| h.rsvp_at.is_some()).count();
    let viewed_no_reply = snap.households.iter().filter(|h| h.first_viewed_at.is_some() && h.rsvp_at.is_none()).count();
    let people = snap.guests.iter().filter(|g| g.plus_one_of.is_none()).count();
    let no_email = snap.households.iter().filter(|h| h.email.is_empty() && snap.guests_of(h.id).iter().all(|g| g.email.is_empty())).count();
    let no_addr = snap.households.iter().filter(|h| h.address_lines().is_empty()).count();
    let pct = if total > 0 { replied * 100 / total } else { 0 };
    let names: HashMap<i64, &str> = snap.households.iter().map(|h| (h.id, h.name.as_str())).collect();
    let by_h: HashMap<i64, &Household> = snap.households.iter().map(|h| (h.id, h)).collect();

    let body = html! {
        @if missing_journal > 0 {
            div.alert.bad { strong { (missing_journal) " RSVP(s) are in the journal but missing from the database." } " Go to " a href="/admin/settings" { "Settings" } " and replay the journal." }
        }
        @if outbox_failed > 0 {
            div.alert.warn { (outbox_failed) " notification(s) or email(s) failed permanently. " a href="/admin/send" { "See details" } }
        }
        div.tiles {
            div.tile { span.tile-label { "Phase" } span.tile-value.small { (phase.label()) } span.tile-sub { "RSVP by " (cfg.wedding.rsvp_deadline) } }
            div.tile {
                span.tile-label { "Households" } span.tile-value { (total) } span.tile-sub { (people) " people" }
                span.tile-sub {
                    @for t in 1..=3 {
                        @let n: usize = snap.households.iter().filter(|h| h.tier == t).map(|h| snap.guests_of(h.id).len()).sum();
                        a href={ "/admin/households?tier=" (t) } { "T" (t) " " (n) } " "
                    }
                }
            }
            div.tile {
                span.tile-label { "Replied" } span.tile-value { (replied) small { " / " (total) } }
                div.meter { span style={ "width:" (pct) "%" } {} }
                span.tile-sub { (viewed_no_reply) " opened their link but haven't replied" }
            }
        }
        div.grid2 {
            section.card {
                h2 { "Headcount" }
                table.compact {
                    thead { tr { th { "Event" } th.num { "Yes" } th.num { "No" } th.num { "Waiting" } } }
                    tbody {
                        @for e in &cfg.events {
                            @let invited: Vec<&Guest> = snap.guests.iter().filter(|g| by_h.get(&g.household_id).is_some_and(|h| g.invited(h, &e.slug))).collect();
                            @let yes = invited.iter().filter(|g| snap.responses.get(&(g.id, e.slug.clone())).is_some_and(|r| r.attending)).count();
                            @let no = invited.iter().filter(|g| snap.responses.get(&(g.id, e.slug.clone())).is_some_and(|r| !r.attending)).count();
                            tr { td { (e.name) } td.num.good { (yes) } td.num { (no) } td.num.muted { (invited.len().saturating_sub(yes + no)) } }
                        }
                    }
                }
                @for e in cfg.events.iter().filter(|e| e.meal) {
                    h3 { "Meals · " (e.name) }
                    div.chips {
                        @for m in &cfg.meals {
                            @let n = snap.responses.values().filter(|r| r.event_slug == e.slug && r.attending && r.meal == m.slug).count();
                            span.chip { (m.name) " " strong { (n) } }
                        }
                    }
                }
            }
            section.card {
                h2 { "Latest replies" }
                @if recent.is_empty() { p.muted { "No replies yet." } }
                ul.feed {
                    @for r in &recent {
                        li {
                            a href={ "/admin/households/" (r.household_id) } { (names.get(&r.household_id).copied().unwrap_or("?")) }
                            span.muted { " · " (when(cfg, &r.received_at)) }
                            @if let Some(h) = by_h.get(&r.household_id) { " " (status_badge(snap.status(h))) }
                        }
                    }
                }
            }
        }
        section.card {
            h2 { "To do" }
            ul.todo {
                li { a href="/admin/households?status=no_reply" { (total - replied) " households haven't replied" } }
                li { a href="/admin/households?filter=no_email" { (no_email) " households have no email address" } }
                li { a href="/admin/households?filter=no_address" { (no_addr) " households have no postal address" } }
                li { "Last backup: " (when_opt(cfg, &last_backup)) }
            }
        }
    };
    page(&st, &s, "dash", "Overview", &fl, body).await
}

// ---------------------------------------------------------------------------
// Households

#[derive(Deserialize, Default)]
struct ListQuery {
    q: Option<String>,
    tier: Option<String>,
    status: Option<String>,
    side: Option<String>,
    filter: Option<String>,
    m: Option<String>,
}

fn side_short(cfg: &Config, side: &str) -> String {
    match side {
        "a" => cfg.couple.a_first.clone(),
        "b" => cfg.couple.b_first.clone(),
        _ => "Both".into(),
    }
}

/// Tier switch: three buttons, the current one pressed. Inline-saves with JS; plain POST without.
fn tier_toggle(csrf: &str, h: &Household, back: &str) -> Markup {
    html! {
        form.seg.inline-save method="post" action={ "/admin/households/" (h.id) "/quick" } data-inline {
            (csrf_input(csrf)) input type="hidden" name="back" value=(back);
            @for t in 1..=3 {
                button type="submit" name="tier" value=(t) aria-pressed=(if h.tier == t { "true" } else { "false" }) title={ "Tier " (t) } { (t) }
            }
        }
    }
}

/// "All correct" switch: someone confirmed the names and details.
fn checked_toggle(cfg: &Config, csrf: &str, h: &Household, back: &str) -> Markup {
    let title = match (&h.checked_by, &h.checked_at) {
        (Some(by), Some(at)) => format!("Checked by {by}, {}", when(cfg, at)),
        _ => "Mark as checked: names and details are all correct".to_string(),
    };
    html! {
        form.chips-toggle.inline-save method="post" action={ "/admin/households/" (h.id) "/quick" } data-inline {
            (csrf_input(csrf)) input type="hidden" name="back" value=(back);
            button.ok type="submit" name="toggle_checked" value="1" aria-pressed=(if h.checked_at.is_some() { "true" } else { "false" }) title=(title) { "Correct" }
        }
        span.checked-by.small.muted title=(title) { (h.checked_by.as_deref().filter(|_| h.checked_at.is_some()).unwrap_or("")) }
    }
}

/// "Lives in Australia" switch.
fn australia_toggle(csrf: &str, h: &Household, back: &str) -> Markup {
    html! {
        form.chips-toggle.inline-save method="post" action={ "/admin/households/" (h.id) "/quick" } data-inline {
            (csrf_input(csrf)) input type="hidden" name="back" value=(back);
            button.au type="submit" name="toggle_australia" value="1" aria-pressed=(if h.australia { "true" } else { "false" })
                title="Lives in Australia: sees the Australia travel info" { "AU" }
        }
    }
}

/// One chip per configured event; pressed = invited.
fn event_toggles(cfg: &Config, csrf: &str, h: &Household, back: &str) -> Markup {
    let replied = h.rsvp_at.is_some();
    html! {
        form.chips-toggle.inline-save method="post" action={ "/admin/households/" (h.id) "/quick" } data-inline {
            (csrf_input(csrf)) input type="hidden" name="back" value=(back);
            @for e in &cfg.events {
                @let on = h.events.contains(&e.slug);
                button type="submit" name="toggle_event" value=(e.slug) aria-pressed=(if on { "true" } else { "false" })
                    data-confirm=[replied.then(|| format!("{} already replied. {} the {}?", h.greeting, if on { "Remove them from" } else { "Invite them to" }, e.name.to_lowercase()))]
                    title=(e.name) { (e.short_name()) }
            }
        }
    }
}

async fn households(State(st): State<AppState>, Member(s): Member, Query(q): Query<ListQuery>) -> AppResult {
    let snap = Snapshot::load(&st).await?;
    let cfg = &st.cfg;
    let needle = q.q.clone().unwrap_or_default().to_lowercase();
    let tier_q: Option<i64> = q.tier.as_deref().and_then(|t| t.parse().ok());
    let rows: Vec<(&Household, Vec<Guest>, Status)> = snap
        .households
        .iter()
        .map(|h| (h, snap.guests_of(h.id), snap.status(h)))
        .filter(|(h, gs, status)| {
            let text_ok = needle.is_empty()
                || h.name.to_lowercase().contains(&needle)
                || h.greeting.to_lowercase().contains(&needle)
                || h.tags.to_lowercase().contains(&needle)
                || h.email.to_lowercase().contains(&needle)
                || gs.iter().any(|g| g.full_name().to_lowercase().contains(&needle));
            let status_ok = match q.status.as_deref() {
                Some("no_reply") => *status == Status::NoReply,
                Some("attending") => *status == Status::Attending,
                Some("declined") => *status == Status::Declined,
                Some("partial") => *status == Status::Partial,
                _ => true,
            };
            let side_ok = q.side.as_deref().is_none_or(|sd| sd.is_empty() || h.side == sd);
            let tier_ok = tier_q.is_none_or(|t| h.tier == t);
            let filter_ok = match q.filter.as_deref() {
                Some("no_email") => h.email.is_empty() && gs.iter().all(|g| g.email.is_empty()),
                Some("no_address") => h.address_lines().is_empty(),
                Some("no_events") => h.events.is_empty(),
                Some("australia") => h.australia,
                Some("unchecked") => h.checked_at.is_none(),
                Some("checked") => h.checked_at.is_some(),
                Some("not_australia") => !h.australia,
                Some("std_unsent") => h.std_sent_at.is_none(),
                Some("invite_unsent") => h.invite_sent_at.is_none(),
                _ => true,
            };
            text_ok && status_ok && side_ok && tier_ok && filter_ok
        })
        .collect();
    let people: usize = rows.iter().map(|(_, g, _)| g.len()).sum();
    let sel = |a: &Option<String>, v: &str| a.as_deref() == Some(v);
    // Keep the current filters when an inline form falls back to a full POST + redirect.
    let back = {
        let mut parts = vec![];
        for (k, v) in [("q", &q.q), ("tier", &q.tier), ("status", &q.status), ("side", &q.side), ("filter", &q.filter)] {
            if let Some(v) = v.as_deref().filter(|v| !v.is_empty()) {
                parts.push(format!("{k}={}", urlencoding::encode(v)));
            }
        }
        if parts.is_empty() { "/admin/households".to_string() } else { format!("/admin/households?{}", parts.join("&")) }
    };
    let tier_counts: Vec<(i64, usize, usize)> = (1..=3)
        .map(|t| {
            let hs: Vec<&Household> = snap.households.iter().filter(|h| h.tier == t).collect();
            let ppl = hs.iter().map(|h| snap.guests_of(h.id).len()).sum();
            (t, hs.len(), ppl)
        })
        .collect();
    let body = html! {
        div.toolbar {
            form.filters.autosubmit method="get" {
                input type="search" name="q" value=[q.q.as_deref()] placeholder="Search people, tags, email…" aria-label="Search";
                select name="tier" aria-label="Tier" {
                    option value="" { "All tiers" }
                    @for t in 1..=3 { option value=(t) selected[tier_q == Some(t)] { "Tier " (t) } }
                }
                select name="status" aria-label="Reply status" {
                    option value="" { "Any reply" }
                    option value="no_reply" selected[sel(&q.status, "no_reply")] { "No reply" }
                    option value="attending" selected[sel(&q.status, "attending")] { "Attending" }
                    option value="partial" selected[sel(&q.status, "partial")] { "Partly attending" }
                    option value="declined" selected[sel(&q.status, "declined")] { "Declined" }
                }
                select name="side" aria-label="Side" {
                    option value="" { "Either side" }
                    option value="a" selected[sel(&q.side, "a")] { (cfg.couple.a_first) }
                    option value="b" selected[sel(&q.side, "b")] { (cfg.couple.b_first) }
                    option value="both" selected[sel(&q.side, "both")] { "Both" }
                }
                select name="filter" aria-label="Other" {
                    option value="" { "Everything" }
                    option value="unchecked" selected[sel(&q.filter, "unchecked")] { "Not checked yet" }
                    option value="checked" selected[sel(&q.filter, "checked")] { "Checked correct" }
                    option value="australia" selected[sel(&q.filter, "australia")] { "Lives in Australia" }
                    option value="not_australia" selected[sel(&q.filter, "not_australia")] { "Lives outside Australia" }
                    option value="no_events" selected[sel(&q.filter, "no_events")] { "Not invited to anything" }
                    option value="no_email" selected[sel(&q.filter, "no_email")] { "Missing email" }
                    option value="no_address" selected[sel(&q.filter, "no_address")] { "Missing address" }
                    option value="std_unsent" selected[sel(&q.filter, "std_unsent")] { "Save the date not sent" }
                    option value="invite_unsent" selected[sel(&q.filter, "invite_unsent")] { "Invitation not sent" }
                }
                noscript { button.button type="submit" { "Filter" } }
            }
            a.button.primary href="/admin/households/new" { "New household" }
        }
        div.summary-bar {
            span.summary-total { strong { (rows.len()) } " households · " strong { (people) } " people" }
            @for (t, n, ppl) in &tier_counts {
                a.tier-pill.active[tier_q == Some(*t)] href={ "/admin/households?tier=" (t) } { "Tier " (t) " " strong { (ppl) } span.muted { " people · " (n) " hh" } }
            }
            @let checked = snap.households.iter().filter(|h| h.checked_at.is_some()).count();
            a.tier-pill.active[sel(&q.filter, "unchecked")] href="/admin/households?filter=unchecked" { "Checked " strong { (checked) } span.muted { " / " (snap.households.len()) } }
            @let au = snap.households.iter().filter(|h| h.australia).count();
            a.tier-pill.active[sel(&q.filter, "australia")] href="/admin/households?filter=australia" { "AU " strong { (au) } span.muted { " hh" } }
        }
        div.table-wrap {
            table.list.households {
                thead { tr {
                    th { "Household" }
                    th.num { "#" }
                    th { "Tier" }
                    th title="Lives in Australia" { "AU" }
                    th { "Invited to" }
                    th title="Names and details checked" { "Checked" }
                    th title="Reply status · S = save the date sent, I = invitation sent" { "Reply · sent" }
                } }
                tbody {
                    @for (h, gs, status) in &rows {
                        tr id={ "h" (h.id) } {
                            td.hh {
                                div.hh-head {
                                    a.hh-name href={ "/admin/households/" (h.id) } { (h.name) }
                                    span class={ "side side-" (h.side) } { (side_short(cfg, &h.side)) }
                                    @for t in h.tag_list() { span.tag { (t) } }
                                }
                                @let solo = gs.len() == 1 && gs[0].full_name() == h.name && !gs[0].plus_one_allowed && gs[0].events.is_none();
                                @if !solo { div.hh-people {
                                    @for (i, g) in gs.iter().enumerate() {
                                        @if i > 0 { span.sep { " · " } }
                                        span.person.muted[g.plus_one_of.is_some()] {
                                            (g.full_name())
                                            @if g.last_name.trim().is_empty() && g.plus_one_of.is_none() { small.warn-text { " no surname" } }
                                            @if g.is_child { small { " child" } }
                                            @if g.plus_one_allowed { small { " +1" } }
                                            @if g.events.is_some() { small title="Invited to fewer events than the household" { " ◐" } }
                                        }
                                    }
                                    @if gs.is_empty() { span.muted { "No people yet" } }
                                } }
                            }
                            td.num { (gs.len()) }
                            td { (tier_toggle(&s.csrf, h, &back)) }
                            td { (australia_toggle(&s.csrf, h, &back)) }
                            td.events-cell { (event_toggles(cfg, &s.csrf, h, &back)) }
                            td { (checked_toggle(cfg, &s.csrf, h, &back)) }
                            td.reply {
                                (status_badge(*status))
                                div.reply-meta {
                                    span title="Save the date sent" class=@if h.std_sent_at.is_some() { "dot on" } @else { "dot" } { "S" }
                                    span title="Invitation sent" class=@if h.invite_sent_at.is_some() { "dot on" } @else { "dot" } { "I" }
                                    @if let Some(v) = &h.first_viewed_at { span.small.muted { " opened " (when(cfg, v).split(',').next().unwrap_or("")) } }
                                }
                            }
                        }
                    }
                    @if rows.is_empty() { tr { td colspan="7" { p.muted { "Nothing matches those filters." } } } }
                }
            }
        }
    };
    page(&st, &s, "households", "Households", &Flash { m: q.m }, body).await
}

/// Inline edits from the households table: tier or one event toggle.
async fn quick_update(
    State(st): State<AppState>,
    Member(s): Member,
    Path(id): Path<i64>,
    headers: axum::http::HeaderMap,
    Form(pairs): Form<Vec<(String, String)>>,
) -> AppResult {
    let f = F::new(pairs);
    guard!(f.csrf(&s));
    let back = safe_back(&f.s("back"), "/admin/households");
    let h = st.db.call(move |c| store::must_household(c, id)).await?;
    let inline = headers.get("x-inline").is_some();
    if f.get("toggle_checked").is_some() {
        let (actor, who, now_checked) = (s.user.actor(), s.user.name.clone(), h.checked_at.is_none());
        st.db.call(move |c| store::set_checked(c, &actor, &who, id, now_checked)).await?;
        return Ok(if inline {
            axum::Json(json!({"checked": now_checked, "checked_by": if now_checked { s.user.name.clone() } else { String::new() }, "tier": h.tier, "events": h.events, "australia": h.australia})).into_response()
        } else {
            redirect(&format!("{back}#h{id}"))
        });
    }
    let mut input = HouseholdInput::default();
    if let Some(t) = f.get("tier").and_then(|t| t.parse::<i64>().ok()) {
        input.tier = Some(t);
    }
    if f.get("toggle_australia").is_some() {
        input.australia = Some(!h.australia);
    }
    if let Some(ev) = f.get("toggle_event").filter(|e| st.cfg.event(e).is_some()) {
        let mut events = h.events.clone();
        if let Some(pos) = events.iter().position(|e| *e == ev) {
            events.remove(pos);
        } else {
            events.push(ev);
        }
        // Keep config order.
        events.sort_by_key(|e| st.cfg.events.iter().position(|x| &x.slug == e));
        input.events = Some(events);
    }
    if input.tier.is_none() && input.events.is_none() && input.australia.is_none() || input.validate(&event_slugs(&st.cfg)).is_err() {
        return Ok(if inline { (axum::http::StatusCode::BAD_REQUEST, axum::Json(json!({"error": "invalid change"}))).into_response() } else { redirect(&format!("{back}#h{id}")) });
    }
    let (code, _) = apply_change(&st, &s, "household.update", Some(id), serde_json::to_value(&input)?).await?;
    if inline {
        let h = st.db.call(move |c| store::must_household(c, id)).await?;
        return Ok(axum::Json(json!({"tier": h.tier, "events": h.events, "australia": h.australia, "checked": h.checked_at.is_some()})).into_response());
    }
    let sep = if back.contains('?') { '&' } else { '?' };
    Ok(redirect(&format!("{back}{sep}m={code}#h{id}")))
}

fn household_fields(cfg: &Config, h: Option<&Household>) -> Markup {
    let v = |f: fn(&Household) -> &str| h.map(f).unwrap_or("").to_string();
    let events: Vec<String> = h.map(|h| h.events.clone()).unwrap_or_else(|| default_events(cfg));
    let side = h.map(|h| h.side.as_str()).unwrap_or("both");
    html! {
        div.form-grid {
            label.field { span { "Internal name" } input name="name" value=(v(|h| &h.name)) placeholder="Smith family" maxlength="200"; }
            label.field { span { "Greeting " small { "(shown to guests: “Dear …”)" } } input name="greeting" value=(v(|h| &h.greeting)) placeholder="Jane & John" maxlength="200"; }
            label.field.span2 { span { "Envelope name" } input name="envelope" value=(v(|h| &h.envelope)) placeholder="Mr & Mrs J. Smith" maxlength="200"; }
            label.field { span { "Email" } input type="email" name="email" value=(v(|h| &h.email)) maxlength="200"; }
            label.field.span2 {
                span {
                    "Address "
                    @if let Some(h) = h {
                        @match h.address_status.as_str() {
                            "verified" => { span.badge.good title="Entered by the guest and confirmed by Google's address validation" { "verified by guest + Google" } }
                            "guest-confirmed" => { span.badge.warn title="Google couldn't confirm it or suggested changes; the guest kept their version" { "guest kept unconfirmed version" } }
                            "unverified" => { span.badge.muted title="Entered by the guest; not checked" { "from guest, unchecked" } }
                            _ => {}
                        }
                        @if let Some(at) = &h.address_at { small.muted { " " (at.get(..10).unwrap_or("")) } }
                    }
                }
                input name="addr1" value=(v(|h| &h.addr1)) placeholder="Street" maxlength="200";
            }
            label.field.span2 { input name="addr2" value=(v(|h| &h.addr2)) placeholder="Unit / line 2 (optional)" maxlength="200" aria-label="Address line 2"; }
            label.field { span { "City / suburb" } input name="city" value=(v(|h| &h.city)) maxlength="100"; }
            label.field { span { "State / region" } input name="region" value=(v(|h| &h.region)) maxlength="100"; }
            label.field { span { "Postcode" } input name="postcode" value=(v(|h| &h.postcode)) maxlength="20"; }
            label.field { span { "Country" } input name="country" value=(v(|h| &h.country)) maxlength="100"; }
            label.field {
                span { "Side" }
                select name="side" {
                    option value="a" selected[side == "a"] { (cfg.couple.a_first) }
                    option value="b" selected[side == "b"] { (cfg.couple.b_first) }
                    option value="both" selected[side == "both"] { "Both" }
                }
            }
            label.field {
                span { "Tier" }
                @let tier = h.map(|h| h.tier).unwrap_or(1);
                select name="tier" { @for t in 1..=3 { option value=(t) selected[tier == t] { "Tier " (t) } } }
            }
            label.field { span { "Tags " small { "(comma separated)" } } input name="tags" value=(v(|h| &h.tags)) placeholder="family, uni, work" maxlength="300"; }
            label.check.span2 {
                input type="hidden" name="australia_present" value="1";
                input type="checkbox" name="australia" checked[h.is_some_and(|h| h.australia)];
                "Lives in Australia " span.muted.small { "(sees the Australia travel info on their page, cards and emails)" }
            }
            div.field.span2 {
                span { "Invited to" }
                div.checks {
                    @for e in &cfg.events {
                        label.check { input type="checkbox" name="events" value=(e.slug) checked[events.contains(&e.slug)]; (e.name) }
                    }
                }
            }
            label.field.span2 { span { "Notes" } textarea name="notes" rows="2" maxlength="2000" { (v(|h| &h.notes)) } }
        }
    }
}

fn guest_fields(cfg: &Config, prefix: &str, g: Option<&Guest>, household_events: &[String]) -> Markup {
    let n = |k: &str| format!("{prefix}{k}");
    html! {
        @if household_events.len() > 1 {
            div.guest-events {
                input type="hidden" name=(n("ev_present")) value="1";
                @for e in household_events {
                    label.check.small {
                        input type="checkbox" name=(n("ev")) value=(e) checked[g.is_none_or(|g| g.events.as_ref().is_none_or(|own| own.contains(e)))];
                        (cfg.event(e).map(|x| x.name.as_str()).unwrap_or(e))
                    }
                }
            }
        }
        input name=(n("first_name")) value=[g.map(|g| g.first_name.as_str())] placeholder="First" aria-label="First name" maxlength="80";
        input name=(n("last_name")) value=[g.map(|g| g.last_name.as_str())] placeholder="Last" aria-label="Last name" maxlength="80";
        input type="email" name=(n("email")) value=[g.map(|g| g.email.as_str())] placeholder="Email" aria-label="Email" maxlength="200";
        label.check.small { input type="checkbox" name=(n("is_child")) checked[g.is_some_and(|g| g.is_child)]; "Child" }
        label.check.small { input type="checkbox" name=(n("plus_one_allowed")) checked[g.is_some_and(|g| g.plus_one_allowed)]; "+1" }
    }
}

async fn new_household(State(st): State<AppState>, Member(s): Member) -> AppResult {
    let body = html! {
        form.card method="post" action="/admin/households/new" {
            (csrf_input(&s.csrf))
            h2 { "People" }
            p.muted.small { "Leave rows blank to skip them. Tick +1 to let someone bring a guest of their choosing." }
            @for i in 0..6 { div.guest-row { (guest_fields(&st.cfg, &format!("g{i}."), None, &[])) } }
            h2 { "Household" }
            p.muted.small { "Leave name and greeting blank to build them from the first names above." }
            (household_fields(&st.cfg, None))
            div.form-actions { button.button.primary type="submit" { "Create household" } }
        }
    };
    page(&st, &s, "households", "New household", &Flash::default(), body).await
}

async fn create_household(State(st): State<AppState>, Member(s): Member, Form(pairs): Form<Vec<(String, String)>>) -> AppResult {
    let f = F::new(pairs);
    guard!(f.csrf(&s));
    let mut input = household_input_from(&f, &st.cfg);
    for i in 0..6 {
        let g = guest_input_from(&f, &format!("g{i}."), &[]);
        if g.first_name.as_deref().is_some_and(|n| !n.is_empty()) {
            input.guests.push(g);
        }
    }
    if let Err(e) = input.validate(&event_slugs(&st.cfg)) {
        return Ok(ui::error_page(axum::http::StatusCode::BAD_REQUEST, "Check the form", &e.to_string()));
    }
    let (code, created) = apply_change(&st, &s, "household.create", None, serde_json::to_value(&input)?).await?;
    Ok(match created {
        Some(id) => redirect(&format!("/admin/households/{id}?m={code}")),
        None => redirect(&format!("/admin/households?m={code}")),
    })
}

async fn household(State(st): State<AppState>, Member(s): Member, Path(id): Path<i64>, Query(fl): Query<Flash>) -> AppResult {
    let data = st
        .db
        .call(move |c| {
            let Some(h) = store::get_household(c, id)? else { return Ok(None) };
            Ok(Some((
                h,
                store::guests_for(c, id)?,
                store::responses_for(c, id)?,
                store::submissions(c, Some(id), 20)?,
                store::audit_log(c, 30, Some(("household", &id.to_string())))?,
            )))
        })
        .await?;
    let Some((h, guests, responses, subs, log)) = data else { return Ok(ui::not_found()) };
    let cfg = &st.cfg;
    let owner = s.user.is_owner();
    let token = store::household_token(&st.env.secret, &h);
    let url = assets::link(cfg, &token);
    let qr = assets::qr_svg(&url, "#24211d")?;
    let resp: HashMap<(i64, String), Resp> = responses.iter().map(|r| ((r.guest_id, r.event_slug.clone()), r.clone())).collect();
    let status = store::household_status(&h, &guests, &resp);
    let label = |gid: i64| guests.iter().find(|g| g.id == gid).map(|g| g.full_name()).unwrap_or_else(|| format!("#{gid}"));
    let has_email = !h.email.is_empty() || guests.iter().any(|g| !g.email.is_empty());

    let body = html! {
        @if h.archived_at.is_some() { div.alert.warn { "This household is archived: their link no longer works and they're excluded from lists." } }
        div.head-row {
            div {
                p.muted.small { (ui::side_label(cfg, &h.side)) " · tier " (h.tier) @if h.australia { " · lives in Australia" } @for t in h.tag_list() { " " span.tag { (t) } } }
                form.chips-toggle.check-line method="post" action={ "/admin/households/" (h.id) "/quick" } {
                    (csrf_input(&s.csrf)) input type="hidden" name="back" value={ "/admin/households/" (h.id) };
                    button.ok type="submit" name="toggle_checked" value="1" aria-pressed=(if h.checked_at.is_some() { "true" } else { "false" }) {
                        @if h.checked_at.is_some() { "Names and details checked" } @else { "Mark names and details as correct" }
                    }
                    @if let (Some(by), Some(at)) = (&h.checked_by, &h.checked_at) { span.muted.small { " by " (by) ", " (when(cfg, at)) } }
                }
                p.lead { "“Dear " (h.greeting) "”" }
            }
            (status_badge(status))
        }
        div.grid2.wide-left {
            div {
                section.card {
                    h2 { "People" }
                    @for g in &guests {
                        form.guest-row method="post" action={ "/admin/guests/" (g.id) } {
                            (csrf_input(&s.csrf))
                            (guest_fields(cfg, "", Some(g), if g.plus_one_of.is_some() { &[] } else { &h.events }))
                            input name="dietary" value=(g.dietary) placeholder="Dietary" aria-label="Dietary" maxlength="500";
                            input name="notes" value=(g.notes) placeholder="Notes" aria-label="Notes" maxlength="2000";
                            div.row-actions {
                                button.button.small type="submit" { "Save" }
                                button.button.small.ghost type="submit" formaction={ "/admin/guests/" (g.id) "/archive" } data-confirm={ "Remove " (g.full_name()) "?" } { "Remove" }
                            }
                            @if let Some(host) = g.plus_one_of { p.muted.small.span-all { "Plus-one of " (label(host)) " (added by the guest)" } }
                        }
                    }
                    form.guest-row.add method="post" action={ "/admin/households/" (h.id) "/guests" } {
                        (csrf_input(&s.csrf))
                        (guest_fields(cfg, "", None, &h.events))
                        div.row-actions { button.button.small.primary type="submit" { "Add person" } }
                    }
                }
                section.card {
                    h2 { "RSVP" }
                    @if h.rsvp_at.is_none() { p.muted { "No reply yet." @if let Some(v) = &h.first_viewed_at { " First opened their link " (when(cfg, v)) "." } } }
                    @else {
                        p.muted.small { "Last updated " (when_opt(cfg, &h.rsvp_at)) }
                        table.compact {
                            thead { tr { th { "Guest" } @for e in &h.events { th { (cfg.event(e).map(|e| e.name.as_str()).unwrap_or(e)) } } th { "Dietary" } } }
                            tbody {
                                @for g in &guests {
                                    tr {
                                        td { (g.full_name()) }
                                        @for e in &h.events {
                                            td {
                                                @if g.plus_one_of.is_none() && !g.invited(&h, e) { span.muted { "not invited" } } @else {
                                                @match resp.get(&(g.id, e.clone())) {
                                                    Some(r) if r.attending => { span.good { "Yes" } @if let Some(m) = cfg.meal(&r.meal) { span.muted { " · " (m.name) } } }
                                                    Some(_) => { span.bad { "No" } }
                                                    None => { span.muted { "no reply" } }
                                                }
                                                }
                                            }
                                        }
                                        td.small { (g.dietary) }
                                    }
                                }
                            }
                        }
                        dl.kv {
                            @if !h.rsvp_song.is_empty() { dt { "Song" } dd { (h.rsvp_song) } }
                            @if !h.rsvp_accommodation.is_empty() { dt { "Accommodation" } dd { (h.rsvp_accommodation) } }
                            @if !h.rsvp_transport.is_empty() { dt { "Shuttle" } dd { (h.rsvp_transport) } }
                            @if !h.rsvp_message.is_empty() { dt { "Message" } dd.message { (h.rsvp_message) } }
                        }
                    }
                    @if !subs.is_empty() {
                        details {
                            summary { (subs.len()) " submission(s) on record" }
                            ul.feed { @for sub in &subs { li { (when(cfg, &sub.received_at)) span.muted.small { " · " (sub.id) } } } }
                        }
                    }
                }
                section.card {
                    h2 { "Household details" }
                    form method="post" action={ "/admin/households/" (h.id) } {
                        (csrf_input(&s.csrf))
                        (household_fields(cfg, Some(&h)))
                        div.form-actions { button.button.primary type="submit" { "Save changes" } }
                    }
                }
            }
            div {
                section.card.link-card {
                    h2 { "Their link" }
                    div.qr { (PreEscaped(qr)) }
                    div.copy-row { input #guest-url readonly value=(url) aria-label="Guest link"; button.button.small type="button" data-copy="#guest-url" { "Copy" } }
                    p.small { a href=(url) target="_blank" rel="noopener noreferrer" { "Open their page ↗" } }
                    h3 { "Stationery" }
                    ul.downloads {
                        @for k in [Kind::SaveTheDate, Kind::Invitation, Kind::Envelope] {
                            li { a href={ "/admin/stationery/" (k.slug()) ".pdf?household=" (h.id) } { (k.label()) } " " a.small href={ "/admin/stationery/" (k.slug()) ".png?household=" (h.id) } { "PNG" } }
                        }
                    }
                    @if owner {
                        h3 { "Email" }
                        @if !has_email { p.muted.small { "Add an email address to send by email." } }
                        @else {
                            p.small.muted { "Save the date: " (when_opt(cfg, &h.std_sent_at)) br; "Invitation: " (when_opt(cfg, &h.invite_sent_at)) }
                            form.inline-actions method="post" action={ "/admin/households/" (h.id) "/send" } {
                                (csrf_input(&s.csrf))
                                button.button.small type="submit" name="kind" value="save_the_date" data-confirm="Email the save the date to this household now?" { "Email save the date" }
                                button.button.small type="submit" name="kind" value="invitation" data-confirm="Email the invitation to this household now?" { "Email invitation" }
                            }
                        }
                        h3 { "Danger zone" }
                        form method="post" action={ "/admin/households/" (h.id) "/rotate" } {
                            (csrf_input(&s.csrf))
                            button.button.small.ghost type="submit" data-confirm="Generate a new link? The old link and any printed QR codes will stop working." { "Replace link" }
                        }
                        form method="post" action={ "/admin/households/" (h.id) "/archive" } {
                            (csrf_input(&s.csrf))
                            @if h.archived_at.is_some() {
                                input type="hidden" name="restore" value="1";
                                button.button.small.ghost type="submit" { "Restore household" }
                            } @else {
                                button.button.small.ghost type="submit" data-confirm="Archive this household? Their link will stop working." { "Archive household" }
                            }
                        }
                    }
                }
                section.card {
                    h2 { "Activity" }
                    ul.feed.small {
                        @for a in &log { li { span.muted { (when(cfg, &a.at)) } " · " (a.actor) " · " (a.action) } }
                        @if log.is_empty() { li.muted { "Nothing yet." } }
                    }
                }
            }
        }
    };
    page(&st, &s, "households", &h.name, &fl, body).await
}

async fn update_household(State(st): State<AppState>, Member(s): Member, Path(id): Path<i64>, Form(pairs): Form<Vec<(String, String)>>) -> AppResult {
    let f = F::new(pairs);
    guard!(f.csrf(&s));
    let input = household_input_from(&f, &st.cfg);
    if let Err(e) = input.validate(&event_slugs(&st.cfg)) {
        return Ok(ui::error_page(axum::http::StatusCode::BAD_REQUEST, "Check the form", &e.to_string()));
    }
    let (code, _) = apply_change(&st, &s, "household.update", Some(id), serde_json::to_value(&input)?).await?;
    Ok(redirect(&format!("/admin/households/{id}?m={code}")))
}

async fn add_guest(State(st): State<AppState>, Member(s): Member, Path(id): Path<i64>, Form(pairs): Form<Vec<(String, String)>>) -> AppResult {
    let f = F::new(pairs);
    guard!(f.csrf(&s));
    let hh_events = st.db.call(move |c| Ok(store::must_household(c, id)?.events)).await?;
    let g = guest_input_from(&f, "", &hh_events);
    if g.first_name.as_deref().is_none_or(str::is_empty) || g.validate().is_err() {
        return Ok(redirect(&format!("/admin/households/{id}?m=error")));
    }
    let (code, _) = apply_change(&st, &s, "guest.create", Some(id), serde_json::to_value(&g)?).await?;
    Ok(redirect(&format!("/admin/households/{id}?m={code}")))
}

async fn guest_household(st: &AppState, gid: i64) -> anyhow::Result<i64> {
    Ok(st.db.call(move |c| store::must_guest(c, gid)).await?.household_id)
}

async fn update_guest(State(st): State<AppState>, Member(s): Member, Path(gid): Path<i64>, Form(pairs): Form<Vec<(String, String)>>) -> AppResult {
    let f = F::new(pairs);
    guard!(f.csrf(&s));
    let hid = guest_household(&st, gid).await?;
    let hh_events = st.db.call(move |c| Ok(store::must_household(c, hid)?.events)).await?;
    let g = guest_input_from(&f, "", &hh_events);
    if g.first_name.as_deref().is_none_or(str::is_empty) || g.validate().is_err() {
        return Ok(redirect(&format!("/admin/households/{hid}?m=error")));
    }
    let (code, _) = apply_change(&st, &s, "guest.update", Some(gid), serde_json::to_value(&g)?).await?;
    Ok(redirect(&format!("/admin/households/{hid}?m={code}")))
}

async fn archive_guest(State(st): State<AppState>, Member(s): Member, Path(gid): Path<i64>, Form(pairs): Form<Vec<(String, String)>>) -> AppResult {
    let f = F::new(pairs);
    guard!(f.csrf(&s));
    let hid = guest_household(&st, gid).await?;
    let (code, _) = apply_change(&st, &s, "guest.archive", Some(gid), json!({})).await?;
    Ok(redirect(&format!("/admin/households/{hid}?m={}", if code == "saved" { "archived" } else { code })))
}

async fn archive_household(State(st): State<AppState>, Member(s): Member, Path(id): Path<i64>, Form(pairs): Form<Vec<(String, String)>>) -> AppResult {
    let f = F::new(pairs);
    guard!(f.csrf(&s));
    guard!(s.require_owner());
    let restore = f.check("restore");
    let actor = s.user.actor();
    st.db.call(move |c| store::set_household_archived(c, &actor, id, !restore)).await?;
    Ok(redirect(&format!("/admin/households/{id}?m={}", if restore { "restored" } else { "archived" })))
}

async fn rotate_link(State(st): State<AppState>, Member(s): Member, Path(id): Path<i64>, Form(pairs): Form<Vec<(String, String)>>) -> AppResult {
    let f = F::new(pairs);
    guard!(f.csrf(&s));
    guard!(s.require_owner());
    let (actor, secret) = (s.user.actor(), st.env.secret.clone());
    st.db.call(move |c| store::rotate_token(c, &actor, &secret, id)).await?;
    Ok(redirect(&format!("/admin/households/{id}?m=rotated")))
}

// ---------------------------------------------------------------------------
// People

#[derive(Deserialize, Default)]
struct PeopleQuery {
    event: Option<String>,
    answer: Option<String>,
    q: Option<String>,
}

async fn guests(State(st): State<AppState>, Member(s): Member, Query(q): Query<PeopleQuery>) -> AppResult {
    let snap = Snapshot::load(&st).await?;
    let cfg = &st.cfg;
    let by_h: HashMap<i64, &Household> = snap.households.iter().map(|h| (h.id, h)).collect();
    let needle = q.q.clone().unwrap_or_default().to_lowercase();
    let rows: Vec<&Guest> = snap
        .guests
        .iter()
        .filter(|g| needle.is_empty() || g.full_name().to_lowercase().contains(&needle))
        .filter(|g| {
            let Some(ev) = q.event.as_deref().filter(|e| !e.is_empty()) else { return true };
            let invited = by_h.get(&g.household_id).is_some_and(|h| g.invited(h, ev));
            let r = snap.responses.get(&(g.id, ev.to_string()));
            match q.answer.as_deref() {
                Some("yes") => r.is_some_and(|r| r.attending),
                Some("no") => r.is_some_and(|r| !r.attending),
                Some("waiting") => invited && r.is_none(),
                _ => invited,
            }
        })
        .collect();
    let body = html! {
        div.toolbar {
            form.filters method="get" {
                input type="search" name="q" value=[q.q.as_deref()] placeholder="Search people…";
                select name="event" {
                    option value="" { "Any event" }
                    @for e in &cfg.events { option value=(e.slug) selected[q.event.as_deref() == Some(e.slug.as_str())] { (e.name) } }
                }
                select name="answer" {
                    option value="" { "Invited" }
                    option value="yes" selected[q.answer.as_deref() == Some("yes")] { "Attending" }
                    option value="no" selected[q.answer.as_deref() == Some("no")] { "Declined" }
                    option value="waiting" selected[q.answer.as_deref() == Some("waiting")] { "Waiting" }
                }
                button.button type="submit" { "Filter" }
            }
            a.button href="/admin/export/guests.csv" { "Export CSV" }
        }
        p.muted { (rows.len()) " people" }
        div.table-wrap {
            table.list {
                thead { tr { th { "Name" } th { "Household" } @for e in &cfg.events { th { (e.name) } } th { "Dietary" } } }
                tbody {
                    @for g in &rows {
                        @let h = by_h.get(&g.household_id);
                        tr {
                            td { (g.full_name()) @if g.is_child { span.tag { "child" } } @if g.plus_one_of.is_some() { span.tag { "+1" } } }
                            td.small { a href={ "/admin/households/" (g.household_id) } { (h.map(|h| h.name.as_str()).unwrap_or("")) } }
                            @for e in &cfg.events {
                                td.small {
                                    @if h.is_some_and(|h| g.invited(h, &e.slug)) {
                                        @match snap.responses.get(&(g.id, e.slug.clone())) {
                                            Some(r) if r.attending => { span.good { (cfg.meal(&r.meal).map(|m| m.name.as_str()).unwrap_or("Yes")) } }
                                            Some(_) => { span.bad { "No" } }
                                            None => { span.muted { "…" } }
                                        }
                                    }
                                }
                            }
                            td.small { (g.dietary) }
                        }
                    }
                }
            }
        }
    };
    page(&st, &s, "guests", "People", &Flash::default(), body).await
}

async fn export_csv(State(st): State<AppState>, Member(s): Member) -> AppResult {
    let snap = Snapshot::load(&st).await?;
    let cfg = &st.cfg;
    let by_h: HashMap<i64, &Household> = snap.households.iter().map(|h| (h.id, h)).collect();
    let esc = |v: &str| {
        // Neutralise spreadsheet formula injection, then quote.
        let v = if v.starts_with(['=', '+', '-', '@', '\t', '\r']) { format!("'{v}") } else { v.to_string() };
        format!("\"{}\"", v.replace('"', "\"\""))
    };
    let mut out = String::from("household_id,household,greeting,guest_id,first_name,last_name,email,child,plus_one_of,side,tags");
    for e in &cfg.events {
        out.push_str(&format!(",{}_attending,{}_meal", e.slug, e.slug));
    }
    out.push_str(",dietary,address,song,accommodation,transport,message,replied_at\n");
    for g in &snap.guests {
        let Some(h) = by_h.get(&g.household_id) else { continue };
        let mut row = vec![
            h.id.to_string(),
            esc(&h.name),
            esc(&h.greeting),
            g.id.to_string(),
            esc(&g.first_name),
            esc(&g.last_name),
            esc(&g.email),
            g.is_child.to_string(),
            g.plus_one_of.map(|x| x.to_string()).unwrap_or_default(),
            h.side.clone(),
            esc(&h.tags),
        ];
        for e in &cfg.events {
            let (a, m) = if !g.invited(h, &e.slug) {
                ("not_invited".to_string(), String::new())
            } else {
                match snap.responses.get(&(g.id, e.slug.clone())) {
                    Some(r) => (if r.attending { "yes" } else { "no" }.to_string(), r.meal.clone()),
                    None => ("waiting".into(), String::new()),
                }
            };
            row.push(a);
            row.push(esc(&m));
        }
        row.extend([
            esc(&g.dietary),
            esc(&h.address_lines().join(", ")),
            esc(&h.rsvp_song),
            esc(&h.rsvp_accommodation),
            esc(&h.rsvp_transport),
            esc(&h.rsvp_message),
            h.rsvp_at.clone().unwrap_or_default(),
        ]);
        out.push_str(&row.join(","));
        out.push('\n');
    }
    let actor = s.user.actor();
    st.db.call(move |c| store::audit(c, &actor, "export.csv", "", "", &json!({}))).await?;
    Ok(([(header::CONTENT_TYPE, "text/csv; charset=utf-8"), (header::CONTENT_DISPOSITION, "attachment; filename=\"guests.csv\"")], out).into_response())
}

fn safe_back(back: &str, fallback: &str) -> String {
    if back.starts_with("/admin/") && !back.contains("//") { back.to_string() } else { fallback.to_string() }
}

// ---------------------------------------------------------------------------
// Stationery

async fn stationery(State(st): State<AppState>, Member(s): Member) -> AppResult {
    let snap = Snapshot::load(&st).await?;
    let first = snap.households.first().map(|h| h.id);
    let tags: std::collections::BTreeSet<String> = snap.households.iter().flat_map(|h| h.tag_list().into_iter().map(String::from)).collect();
    let body = html! {
        p.muted { "Everything here is generated from your guest list and the templates. Print-shop PDFs include 3 mm bleed and trim boxes; home-print sheets put two cards on " (st.cfg.print.paper) " with crop marks." }
        div.stationery-grid {
            @for k in Kind::ALL {
                section.card.stationery {
                    @if let Some(id) = first {
                        @if k != Kind::Report && k != Kind::Labels && k != Kind::PlaceCards {
                            a.preview href={ "/admin/stationery/" (k.slug()) ".pdf?household=" (id) } { img src={ "/admin/stationery/" (k.slug()) ".png?household=" (id) } alt={ (k.label()) " preview" } loading="lazy"; }
                        }
                    }
                    h2 { (k.label()) }
                    form.stationery-form method="get" action={ "/admin/stationery/" (k.slug()) ".pdf" } {
                        @if matches!(k, Kind::SaveTheDate | Kind::Invitation | Kind::Envelope | Kind::Labels | Kind::PlaceCards) {
                            select name="scope" aria-label="Which households" {
                                option value="all" { "All households" }
                                @if matches!(k, Kind::SaveTheDate | Kind::Envelope | Kind::Labels) { option value="std_unsent" { "Save the date not yet sent" } }
                                @if matches!(k, Kind::Invitation | Kind::Envelope | Kind::Labels) { option value="invite_unsent" { "Invitation not yet sent" } }
                                @for t in 1..=3 { option value={ "tier:" (t) } { "Tier " (t) " only" } }
                                option value="side:a" { (st.cfg.couple.a_first) "'s side" }
                                option value="side:b" { (st.cfg.couple.b_first) "'s side" }
                                @for t in &tags { option value={ "tag:" (t) } { "Tag: " (t) } }
                            }
                        }
                        @if k.has_sheet_layout() {
                            select name="layout" aria-label="Layout" {
                                option value="card" { "Print shop (bleed)" }
                                option value="sheet" { "Home print (2-up + crop marks)" }
                            }
                        }
                        button.button.primary type="submit" { "Download PDF" }
                    }
                }
            }
        }
        p.small.muted { "Templates are in " code { "templates/*.typ" } ". Drop a modified copy in " code { (st.env.data_dir.join("templates").display()) } " on the server to override without redeploying." }
    };
    page(&st, &s, "stationery", "Stationery", &Flash::default(), body).await
}

#[derive(Deserialize)]
struct StationeryQuery {
    household: Option<i64>,
    scope: Option<String>,
    layout: Option<String>,
}

async fn stationery_file(State(st): State<AppState>, Member(s): Member, Path(file): Path<String>, Query(q): Query<StationeryQuery>) -> AppResult {
    let Some((slug, ext)) = file.rsplit_once('.') else { return Ok(ui::not_found()) };
    let Some(kind) = Kind::parse(slug) else { return Ok(ui::not_found()) };
    let ext: &'static str = match ext {
        "pdf" => "pdf",
        "png" => "png",
        _ => return Ok(ui::not_found()),
    };
    let layout = Layout::parse(q.layout.as_deref().unwrap_or("card"));
    let scope = q.scope.unwrap_or_else(|| "all".into());
    let (r, cfg, env) = (st.renderer.clone(), st.cfg.clone(), st.env.clone());
    let household = q.household;
    let actor = s.user.actor();
    let bytes = st
        .db
        .call(move |c| {
            let doc = if kind == Kind::Report {
                let data = assets::report_data(&cfg, &store::list_households(c, false)?, &store::all_guests(c)?, &store::all_responses(c)?)?;
                r.compile(kind, &data, HashMap::new())?
            } else {
                let mut rows = vec![];
                for h in store::list_households(c, household.is_some())? {
                    if household.is_some_and(|id| id != h.id) {
                        continue;
                    }
                    let keep = match scope.as_str() {
                        "std_unsent" => h.std_sent_at.is_none(),
                        "invite_unsent" => h.invite_sent_at.is_none(),
                        s if s.starts_with("side:") => h.side == s[5..],
                        s if s.starts_with("tier:") => h.tier.to_string() == s[5..],
                        s if s.starts_with("tag:") => h.tag_list().contains(&&s[4..]),
                        _ => true,
                    };
                    if keep || household.is_some() {
                        let g = store::guests_for(c, h.id)?;
                        let rs = store::responses_for(c, h.id)?;
                        rows.push((h, g, rs));
                    }
                }
                assets::render_households(&r, &cfg, &env, kind, layout, &rows)?
            };
            store::audit(c, &actor, "stationery.render", "", kind.slug(), &json!({"household": household}))?;
            if ext == "pdf" { assets::to_pdf(&doc) } else { assets::to_png(&doc, 0, 1.5) }
        })
        .await?;
    let ct = if ext == "pdf" { "application/pdf" } else { "image/png" };
    let name = format!("{}.{}", kind.slug(), ext);
    Ok(([(header::CONTENT_TYPE, ct.to_string()), (header::CONTENT_DISPOSITION, format!("inline; filename=\"{name}\""))], bytes).into_response())
}

// ---------------------------------------------------------------------------
// Sending (owners)

async fn send_one(State(st): State<AppState>, Member(s): Member, Path(id): Path<i64>, Form(pairs): Form<Vec<(String, String)>>) -> AppResult {
    let f = F::new(pairs);
    guard!(f.csrf(&s));
    guard!(s.require_owner());
    let kind = f.s("kind");
    if !matches!(kind.as_str(), "save_the_date" | "invitation") {
        return Ok(redirect(&format!("/admin/households/{id}?m=error")));
    }
    let actor = s.user.actor();
    let ok = st
        .db
        .call(move |c| {
            let h = store::must_household(c, id)?;
            let gs = store::guests_for(c, id)?;
            if crate::email::recipients(&h, &gs).is_empty() {
                return Ok(false);
            }
            let tx = crate::db::begin(c)?;
            store::enqueue(&tx, "email", &json!({"kind": kind, "household_id": id}))?;
            store::audit(&tx, &actor, "email.queue", "household", &id.to_string(), &json!({"kind": kind}))?;
            tx.commit()?;
            Ok(true)
        })
        .await?;
    Ok(redirect(&format!("/admin/households/{id}?m={}", if ok { "queued" } else { "noemail" })))
}

async fn send_page(State(st): State<AppState>, Member(s): Member, Query(fl): Query<Flash>) -> AppResult {
    guard!(s.require_owner());
    let snap = Snapshot::load(&st).await?;
    let outbox = st.db.call(|c| crate::notify::recent(c)).await?;
    let cfg = &st.cfg;
    let configured = st.env.resend_key.is_some() && cfg.email.from.is_some();

    // Per kind and tier: households still to send, already sent, and with no email at all.
    struct Counts {
        ready: [usize; 3],
        sent: usize,
        no_email: usize,
    }
    let counts = |is_sent: fn(&Household) -> bool| {
        let mut c = Counts { ready: [0; 3], sent: 0, no_email: 0 };
        for h in snap.households.iter().filter(|h| !h.events.is_empty()) {
            if is_sent(h) {
                c.sent += 1;
            } else if crate::email::recipients(h, &snap.guests_of(h.id)).is_empty() {
                c.no_email += 1;
            } else {
                c.ready[(h.tier.clamp(1, 3) - 1) as usize] += 1;
            }
        }
        c
    };
    let std_c = counts(|h| h.std_sent_at.is_some());
    let inv_c = counts(|h| h.invite_sent_at.is_some());

    let send_card = |kind: &str, title: &str, blurb: &str, button: &str, c: &Counts| -> Markup {
        html! {
            section.card.send-card {
                h2 { (title) }
                p.muted.small { (blurb) }
                div.send-stats {
                    @for (i, n) in c.ready.iter().enumerate() {
                        div.send-stat { span.send-num { (n) } span.send-label { "Tier " (i + 1) " ready" } }
                    }
                    div.send-stat.quiet { span.send-num { (c.sent) } span.send-label { "already sent" } }
                    div.send-stat.quiet { span.send-num { (c.no_email) } span.send-label {
                        @if c.no_email > 0 { a href="/admin/households?filter=no_email" { "no email" } } @else { "no email" }
                    } }
                }
                form.send-form method="post" action="/admin/send" {
                    (csrf_input(&s.csrf)) input type="hidden" name="kind" value=(kind);
                    label.field.inline-field {
                        span { "Send to" }
                        select name="max_tier" {
                            option value="1" { "Tier 1 (" (c.ready[0]) ")" }
                            option value="2" { "Tiers 1 and 2 (" (c.ready[0] + c.ready[1]) ")" }
                            option value="3" { "All tiers (" (c.ready.iter().sum::<usize>()) ")" }
                        }
                    }
                    label.check.confirm { input type="checkbox" name="confirm" required; "I've checked the test email and I'm ready to send" }
                    div.form-actions { button.button.primary type="submit" disabled[!configured || c.ready.iter().sum::<usize>() == 0] { (button) } }
                }
            }
        }
    };

    let body = html! {
        @if !configured {
            div.alert.warn { "Email isn't set up yet. Add " code { "RESEND_API_KEY" } " to the server's env file and " code { "[email] from" } " to wedding.toml, then restart." }
        }
        div.grid2 {
            (send_card("save_the_date", "Save the dates", "Each household gets an email with their card and their personal link. Households already sent are skipped.", "Send save the dates", &std_c))
            (send_card("invitation", "Invitations", "Each household gets their invitation with the details card attached and a link to RSVP. Households already sent are skipped.", "Send invitations", &inv_c))
        }
        section.card {
            h2 { "Send yourself a test" }
            p.muted.small { "Sends exactly what a household would receive, but only to the address you enter. Nothing is marked as sent." }
            form.test-form method="post" action="/admin/send/test" {
                (csrf_input(&s.csrf))
                label.field { span { "Send to" } input type="email" name="to" placeholder="you@example.com" required; }
                label.field { span { "Which email" } select name="kind" { option value="save_the_date" { "Save the date" } option value="invitation" { "Invitation" } } }
                label.field { span { "As if it were for" } select name="household" { @for h in &snap.households { option value=(h.id) { (h.name) } } } }
                div.test-submit { button.button type="submit" disabled[!configured || snap.households.is_empty()] { "Send test email" } }
            }
            div.divider {}
            form.discord-test method="post" action="/admin/send/test" {
                (csrf_input(&s.csrf)) input type="hidden" name="kind" value="discord";
                span.muted.small { "RSVPs, addresses and sign-ups are posted to Discord." }
                button.button.ghost.small type="submit" { "Send a test Discord message" }
            }
        }
        section.card {
            h2 { "Delivery log" }
            @if outbox.is_empty() { p.muted { "Nothing sent yet." } }
            @else {
                div.table-wrap.flush {
                    table.list.compact-log {
                        thead { tr { th { "What" } th { "Status" } th.num { "Tries" } th { "Queued" } } }
                        tbody {
                            @for o in &outbox {
                                tr {
                                    td { span.muted.small { (o.kind) " · " } (o.what) }
                                    td {
                                        @if o.sent_at.is_some() { span.badge.good { "sent" } }
                                        @else if o.failed_at.is_some() { span.badge.bad { "failed" } }
                                        @else { span.badge.muted { "pending" } }
                                        @if !o.last_error.is_empty() { div.small.bad { (o.last_error) } }
                                    }
                                    td.num { (o.attempts) }
                                    td.small.muted.nowrap { (when(cfg, &o.created_at)) }
                                }
                            }
                        }
                    }
                }
            }
        }
    };
    page(&st, &s, "send", "Send", &fl, body).await
}

async fn send_bulk(State(st): State<AppState>, Member(s): Member, Form(pairs): Form<Vec<(String, String)>>) -> AppResult {
    let f = F::new(pairs);
    guard!(f.csrf(&s));
    guard!(s.require_owner());
    let kind = f.s("kind");
    if !matches!(kind.as_str(), "save_the_date" | "invitation") || !f.check("confirm") {
        return Ok(redirect("/admin/send?m=error"));
    }
    let actor = s.user.actor();
    let max_tier: i64 = f.s("max_tier").parse().unwrap_or(1).clamp(1, 3);
    st.db
        .call(move |c| {
            let tx = crate::db::begin(c)?;
            let mut n = 0;
            for h in store::list_households(&tx, false)?.into_iter().filter(|h| h.tier <= max_tier && !h.events.is_empty()) {
                let sent = if kind == "save_the_date" { h.std_sent_at.is_some() } else { h.invite_sent_at.is_some() };
                // Skip anything already sent or already queued.
                let queued: i64 = tx.query_row(
                    "SELECT COUNT(*) FROM outbox WHERE kind = 'email' AND sent_at IS NULL AND failed_at IS NULL
                     AND json_extract(payload, '$.household_id') = ?1 AND json_extract(payload, '$.kind') = ?2",
                    rusqlite::params![h.id, kind],
                    |r| r.get(0),
                )?;
                if sent || queued > 0 || crate::email::recipients(&h, &store::guests_for(&tx, h.id)?).is_empty() {
                    continue;
                }
                store::enqueue(&tx, "email", &json!({"kind": kind, "household_id": h.id}))?;
                n += 1;
            }
            store::audit(&tx, &actor, "email.bulk_queue", "", "", &json!({"kind": kind, "count": n, "max_tier": max_tier}))?;
            tx.commit()?;
            Ok(())
        })
        .await?;
    Ok(redirect("/admin/send?m=queued"))
}

async fn send_test(State(st): State<AppState>, Member(s): Member, Form(pairs): Form<Vec<(String, String)>>) -> AppResult {
    let f = F::new(pairs);
    guard!(f.csrf(&s));
    guard!(s.require_owner());
    let kind = f.s("kind");
    let actor = s.user.actor();
    if kind == "discord" {
        let name = s.user.name.clone();
        st.db.call(move |c| store::enqueue(c, "discord", &json!({"type": "test", "name": name}))).await?;
        return Ok(redirect("/admin/send?m=tested"));
    }
    let to = f.s("to");
    let Ok(hid) = f.s("household").parse::<i64>() else { return Ok(redirect("/admin/send?m=error")) };
    if !store::plausible_email(&to) || !matches!(kind.as_str(), "save_the_date" | "invitation") {
        return Ok(redirect("/admin/send?m=error"));
    }
    st.db
        .call(move |c| {
            store::enqueue(c, "email", &json!({"kind": kind, "household_id": hid, "test_to": to}))?;
            store::audit(c, &actor, "email.test", "household", &hid.to_string(), &json!({}))
        })
        .await?;
    Ok(redirect("/admin/send?m=queued"))
}

// ---------------------------------------------------------------------------
// Team (owners)

async fn users(State(st): State<AppState>, Member(s): Member, Query(q): Query<InviteQuery>) -> AppResult {
    guard!(s.require_owner());
    let list = st.db.call(|c| store::users(c)).await?;
    let cfg = &st.cfg;
    let invite_url = q.code.as_ref().map(|c| format!("{}/admin/register?invite={c}", cfg.site.base_url));
    let body = html! {
        @if let Some(u) = &invite_url {
            div.alert.good {
                p { strong { "Invite link created." } " Send it privately. It works once and expires in " (q.hours.unwrap_or(48)) " hours. It won't be shown again." }
                div.copy-row { input #invite-url readonly value=(u) aria-label="Invite link"; button.button.small type="button" data-copy="#invite-url" { "Copy" } }
            }
        }
        @let pending: Vec<_> = list.iter().filter(|u| u.role == "pending").collect();
        @if !pending.is_empty() {
            section.card {
                h2 { "Waiting for approval" }
                p.muted.small { "Only approve names you recognise. Anyone can create a pending account, but they see nothing until you approve them." }
                @for u in &pending {
                    form.user-row method="post" action="/admin/users" {
                        (csrf_input(&s.csrf)) input type="hidden" name="user" value=(u.id);
                        span.strong { (u.name) } span.muted.small { " signed up " (when(cfg, &u.created_at)) }
                        span.spacer {}
                        button.button.small.primary type="submit" name="role" value="helper" { "Approve as helper" }
                        button.button.small type="submit" name="role" value="owner" data-confirm={ "Make " (u.name) " an owner? Owners can do everything." } { "Approve as owner" }
                        button.button.small.ghost type="submit" name="role" value="disabled" { "Reject" }
                    }
                }
            }
        }
        section.card {
            h2 { "Team" }
            p.muted.small { strong { "Helpers" } " can view and edit the guest list. " strong { "Owners" } " can also send emails, manage the team and change settings." }
            @for u in list.iter().filter(|u| u.role != "pending") {
                form.user-row method="post" action="/admin/users" {
                    (csrf_input(&s.csrf)) input type="hidden" name="user" value=(u.id);
                    span.strong { (u.name) } @if u.id == s.user.id { span.muted.small { " (you)" } }
                    span.muted.small { " last seen " (when_opt(cfg, &u.last_login_at)) }
                    span.spacer {}
                    select name="role" aria-label="Role" {
                        @for r in ["owner", "helper", "disabled"] { option value=(r) selected[u.role == r] { (r) } }
                    }
                    button.button.small type="submit" { "Update" }
                }
            }
        }
        section.card {
            h2 { "Invite someone" }
            p.muted.small { "Invite links skip the approval step. Alternatively, people can sign up at " code { (cfg.site.base_url) "/admin/register" } " and you approve them here." }
            form.inline-actions method="post" action="/admin/users/invite" {
                (csrf_input(&s.csrf))
                select name="role" aria-label="Role" { option value="helper" { "Helper" } option value="owner" { "Owner" } }
                select name="hours" aria-label="Expires" { option value="24" { "24 hours" } option value="48" selected { "48 hours" } option value="168" { "7 days" } }
                button.button.primary type="submit" { "Create invite link" }
            }
        }
    };
    page(&st, &s, "users", "Team", &Flash { m: q.m }, body).await
}

#[derive(Deserialize, Default)]
struct InviteQuery {
    code: Option<String>,
    hours: Option<i64>,
    m: Option<String>,
}

async fn user_role(State(st): State<AppState>, Member(s): Member, Form(pairs): Form<Vec<(String, String)>>) -> AppResult {
    let f = F::new(pairs);
    guard!(f.csrf(&s));
    guard!(s.require_owner());
    let (uid, role, actor) = (f.s("user"), f.s("role"), s.user.actor());
    let res = st.db.call(move |c| store::set_user_role(c, &actor, &uid, &role)).await;
    Ok(match res {
        Ok(()) => redirect("/admin/users?m=role"),
        Err(e) => ui::error_page(axum::http::StatusCode::BAD_REQUEST, "Couldn't change role", &e.to_string()),
    })
}

async fn create_invite(State(st): State<AppState>, Member(s): Member, Form(pairs): Form<Vec<(String, String)>>) -> AppResult {
    let f = F::new(pairs);
    guard!(f.csrf(&s));
    guard!(s.require_owner());
    let role = f.s("role");
    let hours: i64 = f.s("hours").parse().unwrap_or(48).clamp(1, 24 * 14);
    let actor = s.user.actor();
    let code = st.db.call(move |c| store::create_invite(c, &actor, &role, hours)).await?;
    // The code travels in the redirect URL once; the page tells the owner it won't be shown again.
    Ok(redirect(&format!("/admin/users?code={code}&hours={hours}")))
}

// ---------------------------------------------------------------------------
// Settings (owners)

async fn settings(State(st): State<AppState>, Member(s): Member, Query(fl): Query<Flash>) -> AppResult {
    guard!(s.require_owner());
    let journal = st.journal.clone();
    let (phase, last_backup, backup_status, missing, journal_count) = st
        .db
        .call(move |c| {
            let missing = crate::rsvp::journal_missing(c, &journal)?;
            Ok((
                store::phase(c)?,
                store::setting(c, "last_backup_at")?,
                store::setting(c, "last_backup_status")?,
                missing.len(),
                journal.read_all()?.len(),
            ))
        })
        .await?;
    let cfg = &st.cfg;
    let body = html! {
        section.card {
            h2 { "Phase" }
            p.muted.small { "Controls what guests see at their link." }
            form method="post" action="/admin/settings" {
                (csrf_input(&s.csrf)) input type="hidden" name="action" value="phase";
                div.phase-options {
                    @for (p, desc) in [
                        (Phase::SaveTheDate, "Guests see the save the date, the date and location, and can add it to their calendar."),
                        (Phase::RsvpOpen, "Guests see full event details and the RSVP form (until the deadline)."),
                        (Phase::RsvpClosed, "Guests see their recorded answers but can't change them."),
                    ] {
                        label.phase-option { input type="radio" name="phase" value=(p.as_str()) checked[phase == p]; span { strong { (p.label()) } small { (desc) } } }
                    }
                }
                div.form-actions { button.button.primary type="submit" { "Update phase" } }
            }
        }
        div.grid2 {
            section.card {
                h2 { "Backups" }
                p { "Litestream streams every write to Cloud Storage continuously. A full snapshot is also taken daily at " (cfg.backup.hour_utc) ":00 UTC." }
                dl.kv {
                    dt { "Last snapshot" } dd { (when_opt(cfg, &last_backup)) }
                    dt { "Status" } dd { (backup_status.unwrap_or_else(|| "No backup yet".into())) }
                    dt { "Bucket" } dd { (cfg.backup.bucket.clone().unwrap_or_else(|| "local only".into())) }
                }
                form method="post" action="/admin/settings" {
                    (csrf_input(&s.csrf)) input type="hidden" name="action" value="backup";
                    button.button type="submit" { "Take a snapshot now" }
                }
            }
            section.card {
                h2 { "RSVP journal" }
                p { "Every RSVP is written to an append-only journal before the database. " (journal_count) " entries on disk." }
                @if missing == 0 { p.good { "✓ Every journal entry is in the database." } }
                @else {
                    p.bad { strong { (missing) " entries are missing from the database." } }
                    form method="post" action="/admin/settings" {
                        (csrf_input(&s.csrf)) input type="hidden" name="action" value="replay";
                        button.button.primary type="submit" { "Replay journal" }
                    }
                }
            }
        }
    };
    page(&st, &s, "settings", "Settings", &fl, body).await
}

async fn update_settings(State(st): State<AppState>, Member(s): Member, Form(pairs): Form<Vec<(String, String)>>) -> AppResult {
    let f = F::new(pairs);
    guard!(f.csrf(&s));
    guard!(s.require_owner());
    let actor = s.user.actor();
    match f.s("action").as_str() {
        "phase" => {
            let Some(p) = Phase::parse(&f.s("phase")) else { return Ok(redirect("/admin/settings?m=error")) };
            st.db.call(move |c| store::set_setting(c, &actor, "phase", p.as_str())).await?;
            Ok(redirect("/admin/settings?m=phase"))
        }
        "backup" => {
            crate::backup::run(&st.db, &st.cfg, &st.env).await?;
            Ok(redirect("/admin/settings?m=backup"))
        }
        "replay" => {
            let (journal, cfg) = (st.journal.clone(), st.cfg.clone());
            st.db
                .call(move |c| {
                    for e in crate::rsvp::journal_missing(c, &journal)? {
                        crate::rsvp::apply(c, &cfg, &e, false)?;
                    }
                    store::audit(c, &actor, "journal.replay", "", "", &json!({}))
                })
                .await?;
            Ok(redirect("/admin/settings?m=replayed"))
        }
        _ => Ok(redirect("/admin/settings?m=error")),
    }
}

// ---------------------------------------------------------------------------
// Account

async fn account(State(st): State<AppState>, Member(s): Member, Query(fl): Query<Flash>) -> AppResult {
    let uid = s.user.id.clone();
    let creds: Vec<(String, Option<String>)> = st
        .db
        .call(move |c| {
            let mut stmt = c.prepare("SELECT created_at, last_used_at FROM credentials WHERE user_id = ?1 ORDER BY created_at")?;
            let rows = stmt.query_map([uid], |r| Ok((r.get(0)?, r.get(1)?)))?.collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(rows)
        })
        .await?;
    let cfg = &st.cfg;
    let body = html! {
        section.card {
            h2 { "Your passkeys" }
            p.muted.small { "Add a passkey on each device you use (or rely on one synced by your password manager). If you lose them all, an owner can invite you again." }
            ul.feed { @for (i, (created, used)) in creds.iter().enumerate() { li { "Passkey " (i + 1) span.muted.small { " · added " (when(cfg, created)) " · last used " (when_opt(cfg, used)) } } } }
            button.button.primary #passkey-add type="button" { "Add a passkey on this device" }
            p.error #auth-error role="alert" hidden {}
        }
        section.card {
            h2 { "Sessions" }
            form method="post" action="/admin/account/signout-all" {
                (csrf_input(&s.csrf))
                button.button.ghost type="submit" { "Sign out everywhere else" }
            }
        }
        script src=(ui::asset("passkey.js")) defer {}
    };
    page(&st, &s, "account", "Account", &fl, body).await
}

async fn signout_all(State(st): State<AppState>, Member(s): Member, headers: axum::http::HeaderMap, Form(pairs): Form<Vec<(String, String)>>) -> AppResult {
    let f = F::new(pairs);
    guard!(f.csrf(&s));
    let current = headers
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(';'))
        .find_map(|kv| kv.trim().strip_prefix("__Host-rsvp_session="))
        .map(crate::tokens::sha256_hex)
        .unwrap_or_default();
    let uid = s.user.id.clone();
    st.db.call(move |c| Ok(c.execute("DELETE FROM sessions WHERE user_id = ?1 AND id_hash <> ?2", [uid, current])?)).await?;
    Ok(redirect("/admin/account?m=signedout"))
}

// ---------------------------------------------------------------------------
// Activity

#[derive(Deserialize, Default)]
struct AuditQuery {
    actor: Option<String>,
}

async fn audit(State(st): State<AppState>, Member(s): Member, Query(q): Query<AuditQuery>) -> AppResult {
    let rows = st.db.call(|c| store::audit_log(c, 500, None)).await?;
    let cfg = &st.cfg;
    let actors: std::collections::BTreeSet<&str> = rows.iter().map(|r| r.actor.split(" (").next().unwrap_or(&r.actor)).collect();
    let filtered: Vec<_> = rows.iter().filter(|r| q.actor.as_deref().is_none_or(|a| a.is_empty() || r.actor.starts_with(a))).collect();
    let link = |r: &store::AuditRow| -> Option<String> {
        match r.entity.as_str() {
            "household" => Some(format!("/admin/households/{}", r.entity_id)),
            _ => None,
        }
    };
    let body = html! {
        form.filters method="get" {
            select name="actor" aria-label="Who" {
                option value="" { "Everyone" }
                @for a in &actors { option value=(a) selected[q.actor.as_deref() == Some(*a)] { (a) } }
            }
            button.button type="submit" { "Filter" }
        }
        div.table-wrap {
            table.list {
                thead { tr { th { "When" } th { "Who" } th { "What" } th { "Detail" } } }
                tbody {
                    @for r in &filtered {
                        tr {
                            td.small.muted.nowrap { (when(cfg, &r.at)) }
                            td.small { (r.actor) }
                            td.small { @if let Some(l) = link(r) { a href=(l) { (r.action) " " (r.entity_id) } } @else { (r.action) } }
                            td.small.muted.detail { (if r.detail.len() > 200 { format!("{}…", &r.detail[..r.detail.char_indices().nth(200).map(|(i, _)| i).unwrap_or(r.detail.len())]) } else { r.detail.clone() }) }
                        }
                    }
                }
            }
        }
    };
    page(&st, &s, "audit", "Activity", &Flash::default(), body).await
}

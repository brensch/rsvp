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
        .route("/admin/households/{id}/quick", post(quick_update))
        .route("/admin/households/{id}/request", post(review_request))
        .route("/admin/households/{id}/archive", post(archive_household))
        .route("/admin/households/{id}/rotate", post(rotate_link))
        .route("/admin/households/{id}/send", post(send_one))
        .route("/admin/guests", get(guests))
        .route("/admin/website", get(website).post(create_page))
        .route("/admin/website/{slug}", get(edit_page).post(save_page))
        .route("/admin/website/{slug}/delete", post(delete_page))
        .route("/admin/gallery", get(gallery))
        .route("/admin/gallery/upload", post(gallery_upload).layer(axum::extract::DefaultBodyLimit::max(150 * 1024 * 1024)))
        .route("/admin/gallery/{id}", post(gallery_photo))
        .route("/admin/stationery", get(stationery))
        .route("/admin/stationery/{file}", get(stationery_file))
        .route("/admin/send", get(send_page).post(send_bulk))
        .route("/admin/send/test", post(send_test))
        .route("/admin/gmail/connect", get(gmail_connect))
        .route("/admin/gmail/callback", get(gmail_return))
        .route("/admin/gmail/finish", get(gmail_callback))
        .route("/admin/gmail/disconnect", post(gmail_disconnect))
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
        "uploaded" => "Photos added.",
        "requested" => "Thanks! Your request was sent to the couple to approve.",
        "approved" => "Approved and added to the guest list.",
        "declined" => "Request declined.",
        "owners-only" => "Only the couple can add people to an existing household.",
        "page-saved" => "Page saved.",
        "page-deleted" => "Page deleted.",
        "error" => "Something about that didn't work. Check the values and try again.",
        "gmail-connected" => "Gmail connected. Emails will be sent from it.",
        "gmail-disconnected" => "Gmail disconnected. Nothing can be emailed until it's connected again.",
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
        name: None,
        greeting: None,
        envelope: None,
        email: None,
        addr1: f.get("addr1"),
        addr2: f.get("addr2"),
        city: f.get("city"),
        region: f.get("region"),
        postcode: f.get("postcode"),
        country: f.get("country"),
        side: f.get("side"),
        tier: f.get("tier").and_then(|t| t.parse().ok()),
        australia: f.check("australia_present").then(|| f.check("australia")),
        unlikely: f.check("unlikely_present").then(|| f.check("unlikely")),
        tags: f.get("tags"),
        events: Some(f.all("events").into_iter().filter(|e| cfg.event(e).is_some()).collect()),
        notes: f.get("notes"),
        guests: vec![],
        added_by: None,
    }
}

fn guest_input_from(f: &F, prefix: &str) -> GuestInput {
    let g = |k: &str| f.get(&format!("{prefix}{k}"));
    GuestInput {
        first_name: g("first_name"),
        last_name: g("last_name"),
        email: g("email"),
        is_child: Some(f.check(&format!("{prefix}is_child"))),
        plus_one_label: None,
        dietary: g("dietary"),
        notes: g("notes"),
        sort: None,
    }
}

/// "Ben's plus one" (then "Ben's second plus one", ...): named after the one person in the
/// household, else the household. `n` is which plus one this is, from 1.
fn plus_one_label(h: &Household, guests: &[Guest], n: usize) -> String {
    let people: Vec<String> = guests.iter().filter(|g| g.plus_one_of.is_none() && !g.is_plus_one()).map(|g| g.first_name.clone()).collect();
    plus_one_name(&people, &h.name, n)
}

fn plus_one_name(first_names: &[String], household: &str, n: usize) -> String {
    let host = match first_names {
        [one] => one.clone(),
        _ if !household.trim().is_empty() => household.trim().to_string(),
        [] => "Their".to_string(),
        [init @ .., last] => format!("{} & {}", init.join(", "), last),
    };
    const NTH: [&str; 9] = ["", "second ", "third ", "fourth ", "fifth ", "sixth ", "seventh ", "eighth ", "ninth "];
    match NTH.get(n.saturating_sub(1)) {
        Some(nth) => format!("{host}'s {nth}plus one"),
        None => format!("{host}'s plus one {n}"),
    }
}

/// Prefixes of the rows added with "Add person" / "Add a plus one" (`new0.`, `new1.`, ...), in order.
fn new_rows(f: &F) -> Vec<String> {
    let mut out: Vec<String> = vec![];
    for (k, _) in &f.1 {
        let Some(rest) = k.strip_prefix("new") else { continue };
        let Some((i, _)) = rest.split_once('.') else { continue };
        if i.parse::<u16>().is_ok() {
            let p = format!("new{i}.");
            if !out.contains(&p) {
                out.push(p);
            }
        }
    }
    out
}

/// A new person row, or None if it was left blank. Plus-one rows may have no name.
fn new_row_input(f: &F, p: &str) -> Option<(GuestInput, bool)> {
    let gi = guest_input_from(f, p);
    let slot = f.check(&format!("{p}plus_one"));
    (slot || gi.first_name.as_deref().is_some_and(|n| !n.is_empty())).then_some((gi, slot))
}

/// "Add person" and "Add a plus one" buttons. admin.js clones the rows from the `<template>`s
/// (named `new{i}.`); `start` blank person rows are shown up front. `plus_one` = the label to
/// show, or None to leave the plus-one button out.
fn add_people(plus_one: Option<&str>, start: usize) -> Markup {
    html! {
        div.new-people.sortable data-next=(start) {
            @for i in 0..start { (new_person_row(&format!("new{i}."))) }
        }
        template #tpl-person { (new_person_row("new__I__.")) }
        @if let Some(label) = plus_one {
            template #tpl-plus-one {
                div.guest-row.add.plus-one-row {
                    (drag_handle())
                    input type="hidden" name="new__I__.plus_one" value="1";
                    p.span-all { span.tag { "+1" } " " (label) ". Add their name if you know it; otherwise they'll tell us on their RSVP." }
                    input name="new__I__.first_name" placeholder="First (if known)" aria-label="First name" maxlength="80";
                    input name="new__I__.last_name" placeholder="Last" aria-label="Last name" maxlength="80";
                    button.link-button type="button" data-remove-row { "Remove" }
                }
            }
        }
        div.add-buttons {
            button.button type="button" data-add="tpl-person" { "+ Add person" }
            @if plus_one.is_some() { button.button type="button" data-add="tpl-plus-one" { "+ Add a plus one" } }
        }
    }
}

/// Grip for reordering people (admin.js: drag, or arrow keys when focused).
fn drag_handle() -> Markup {
    html! { button.drag-handle type="button" data-drag aria-label="Drag to reorder, or use the arrow keys" title="Drag to reorder" { "⠿" } }
}

fn new_person_row(p: &str) -> Markup {
    html! {
        div.guest-row.add {
            (drag_handle())
            (guest_fields(p, None))
            input name={ (p) "dietary" } placeholder="Dietary" aria-label="Dietary" maxlength="500";
            input name={ (p) "notes" } placeholder="Notes" aria-label="Notes" maxlength="2000";
            button.link-button type="button" data-remove-row { "Remove" }
        }
    }
}

/// How a team member is shown as "added by": "Sam", "Jo (Alex's parent)", "Kim (helper)".
fn person_label(cfg: &Config, u: &store::User) -> String {
    match u.access() {
        "owner" => u.name.clone(),
        "a_parent" => format!("{} ({}'s parent)", u.name, cfg.couple.a_first),
        "b_parent" => format!("{} ({}'s parent)", u.name, cfg.couple.b_first),
        _ => format!("{} (helper)", u.name),
    }
}

fn access_label(cfg: &Config, access: &str) -> String {
    match access {
        "owner" => "Owner".into(),
        "a_parent" => format!("{}'s parent", cfg.couple.a_first),
        "b_parent" => format!("{}'s parent", cfg.couple.b_first),
        "helper" => "Helper".into(),
        other => other.to_string(),
    }
}

/// Apply a household/guest change as the signed-in user. Returns (flash code, created id).
/// Owners add households directly; anyone else's new household becomes a request.
async fn apply_change(
    st: &AppState,
    s: &Session,
    kind: &'static str,
    target: Option<i64>,
    payload: serde_json::Value,
) -> anyhow::Result<(&'static str, Option<i64>)> {
    let user = s.user.clone();
    let label = person_label(&st.cfg, &user);
    let (secret, defaults, valid) = (st.env.secret.clone(), default_events(&st.cfg), event_slugs(&st.cfg));
    st.db
        .call(move |c| {
            let actor = user.actor();
            let created = match kind {
                "household.create" => {
                    let mut input: HouseholdInput = serde_json::from_value(payload)?;
                    input.validate(&valid)?;
                    input.added_by = None;
                    let request = !user.is_owner();
                    let id = store::create_household_as(c, &actor, &label, request, &secret, &input, &defaults)?;
                    if request {
                        let h = store::must_household(c, id)?;
                        store::enqueue(c, "discord", &json!({"type": "alert", "text": format!("📝 **{label}** asked to add **{}** to the guest list. Approve or decline it on the Households page.", h.name)}))?;
                        return Ok(("requested", Some(id)));
                    }
                    Some(id)
                }
                "household.update" => {
                    let input: HouseholdInput = serde_json::from_value(payload)?;
                    input.validate(&valid)?;
                    store::update_household(c, &actor, target.unwrap(), &input)?;
                    None
                }
                "guest.create" => {
                    if !user.is_owner() {
                        return Err(anyhow!("only the couple can add people to an existing household"));
                    }
                    Some(store::create_guest(c, &actor, target.unwrap(), &serde_json::from_value(payload)?)?)
                }
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
    /// Emails sent/opened per (household, kind).
    mail: HashMap<(i64, String), store::MailStat>,
    /// When each household last added its events to a calendar.
    calendar: HashMap<i64, String>,
}

impl Snapshot {
    async fn load(st: &AppState) -> anyhow::Result<Self> {
        st.db
            .call(|c| {
                Ok(Snapshot {
                    households: store::list_households(c, false)?,
                    guests: store::all_guests(c)?,
                    responses: store::all_responses(c)?,
                    mail: store::mail_stats(c)?,
                    calendar: store::calendar_last_downloads(c)?,
                })
            })
            .await
    }
    fn mail(&self, hid: i64, kind: &str) -> Option<&store::MailStat> {
        self.mail.get(&(hid, kind.to_string()))
    }
    fn guests_of(&self, hid: i64) -> Vec<Guest> {
        self.guests.iter().filter(|g| g.household_id == hid).cloned().collect()
    }
    fn status(&self, h: &Household) -> Status {
        store::household_status(h, &self.guests_of(h.id), &self.responses)
    }
}

/// Bar widths come from CSS classes (`w0` … `w100` in 5% steps): the CSP forbids inline styles.
fn pct_class(pct: usize) -> String {
    let p = pct.min(100);
    // Round to the nearest 5, but never show an empty bar for a non-zero value.
    let step = if p > 0 && p < 5 { 5 } else { (p + 2) / 5 * 5 };
    format!("w{step}")
}

// ---------------------------------------------------------------------------
// Dashboard

async fn dashboard(State(st): State<AppState>, Member(s): Member, Query(fl): Query<Flash>) -> AppResult {
    let snap = Snapshot::load(&st).await?;
    let (phase, recent, outbox_failed, last_backup, missing_journal, pending_requests) = {
        let journal = st.journal.clone();
        st.db
            .call(move |c| {
                let failed: i64 = c.query_row("SELECT COUNT(*) FROM outbox WHERE failed_at IS NOT NULL", [], |r| r.get(0))?;
                let missing = crate::rsvp::journal_missing(c, &journal)?.len();
                let requests = store::pending_requests(c)?.len();
                Ok((store::phase(c)?, store::submissions(c, None, 8)?, failed, store::setting(c, "last_backup_at")?, missing, requests))
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

    // Per-tier progress. Attendance is for the main event ("wedding", else the first event).
    let main_ev = cfg.event("wedding").or(cfg.events.first());
    struct TierRow {
        label: String,
        href: String,
        households: usize,
        people: usize,
        /// People in households not marked unlikely to come.
        likely: usize,
        likely_href: String,
        address: usize,
        checked: usize,
        opened: usize,
        replied: usize,
        yes: usize,
        no: usize,
        waiting: usize,
    }
    let tier_row = |tier: Option<i64>| -> TierRow {
        let hs: Vec<&Household> = snap.households.iter().filter(|h| tier.is_none_or(|t| h.tier == t)).collect();
        let mut r = TierRow {
            label: tier.map(|t| format!("Tier {t}")).unwrap_or_else(|| "All tiers".into()),
            href: tier.map(|t| format!("/admin/households?tier={t}")).unwrap_or_else(|| "/admin/households".into()),
            households: hs.len(),
            people: 0,
            likely: 0,
            likely_href: tier.map(|t| format!("/admin/households?tier={t}&filter=likely")).unwrap_or_else(|| "/admin/households?filter=likely".into()),
            address: hs.iter().filter(|h| !h.address_lines().is_empty()).count(),
            checked: hs.iter().filter(|h| h.checked_at.is_some()).count(),
            opened: hs.iter().filter(|h| h.first_viewed_at.is_some()).count(),
            replied: hs.iter().filter(|h| h.rsvp_at.is_some()).count(),
            yes: 0,
            no: 0,
            waiting: 0,
        };
        for h in &hs {
            for g in snap.guests_of(h.id) {
                if g.plus_one_of.is_none() {
                    r.people += 1;
                    if !h.unlikely {
                        r.likely += 1;
                    }
                }
                let Some(e) = main_ev else { continue };
                if !(g.plus_one_of.is_some() || g.invited(h, &e.slug)) {
                    continue;
                }
                match snap.responses.get(&(g.id, e.slug.clone())) {
                    Some(resp) if resp.attending => r.yes += 1,
                    Some(_) => r.no += 1,
                    None if g.plus_one_of.is_none() => r.waiting += 1,
                    None => {}
                }
            }
        }
        r
    };
    let tier_rows: Vec<TierRow> = vec![tier_row(Some(1)), tier_row(Some(2)), tier_row(Some(3)), tier_row(None)];
    let progress = |n: usize, of: usize| -> Markup {
        let pct = if of > 0 { n * 100 / of } else { 0 };
        html! {
            div.prog {
                span.prog-nums { strong { (n) } span.muted { " / " (of) } }
                span.prog-pct { (pct) "%" }
                div.prog-bar { span class=(pct_class(pct)) {} }
            }
        }
    };

    let body = html! {
        @if pending_requests > 0 {
            div.alert.warn { strong { (pending_requests) " household request(s)" } " waiting for approval. " a href="/admin/households" { "Review them" } }
        }
        @if missing_journal > 0 {
            div.alert.bad { strong { (missing_journal) " RSVP(s) are in the journal but missing from the database." } " Go to " a href="/admin/settings" { "Settings" } " and replay the journal." }
        }
        @if outbox_failed > 0 {
            div.alert.warn { (outbox_failed) " notification(s) or email(s) failed permanently. " a href="/admin/send" { "See details" } }
        }
        div.tiles {
            div.tile { span.tile-label { "Phase" } span.tile-value.small { (phase.label()) } span.tile-sub { "RSVP by " (cfg.wedding.rsvp_deadline) } }
            div.tile {
                span.tile-label { "Households" } span.tile-value { (total) }
                span.tile-sub { (people) " people across all tiers" }
                @let unlikely_hh: Vec<&Household> = snap.households.iter().filter(|h| h.unlikely).collect();
                @let unlikely_people = unlikely_hh.iter().map(|h| snap.guests_of(h.id).iter().filter(|g| g.plus_one_of.is_none()).count()).sum::<usize>();
                span.tile-sub {
                    a href="/admin/households?filter=likely" { strong { (people.saturating_sub(unlikely_people)) } " likely to come" }
                    " · " (unlikely_people) " marked unlikely"
                }
            }
            div.tile {
                span.tile-label { "Replied" } span.tile-value { (replied) small { " / " (total) } }
                div.meter { span class=(pct_class(pct)) {} }
                span.tile-sub { (viewed_no_reply) " opened their link but haven't replied" }
            }
            div.tile {
                @let added = snap.households.iter().filter(|h| snap.calendar.contains_key(&h.id)).count();
                span.tile-label { "Added to calendar" } span.tile-value { (added) small { " / " (total) } }
                div.meter { span class=(pct_class(if total > 0 { added * 100 / total } else { 0 })) {} }
                span.tile-sub { a href="/admin/households?filter=calendar" { "See who" } " · households that downloaded their calendar file" }
            }
        }
        section.card.by-tier {
            div.card-head {
                h2 { "By tier" }
                span.muted.small { "Tier 1 is invited first; tiers 2 and 3 as numbers allow. Click a row to see those households." }
            }
            div.table-wrap.flush {
                table.tier-table {
                    thead {
                        tr.group {
                            th {}
                            th colspan="3" { "Invited" }
                            th colspan="4" { "Progress (households)" }
                            th colspan="3" { (main_ev.map(|e| e.name.as_str()).unwrap_or("Attendance")) " (people)" }
                        }
                        tr {
                            th { "Tier" }
                            th.num { "Households" } th.num { "People" } th.num title="People in households not marked unlikely to come" { "Likely" }
                            th { "Address" } th { "Checked correct" } th { "Opened link" } th { "Replied" }
                            th.num { "Attending" } th.num { "Declined" } th.num { "Waiting" }
                        }
                    }
                    tbody {
                        @for (i, r) in tier_rows.iter().enumerate() {
                            tr.total[i == 3] data-href=(r.href) {
                                td { a.tier-name href=(r.href) { @if i < 3 { span class={ "tier-dot t" (i + 1) } { (i + 1) } } (r.label) } }
                                td.num.big { (r.households) }
                                td.num.big { (r.people) }
                                td.num.big { a href=(r.likely_href) { (r.likely) } }
                                td { (progress(r.address, r.households)) }
                                td { (progress(r.checked, r.households)) }
                                td { (progress(r.opened, r.households)) }
                                td { (progress(r.replied, r.households)) }
                                td.num.big.good { (r.yes) }
                                td.num.big { (r.no) }
                                td.num.big.muted { (r.waiting) }
                            }
                        }
                    }
                }
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
    added: Option<String>,
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

/// S / I dot: grey = not sent, green = sent, gold = opened.
fn mail_dot(cfg: &Config, letter: &str, what: &str, sent_at: Option<&str>, stat: Option<&store::MailStat>) -> Markup {
    let opened = stat.and_then(|m| m.first_opened_at.as_deref());
    let title = match (sent_at, opened) {
        (None, _) => format!("{what} not sent"),
        (Some(s), None) => format!("{what} sent {}, not opened yet", when(cfg, s)),
        (Some(s), Some(o)) => format!("{what} sent {}, opened {}", when(cfg, s), when(cfg, o)),
    };
    let class = match (sent_at, opened) {
        (None, _) => "dot",
        (Some(_), None) => "dot on",
        (Some(_), Some(_)) => "dot on opened",
    };
    html! { span title=(title) class=(class) { (letter) } }
}

/// "Unlikely to come" switch.
fn unlikely_toggle(csrf: &str, h: &Household, back: &str) -> Markup {
    html! {
        form.chips-toggle.inline-save method="post" action={ "/admin/households/" (h.id) "/quick" } data-inline {
            (csrf_input(csrf)) input type="hidden" name="back" value=(back);
            button.unlikely type="submit" name="toggle_unlikely" value="1" aria-pressed=(if h.unlikely { "true" } else { "false" })
                title="Click to switch between likely and unlikely to come" { @if h.unlikely { "Unlikely" } @else { "Likely" } }
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
    let requests: Vec<(Household, Vec<Guest>)> = st
        .db
        .call(|c| {
            let mut out = vec![];
            for h in store::pending_requests(c)? {
                let g = store::guests_for(c, h.id)?;
                out.push((h, g));
            }
            Ok(out)
        })
        .await?;
    let added_by_values: std::collections::BTreeSet<String> =
        snap.households.iter().map(|h| h.added_by.clone()).filter(|a| !a.is_empty()).collect();
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
            let added_ok = q.added.as_deref().is_none_or(|a| a.is_empty() || h.added_by == a);
            let filter_ok = match q.filter.as_deref() {
                Some("no_email") => h.email.is_empty() && gs.iter().all(|g| g.email.is_empty()),
                Some("no_address") => h.address_lines().is_empty(),
                Some("no_events") => h.events.is_empty(),
                Some("australia") => h.australia,
                Some("unchecked") => h.checked_at.is_none(),
                Some("checked") => h.checked_at.is_some(),
                Some("not_australia") => !h.australia,
                Some("unlikely") => h.unlikely,
                Some("likely") => !h.unlikely,
                Some("std_unsent") => h.std_sent_at.is_none(),
                Some("invite_unsent") => h.invite_sent_at.is_none(),
                Some("std_opened") => snap.mail(h.id, "save_the_date").is_some_and(|m| m.opened > 0),
                Some("invite_opened_no_reply") => h.rsvp_at.is_none() && snap.mail(h.id, "invitation").is_some_and(|m| m.opened > 0),
                Some("nudged") => h.nudged_at.is_some() && h.rsvp_at.is_none(),
                Some("calendar") => snap.calendar.contains_key(&h.id),
                Some("no_calendar") => !snap.calendar.contains_key(&h.id),
                _ => true,
            };
            text_ok && status_ok && side_ok && tier_ok && added_ok && filter_ok
        })
        .collect();
    let people: usize = rows.iter().map(|(_, g, _)| g.len()).sum();
    let sel = |a: &Option<String>, v: &str| a.as_deref() == Some(v);
    // Keep the current filters when an inline form falls back to a full POST + redirect.
    let back = {
        let mut parts = vec![];
        for (k, v) in [("q", &q.q), ("tier", &q.tier), ("status", &q.status), ("side", &q.side), ("filter", &q.filter), ("added", &q.added)] {
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
                    option value="unlikely" selected[sel(&q.filter, "unlikely")] { "Unlikely to come" }
                    option value="likely" selected[sel(&q.filter, "likely")] { "Not marked unlikely" }
                    option value="no_events" selected[sel(&q.filter, "no_events")] { "Not invited to anything" }
                    option value="no_email" selected[sel(&q.filter, "no_email")] { "Missing email" }
                    option value="no_address" selected[sel(&q.filter, "no_address")] { "Missing address" }
                    option value="std_unsent" selected[sel(&q.filter, "std_unsent")] { "Save the date not sent" }
                    option value="invite_unsent" selected[sel(&q.filter, "invite_unsent")] { "Invitation not sent" }
                    option value="std_opened" selected[sel(&q.filter, "std_opened")] { "Opened save the date email" }
                    option value="invite_opened_no_reply" selected[sel(&q.filter, "invite_opened_no_reply")] { "Opened invitation, no reply" }
                    option value="nudged" selected[sel(&q.filter, "nudged")] { "No reply 2+ days after opening invitation" }
                    option value="calendar" selected[sel(&q.filter, "calendar")] { "Added to calendar" }
                    option value="no_calendar" selected[sel(&q.filter, "no_calendar")] { "Not added to calendar" }
                }
                select name="added" aria-label="Added by" {
                    option value="" { "Added by anyone" }
                    @for a in &added_by_values { option value=(a) selected[q.added.as_deref() == Some(a.as_str())] { "Added by " (a) } }
                }
                noscript { button.button type="submit" { "Filter" } }
            }
            a.button.primary href="/admin/households/new" { @if s.user.is_owner() { "New household" } @else { "Request a household" } }
        }
        @if !requests.is_empty() {
            section.card.requests {
                h2 { "Waiting for approval" span.muted.small { " · " (requests.len()) } }
                p.muted.small { "Households requested by family and helpers. They don't get a link and aren't counted, sent to or printed until approved." }
                @for (h, gs) in &requests {
                    div.request-row {
                        div {
                            a.strong href={ "/admin/households/" (h.id) } { (h.name) }
                            span class={ "side side-" (h.side) } { (side_short(cfg, &h.side)) }
                            div.small.muted {
                                (gs.iter().map(|g| g.full_name()).collect::<Vec<_>>().join(" · "))
                                " · requested by " strong { (h.added_by) } ", " (when(cfg, &h.created_at))
                            }
                            @if !h.notes.is_empty() { div.small { (h.notes) } }
                        }
                        @if s.user.is_owner() {
                            form.inline-actions method="post" action={ "/admin/households/" (h.id) "/request" } {
                                (csrf_input(&s.csrf))
                                button.button.small.primary type="submit" name="decision" value="approve" { "Approve" }
                                button.button.small.ghost type="submit" name="decision" value="decline" data-confirm={ "Decline the request for " (h.name) "?" } { "Decline" }
                            }
                        } @else {
                            span.badge.warn { "Waiting for the couple" }
                        }
                    }
                }
            }
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
            @let unlikely: Vec<_> = snap.households.iter().filter(|h| h.unlikely).collect();
            @let unlikely_people = snap.guests.iter().filter(|g| unlikely.iter().any(|h| h.id == g.household_id)).count();
            a.tier-pill.active[sel(&q.filter, "unlikely")] href="/admin/households?filter=unlikely" { "Unlikely " strong { (unlikely_people) } span.muted { " people · " (unlikely.len()) " hh" } }
        }
        div.table-wrap {
            table.list.households {
                thead { tr {
                    th { "Household" }
                    th.num { "#" }
                    th { "Tier" }
                    th title="Lives in Australia" { "AU" }
                    th title="Unlikely to come" { "Likely?" }
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
                                    @if !h.added_by.is_empty() { span.added-by title="Added by" { "added by " (h.added_by) } }
                                }
                                @let solo = gs.len() == 1 && gs[0].full_name() == h.name;
                                @if !solo { div.hh-people {
                                    @for (i, g) in gs.iter().enumerate() {
                                        @if i > 0 { span.sep { " · " } }
                                        span.person.muted[g.plus_one_of.is_some() || g.unnamed_plus_one()] {
                                            (g.full_name())
                                            @if g.last_name.trim().is_empty() && g.plus_one_of.is_none() && !g.unnamed_plus_one() { small.warn-text { " no surname" } }
                                            @if g.is_child { small { " child" } }
                                            @if g.is_plus_one() && !g.unnamed_plus_one() { small { " +1" } }
                                        }
                                    }
                                    @if gs.is_empty() { span.muted { "No people yet" } }
                                } }
                            }
                            td.num { (gs.len()) }
                            td { (tier_toggle(&s.csrf, h, &back)) }
                            td { (australia_toggle(&s.csrf, h, &back)) }
                            td { (unlikely_toggle(&s.csrf, h, &back)) }
                            td.events-cell { (event_toggles(cfg, &s.csrf, h, &back)) }
                            td { (checked_toggle(cfg, &s.csrf, h, &back)) }
                            td.reply {
                                (status_badge(*status))
                                div.reply-meta {
                                    (mail_dot(cfg, "S", "Save the date", h.std_sent_at.as_deref(), snap.mail(h.id, "save_the_date")))
                                    (mail_dot(cfg, "I", "Invitation", h.invite_sent_at.as_deref(), snap.mail(h.id, "invitation")))
                                    @if let Some(v) = &h.first_viewed_at { span.small.muted title="First visited their page" { " visited " (when(cfg, v).split(',').next().unwrap_or("")) } }
                                    @if h.nudged_at.is_some() && h.rsvp_at.is_none() { span.badge.warn title="Opened their invitation 2+ days ago and hasn't replied" { "no reply 2d+" } }
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
    if f.get("toggle_unlikely").is_some() {
        input.unlikely = Some(!h.unlikely);
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
    if input.tier.is_none() && input.events.is_none() && input.australia.is_none() && input.unlikely.is_none() || input.validate(&event_slugs(&st.cfg)).is_err() {
        return Ok(if inline { (axum::http::StatusCode::BAD_REQUEST, axum::Json(json!({"error": "invalid change"}))).into_response() } else { redirect(&format!("{back}#h{id}")) });
    }
    let (code, _) = apply_change(&st, &s, "household.update", Some(id), serde_json::to_value(&input)?).await?;
    if inline {
        let h = st.db.call(move |c| store::must_household(c, id)).await?;
        return Ok(axum::Json(json!({"tier": h.tier, "events": h.events, "australia": h.australia, "unlikely": h.unlikely, "checked": h.checked_at.is_some()})).into_response());
    }
    let sep = if back.contains('?') { '&' } else { '?' };
    Ok(redirect(&format!("{back}{sep}m={code}#h{id}")))
}

fn household_fields(cfg: &Config, h: Option<&Household>) -> Markup {
    household_fields_with(cfg, h, "both")
}

fn household_fields_with(cfg: &Config, h: Option<&Household>, default_side: &str) -> Markup {
    let v = |f: fn(&Household) -> &str| h.map(f).unwrap_or("").to_string();
    let events: Vec<String> = h.map(|h| h.events.clone()).unwrap_or_else(|| default_events(cfg));
    let side = h.map(|h| h.side.as_str()).unwrap_or(default_side);
    html! {
        div.form-grid {
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
            label.check.span2 {
                input type="hidden" name="unlikely_present" value="1";
                input type="checkbox" name="unlikely" checked[h.is_some_and(|h| h.unlikely)];
                "Unlikely to come " span.muted.small { "(only for our planning; guests never see this)" }
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

fn guest_fields(prefix: &str, g: Option<&Guest>) -> Markup {
    let n = |k: &str| format!("{prefix}{k}");
    html! {
        input name=(n("first_name")) value=[g.map(|g| g.first_name.as_str())] placeholder=(if g.is_some_and(|g| g.is_plus_one()) { "First (if known)" } else { "First" }) aria-label="First name" maxlength="80";
        input name=(n("last_name")) value=[g.map(|g| g.last_name.as_str())] placeholder="Last" aria-label="Last name" maxlength="80";
        input type="email" name=(n("email")) value=[g.map(|g| g.email.as_str())] placeholder="Email" aria-label="Email" maxlength="200";
        label.check.small { input type="checkbox" name=(n("is_child")) checked[g.is_some_and(|g| g.is_child)]; "Child" }
    }
}

async fn new_household(State(st): State<AppState>, Member(s): Member) -> AppResult {
    let owner = s.user.is_owner();
    let default_side = match s.user.access() {
        "a_parent" => "a",
        "b_parent" => "b",
        _ => "both",
    };
    let body = html! {
        form.card method="post" action="/admin/households/new" {
            (csrf_input(&s.csrf))
            @if !owner { p.alert { "This sends a request to the couple. They'll add the household to the list once they approve it." } }
            h2 { "People" }
            (add_people(Some("A plus one for this household"), 1))
            h2 { "Household" }
            (household_fields_with(&st.cfg, None, default_side))
            div.form-actions { button.button.primary type="submit" { @if owner { "Create household" } @else { "Send request" } } }
        }
    };
    page(&st, &s, "households", if owner { "New household" } else { "Request a household" }, &Flash::default(), body).await
}

async fn create_household(State(st): State<AppState>, Member(s): Member, Form(pairs): Form<Vec<(String, String)>>) -> AppResult {
    let f = F::new(pairs);
    guard!(f.csrf(&s));
    let mut input = household_input_from(&f, &st.cfg);
    for p in new_rows(&f) {
        let Some((mut gi, slot)) = new_row_input(&f, &p) else { continue };
        if slot {
            gi.plus_one_label = Some(String::new()); // named below, once we know who's in the household
        }
        input.guests.push(gi);
    }
    let people: Vec<String> = input.guests.iter().filter(|g| g.plus_one_label.is_none()).filter_map(|g| g.first_name.clone()).collect();
    let household = input.name.as_deref().or(input.greeting.as_deref()).unwrap_or("").to_string();
    for (n, g) in input.guests.iter_mut().filter(|g| g.plus_one_label.is_some()).enumerate() {
        g.plus_one_label = Some(plus_one_name(&people, &household, n + 1));
    }
    if input.guests.iter().all(|g| g.plus_one_label.is_some()) {
        return Ok(ui::error_page(axum::http::StatusCode::BAD_REQUEST, "Check the form", "Add at least one person (not just a plus one)."));
    }
    if let Err(e) = input.validate(&event_slugs(&st.cfg)) {
        return Ok(ui::error_page(axum::http::StatusCode::BAD_REQUEST, "Check the form", &e.to_string()));
    }
    let (code, created) = apply_change(&st, &s, "household.create", None, serde_json::to_value(&input)?).await?;
    Ok(match created {
        Some(_) if code == "requested" => redirect("/admin/households?m=requested"),
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
                store::sends_for(c, id)?,
                store::calendar_downloads(c, id)?,
            )))
        })
        .await?;
    let Some((h, guests, responses, subs, log, sends, cal)) = data else { return Ok(ui::not_found()) };
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
        @if h.request_status == "pending" {
            div.alert.warn {
                strong { "Waiting for approval. " } "Requested by " (h.added_by) ". Until it's approved this household has no working link and isn't counted, sent to or printed."
                @if owner {
                    form.inline-actions method="post" action={ "/admin/households/" (h.id) "/request" } {
                        (csrf_input(&s.csrf))
                        button.button.small.primary type="submit" name="decision" value="approve" { "Approve" }
                        button.button.small.ghost type="submit" name="decision" value="decline" data-confirm="Decline this request?" { "Decline" }
                    }
                }
            }
        }
        @if h.archived_at.is_some() { div.alert.warn { "This household is archived: their link no longer works and they're excluded from lists." } }
        div.head-row {
            div {
                p.muted.small { (ui::side_label(cfg, &h.side)) " · tier " (h.tier) @if h.australia { " · lives in Australia" } @if h.unlikely { " · " span.badge.warn { "unlikely to come" } } @for t in h.tag_list() { " " span.tag { (t) } } }
                form.chips-toggle.check-line method="post" action={ "/admin/households/" (h.id) "/quick" } {
                    (csrf_input(&s.csrf)) input type="hidden" name="back" value={ "/admin/households/" (h.id) };
                    button.ok type="submit" name="toggle_checked" value="1" aria-pressed=(if h.checked_at.is_some() { "true" } else { "false" }) {
                        @if h.checked_at.is_some() { "Names and details checked" } @else { "Mark names and details as correct" }
                    }
                    @if let (Some(by), Some(at)) = (&h.checked_by, &h.checked_at) { span.muted.small { " by " (by) ", " (when(cfg, at)) } }
                }
                p.lead { "“Dear " (h.greeting) "”" }
                @if !h.added_by.is_empty() { p.muted.small { "Added by " strong { (h.added_by) } " · " (when(cfg, &h.created_at)) } }
            }
            (status_badge(status))
        }
        div.grid2.wide-left {
            div {
                form.card.household-form method="post" action={ "/admin/households/" (h.id) } {
                    (csrf_input(&s.csrf))
                    h2 { "People" }
                    p.muted.small { "Drag " span.drag-glyph { "⠿" } " to reorder. The household's name follows this order (adults first, then children)." }
                    div.sortable {
                    @for g in &guests {
                        @let p = format!("g{}.", g.id);
                        div.guest-row {
                            (drag_handle())
                            input type="hidden" name="guest_ids" value=(g.id);
                            (guest_fields(&p, Some(g)))
                            input name={ (p) "dietary" } value=(g.dietary) placeholder="Dietary" aria-label="Dietary" maxlength="500";
                            input name={ (p) "notes" } value=(g.notes) placeholder="Notes" aria-label="Notes" maxlength="2000";
                            label.check.small.remove-person { input type="checkbox" name={ (p) "remove" }; "Remove" }
                            @if let Some(host) = g.plus_one_of { p.muted.small.span-all { "Plus-one of " (label(host)) " (added by the guest)" } }
                            @if g.is_plus_one() {
                                p.muted.small.span-all {
                                    span.tag { "+1" } " " (g.plus_one_label)
                                    @if g.unnamed_plus_one() { ". They'll tell us who on their RSVP." }
                                }
                            }
                        }
                    }
                    }
                    @if owner {
                        (add_people(Some("A plus one"), 0))
                    } @else {
                        p.muted.small { "To add someone new, " a href="/admin/households/new" { "request a household" } "." }
                    }
                    h2.form-section { "Household" }
                    (household_fields(cfg, Some(&h)))
                    div.form-actions.save-bar { button.button.primary type="submit" { "Save" } }
                }
                section.card {
                    p.muted.small {
                        "Calendar: "
                        @match &cal.last_at {
                            Some(at) => { "added " (when(cfg, at)) @if cal.count > 1 { " (" (cal.count) " downloads)" } }
                            None => { "not added yet" }
                        }
                    }
                    h2 { "Emails" }
                    @if sends.is_empty() { p.muted { "No emails sent yet." } } @else {
                        table.compact {
                            thead { tr { th { "Email" } th { "To" } th { "Sent" } th { "Opened" } } }
                            tbody {
                                @for m in &sends {
                                    tr {
                                        td { (if m.kind == "invitation" { "Invitation" } else { "Save the date" }) @if m.test { " " span.tag { "test" } } }
                                        td.small { (m.address) }
                                        td.small { @match &m.sent_at { Some(t) => { (when(cfg, t)) } None => { span.muted { "queued" } } } }
                                        td.small {
                                            @match &m.opened_at {
                                                Some(t) => { span.good { (when(cfg, t)) } @if m.opens > 1 { span.muted { " · " (m.opens) " times" } } }
                                                None => { span.muted { "not yet" } }
                                            }
                                        }
                                    }
                                }
                            }
                        }
                        p.muted.small { "\"Opened\" means the card picture loaded. Some mail apps (iPhone Mail especially) load pictures automatically, and some block them, so treat it as a hint. Visiting their page is the sure sign." }
                    }
                    @if h.nudged_at.is_some() && h.rsvp_at.is_none() { p.alert.warn { "Opened their invitation over 2 days ago and hasn't replied (Discord was pinged " (when_opt(cfg, &h.nudged_at)) ")." } }
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
            }
            div {
                section.card.link-card {
                    h2 { "Their link" }
                    div.qr { (PreEscaped(qr)) }
                    div.copy-row { input #guest-url readonly value=(url) aria-label="Guest link"; button.button.small type="button" data-copy="#guest-url" { "Copy" } }
                    p.small { a href=(url) target="_blank" rel="noopener noreferrer" { "Open their page ↗" } }
                    h3 { "Stationery" }
                    ul.downloads {
                        @for k in [Kind::Invitation, Kind::Envelope] {
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

/// Save the whole household page at once: details, every person, removals and new people.
async fn update_household(State(st): State<AppState>, Member(s): Member, Path(id): Path<i64>, Form(pairs): Form<Vec<(String, String)>>) -> AppResult {
    let f = F::new(pairs);
    guard!(f.csrf(&s));
    let bad = |msg: String| Ok(ui::error_page(axum::http::StatusCode::BAD_REQUEST, "Check the form", &msg));
    let input = household_input_from(&f, &st.cfg);
    if let Err(e) = input.validate(&event_slugs(&st.cfg)) {
        return bad(e.to_string());
    }
    let (h, current) = st.db.call(move |c| Ok((store::must_household(c, id)?, store::guests_for(c, id)?))).await?;

    let mut updates = vec![];
    let mut removals = vec![];
    // Rows arrive in the order they were dragged into; that's the new sort order.
    let mut pos = 0i64;
    for gid in f.all("guest_ids").iter().filter_map(|g| g.parse::<i64>().ok()) {
        let Some(g) = current.iter().find(|g| g.id == gid) else { return bad("That person isn't in this household any more; reload the page.".into()) };
        let p = format!("g{gid}.");
        if f.check(&format!("{p}remove")) {
            removals.push(gid);
            continue;
        }
        let gi = guest_input_from(&f, &p);
        if gi.first_name.as_deref().is_none_or(str::is_empty) && !g.is_plus_one() {
            return bad(format!("{} needs a first name.", g.full_name()));
        }
        if let Err(e) = gi.validate() {
            return bad(e.to_string());
        }
        let mut gi = gi;
        gi.sort = Some(pos);
        pos += 1;
        updates.push((gid, gi));
    }
    let mut additions = vec![];
    let kept: Vec<Guest> = current.iter().filter(|g| !removals.contains(&g.id)).cloned().collect();
    let mut slots = kept.iter().filter(|g| g.is_plus_one()).count();
    for p in new_rows(&f) {
        let Some((mut gi, slot)) = new_row_input(&f, &p) else { continue };
        if !s.user.is_owner() {
            return Ok(redirect(&format!("/admin/households/{id}?m=owners-only")));
        }
        if slot {
            slots += 1;
            gi.plus_one_label = Some(plus_one_label(&h, &kept, slots));
        }
        gi.sort = Some(pos);
        pos += 1;
        if let Err(e) = gi.validate() {
            return bad(e.to_string());
        }
        additions.push(gi);
    }
    if updates.is_empty() && additions.is_empty() && !removals.is_empty() {
        return bad("A household needs at least one person. Archive the household instead.".into());
    }

    let actor = s.user.actor();
    st.db
        .call(move |c| {
            let tx = crate::db::begin(c)?;
            store::update_household(&tx, &actor, id, &input)?;
            for (gid, gi) in &updates {
                store::update_guest(&tx, &actor, *gid, gi)?;
            }
            for gid in &removals {
                store::archive_guest(&tx, &actor, *gid)?;
            }
            for gi in &additions {
                store::create_guest(&tx, &actor, id, gi)?;
            }
            tx.commit()
        })
        .await?;
    Ok(redirect(&format!("/admin/households/{id}?m=saved")))
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
    kind: Option<String>,
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
        .filter(|g| match q.kind.as_deref() {
            Some("children") => g.is_child,
            Some("adults") => !g.is_child,
            Some("plus-ones") => g.is_plus_one() || g.plus_one_of.is_some(),
            _ => true,
        })
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
                select name="kind" {
                    option value="" { "Everyone" }
                    @for (k, label) in [("children", "Children"), ("adults", "Adults"), ("plus-ones", "Plus ones")] {
                        option value=(k) selected[q.kind.as_deref() == Some(k)] { (label) }
                    }
                }
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
                            td { (g.full_name()) @if g.is_child { span.tag { "child" } } @if g.plus_one_of.is_some() || g.is_plus_one() { span.tag { "+1" } } }
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
            // No save the date card: it goes out by email, as a link to their page.
            @for k in Kind::ALL.into_iter().filter(|k| *k != Kind::SaveTheDate) {
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
    let Some(kind) = Kind::parse(slug).filter(|k| *k != Kind::SaveTheDate) else { return Ok(ui::not_found()) };
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
    let configured = crate::gmail::connected(&st.env).is_some();

    // Per kind and tier: households still to send, already sent, and with no email at all.
    struct Counts {
        ready: [usize; 3],
        sent: usize,
        opened: usize,
        no_email: usize,
    }
    let counts = |kind: &str, is_sent: fn(&Household) -> bool| {
        let mut c = Counts { ready: [0; 3], sent: 0, opened: 0, no_email: 0 };
        for h in snap.households.iter().filter(|h| !h.events.is_empty()) {
            if is_sent(h) {
                c.sent += 1;
                if snap.mail(h.id, kind).is_some_and(|m| m.opened > 0) {
                    c.opened += 1;
                }
            } else if crate::email::recipients(h, &snap.guests_of(h.id)).is_empty() {
                c.no_email += 1;
            } else {
                c.ready[(h.tier.clamp(1, 3) - 1) as usize] += 1;
            }
        }
        c
    };
    let std_c = counts("save_the_date", |h| h.std_sent_at.is_some());
    let inv_c = counts("invitation", |h| h.invite_sent_at.is_some());

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
                    div.send-stat.quiet { span.send-num { (c.opened) } span.send-label { "opened" } }
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
        @if let Some(lock) = cfg.email_lock() {
            div.alert.bad {
                strong { "Email safety lock is on. " }
                "Every email, including bulk sends and tests, goes only to " strong { (lock) } " instead of the guest, "
                "with \"Not sent to …\" in the subject. Everything else is recorded as if it were real (sent, opened), so wipe that before going live. "
                "To send for real, remove " code { "redirect_all_to" } " from wedding.toml and redeploy."
            }
        }
        @if !configured {
            div.alert.warn { "Email isn't set up yet. Connect the Gmail account in " a href="/admin/settings#gmail" { "Settings" } " first." }
        }
        div.grid2 {
            (send_card("save_the_date", "Save the dates", "Each household gets a short email with the wreath and a button to their own save the date page. Households already sent are skipped.", "Send save the dates", &std_c))
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
                        @for a in ["a_parent", "b_parent", "helper"] {
                            button.button.small type="submit" name="role" value=(a) { "Approve as " (access_label(cfg, a)) }
                        }
                        button.button.small type="submit" name="role" value="owner" data-confirm={ "Make " (u.name) " an owner? Owners can do everything." } { "Approve as owner" }
                        button.button.small.ghost type="submit" name="role" value="disabled" { "Reject" }
                    }
                }
            }
        }
        section.card {
            h2 { "Team" }
            p.muted.small { strong { "Owners" } " do everything. " strong { (access_label(cfg, "a_parent")) "s, " (access_label(cfg, "b_parent")) "s and helpers" } " can see and update the list, but anyone they want to add is sent to you as a request." }
            @for u in list.iter().filter(|u| u.role != "pending") {
                form.user-row method="post" action="/admin/users" {
                    (csrf_input(&s.csrf)) input type="hidden" name="user" value=(u.id);
                    span.strong { (u.name) } @if u.id == s.user.id { span.muted.small { " (you)" } }
                    span.muted.small { " last seen " (when_opt(cfg, &u.last_login_at)) }
                    span.spacer {}
                    select name="role" aria-label="Role" {
                        @for a in ["owner", "a_parent", "b_parent", "helper", "disabled"] { option value=(a) selected[u.access() == a] { (if a == "disabled" { "Disabled".to_string() } else { access_label(cfg, a) }) } }
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
                select name="role" aria-label="Access" {
                    @for a in ["a_parent", "b_parent", "helper", "owner"] { option value=(a) { (access_label(cfg, a)) } }
                }
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
    let res = st.db.call(move |c| store::set_user_access(c, &actor, &uid, &role)).await;
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

const GMAIL_STATE_COOKIE: &str = "__Host-rsvp_gmail_state";

/// Start connecting the Gmail account: off to Google's consent screen.
async fn gmail_connect(State(st): State<AppState>, Member(s): Member) -> AppResult {
    guard!(s.require_owner());
    let state = crate::tokens::random_secret(16);
    let url = match crate::gmail::auth_url(&st.cfg, &st.env, &state, st.cfg.email.account.as_deref()) {
        Ok(u) => u,
        Err(e) => return Ok(ui::error_page(axum::http::StatusCode::BAD_REQUEST, "Gmail isn't set up on the server", &e.to_string())),
    };
    let mut r = redirect(&url);
    let c = format!("{GMAIL_STATE_COOKIE}={state}; Path=/; Secure; HttpOnly; SameSite=Lax; Max-Age=600");
    if let Ok(v) = axum::http::HeaderValue::from_str(&c) {
        r.headers_mut().append(axum::http::header::SET_COOKIE, v);
    }
    Ok(r)
}

#[derive(Deserialize)]
struct GmailCallback {
    code: Option<String>,
    state: Option<String>,
    error: Option<String>,
}

/// Google sends the browser back here. The admin session cookie is SameSite=Strict, so it isn't
/// sent on this cross-site arrival; this page hops (same-site) to /admin/gmail/finish, which is.
async fn gmail_return(axum::extract::RawQuery(q): axum::extract::RawQuery) -> axum::response::Response {
    use axum::response::IntoResponse;
    let next = format!("/admin/gmail/finish?{}", q.unwrap_or_default());
    let page = html! {
        (maud::DOCTYPE)
        html lang="en" {
            head {
                meta charset="utf-8";
                meta name="referrer" content="no-referrer";
                meta http-equiv="refresh" content={ "0;url=" (next) };
                title { "Connecting Gmail…" }
            }
            body { p { "Connecting Gmail… " a href=(next) { "Continue" } } }
        }
    };
    page.into_response()
}

async fn gmail_callback(State(st): State<AppState>, Member(s): Member, headers: axum::http::HeaderMap, Query(q): Query<GmailCallback>) -> AppResult {
    guard!(s.require_owner());
    let fail = |msg: String| Ok(ui::error_page(axum::http::StatusCode::BAD_REQUEST, "Couldn't connect Gmail", &msg));
    if let Some(e) = q.error {
        return fail(format!("Google said: {e}. Nothing was connected."));
    }
    let expected = super::auth::cookie(&headers, GMAIL_STATE_COOKIE).unwrap_or("");
    if expected.is_empty() || !crate::tokens::constant_time_eq(expected, q.state.as_deref().unwrap_or("")) {
        return fail("That sign-in link expired or didn't start here. Go back to Settings and click Connect Gmail again.".into());
    }
    let Some(code) = q.code else { return fail("Google didn't send a sign-in code.".into()) };
    let stored = match crate::gmail::connect(&st.http, &st.cfg, &st.env, &code).await {
        Ok(s) => s,
        Err(e) => return fail(e.to_string()),
    };
    if let Some(want) = st.cfg.email.account.as_deref().filter(|w| !w.eq_ignore_ascii_case(&stored.email)) {
        crate::gmail::disconnect(&st.env)?;
        return fail(format!("You signed in as {}, but emails must come from {want}. Try again and pick that account.", stored.email));
    }
    let (actor, email) = (s.user.actor(), stored.email.clone());
    st.db.call(move |c| store::audit(c, &actor, "gmail.connect", "", "", &json!({"account": email}))).await?;
    let mut r = redirect("/admin/settings?m=gmail-connected#gmail");
    if let Ok(v) = axum::http::HeaderValue::from_str(&format!("{GMAIL_STATE_COOKIE}=; Path=/; Secure; HttpOnly; SameSite=Lax; Max-Age=0")) {
        r.headers_mut().append(axum::http::header::SET_COOKIE, v);
    }
    Ok(r)
}

async fn gmail_disconnect(State(st): State<AppState>, Member(s): Member, Form(pairs): Form<Vec<(String, String)>>) -> AppResult {
    let f = F::new(pairs);
    guard!(f.csrf(&s));
    guard!(s.require_owner());
    crate::gmail::disconnect(&st.env)?;
    let actor = s.user.actor();
    st.db.call(move |c| store::audit(c, &actor, "gmail.disconnect", "", "", &json!({}))).await?;
    Ok(redirect("/admin/settings?m=gmail-disconnected#gmail"))
}

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
    let gmail = crate::gmail::connected(&st.env);
    let gmail_ready = st.env.gmail_client_id.is_some() && st.env.gmail_client_secret.is_some();
    let body = html! {
        section.card #gmail {
            h2 { "Email (Gmail)" }
            @match &gmail {
                Some(g) => {
                    p { "Emails are sent from " strong { (g.email) } ", connected " (when(cfg, &g.connected_at)) "." }
                    p.muted.small { "The site can only send email from this account. It can't read the inbox. Replies go to the inbox as normal." }
                    form.inline-actions method="post" action="/admin/gmail/disconnect" {
                        (csrf_input(&s.csrf))
                        button.button type="submit" data-confirm="Disconnect Gmail? Nothing can be emailed until it's connected again." { "Disconnect" }
                    }
                }
                None => {
                    p { "Not connected, so nothing can be emailed yet." }
                    p.muted.small {
                        "Connecting asks Google for permission to send email only. The site never sees the password and can't read the inbox."
                        @if let Some(a) = &cfg.email.account { " Sign in as " strong { (a) } "." }
                    }
                    @if gmail_ready { a.button.primary href="/admin/gmail/connect" { "Connect Gmail" } }
                    @else { p.alert.warn { "The server needs " code { "GMAIL_CLIENT_ID" } " and " code { "GMAIL_CLIENT_SECRET" } " in its env file first." } }
                }
            }
        }
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

// ---------------------------------------------------------------------------
// Website pages

async fn website(State(st): State<AppState>, Member(s): Member, Query(fl): Query<Flash>) -> AppResult {
    let pages = st.db.call(|c| crate::site::pages(c)).await?;
    let cfg = &st.cfg;
    let audience = |a: &str| crate::site::AUDIENCES.iter().find(|(k, _)| *k == a).map(|(_, l)| *l).unwrap_or(a).to_string();
    let body = html! {
        p.muted { "These pages appear in the menu on your guests' site once they've opened their invitation link. Only households with a valid link can see them." }
        div.table-wrap {
            table.list {
                thead { tr { th { "Page" } th { "Address" } th { "Shown to" } th { "Status" } th { "Last edited" } } }
                tbody {
                    @for p in &pages {
                        tr data-href={ "/admin/website/" (p.slug) } {
                            td { a.strong href={ "/admin/website/" (p.slug) } { (p.title) } @if p.builtin() { span.tag { "built in" } } }
                            td.small.muted { "/" (p.slug) }
                            td.small { (audience(&p.audience)) }
                            td { @if p.published { span.badge.good { "Published" } } @else { span.badge.muted { "Hidden" } } }
                            td.small.muted { (when(cfg, &p.updated_at)) @if !p.updated_by.is_empty() && p.updated_by != "system" { " · " (p.updated_by.trim_start_matches("user:")) } }
                        }
                    }
                }
            }
        }
        section.card {
            h2 { "Add a page" }
            form.inline-actions method="post" action="/admin/website" {
                (csrf_input(&s.csrf))
                input name="title" placeholder="e.g. Things to do" required maxlength="100" aria-label="Page title";
                button.button.primary type="submit" { "Create page" }
            }
        }
    };
    page(&st, &s, "website", "Website", &fl, body).await
}

async fn create_page(State(st): State<AppState>, Member(s): Member, Form(pairs): Form<Vec<(String, String)>>) -> AppResult {
    let f = F::new(pairs);
    guard!(f.csrf(&s));
    let title = f.s("title");
    let mut slug = crate::site::slugify(&title);
    if !crate::site::valid_slug(&slug) {
        slug = format!("page-{slug}");
    }
    let actor = s.user.actor();
    let res = st
        .db
        .call(move |c| {
            let mut candidate = slug.clone();
            let mut n = 2;
            while crate::site::get_page(c, &candidate)?.is_some() {
                candidate = format!("{slug}-{n}");
                n += 1;
            }
            let sort: i64 = c.query_row("SELECT COALESCE(MAX(sort), 0) + 10 FROM pages WHERE slug <> 'gallery'", [], |r| r.get(0))?;
            let input = crate::site::PageInput { title, body: String::new(), audience: "all".into(), published: false, sort };
            crate::site::save_page(c, &actor, &candidate, &input)?;
            Ok(candidate)
        })
        .await;
    Ok(match res {
        Ok(slug) => redirect(&format!("/admin/website/{slug}")),
        Err(e) => ui::error_page(axum::http::StatusCode::BAD_REQUEST, "Couldn't create that page", &e.to_string()),
    })
}

async fn edit_page(State(st): State<AppState>, Member(s): Member, Path(slug): Path<String>, Query(fl): Query<Flash>) -> AppResult {
    let p = { let slug = slug.clone(); st.db.call(move |c| crate::site::get_page(c, &slug)).await? };
    let Some(p) = p else { return Ok(ui::not_found()) };
    let body = html! {
        form.card.page-editor method="post" action={ "/admin/website/" (p.slug) } {
            (csrf_input(&s.csrf))
            div.form-grid {
                label.field { span { "Title (shown in the menu)" } input name="title" value=(p.title) required maxlength="100"; }
                label.field { span { "Address" } input value={ (st.cfg.site.base_url) "/" (p.slug) } readonly; }
                label.field {
                    span { "Shown to" }
                    select name="audience" { @for (k, l) in crate::site::AUDIENCES { option value=(k) selected[p.audience == *k] { (l) } } }
                }
                label.field { span { "Menu order " small { "(lower comes first)" } } input type="number" name="sort" value=(p.sort) step="1"; }
                label.check.span2 { input type="checkbox" name="published" checked[p.published]; "Published: show this page on the site" }
                @if p.slug == "schedule" { p.span2.muted.small { "Each household's official events (with dates, times and venues) are listed automatically under your text. Use the text for everything else: welcome drinks, the day after, timings, notes." } }
                @if p.slug == "gallery" { p.span2.muted.small { "This text appears above the photos. Add and caption photos on the " a href="/admin/gallery" { "Gallery" } " page." } }
                label.field.span2 {
                    span { "Page text" }
                    textarea.page-body name="body" rows="18" maxlength="50000" { (p.body) }
                }
                details.span2.format-help {
                    summary { "Formatting help" }
                    pre { "## A heading\n\nA paragraph. **Bold**, *italic*, [a link](https://example.com).\n\n- a bullet\n- another bullet\n\n1. a numbered step\n2. another step" }
                    p.muted.small { "Text only for households in Australia (or only for everyone else) goes between two marker lines:" }
                    pre { "::: australia\n## Coming from Australia\nOnly households in Australia see this.\n:::\n\n::: us\nOnly households outside Australia see this.\n:::" }
                }
            }
            div.form-actions {
                button.button.primary type="submit" { "Save page" }
                @if p.published { a.button.ghost href={ "/" (p.slug) } target="_blank" rel="noopener" { "View on site ↗" } }
            }
        }
        @if !p.builtin() {
            form method="post" action={ "/admin/website/" (p.slug) "/delete" } {
                (csrf_input(&s.csrf))
                button.button.ghost.small type="submit" data-confirm="Delete this page?" { "Delete page" }
            }
        }
    };
    page(&st, &s, "website", &p.title, &fl, body).await
}

async fn save_page(State(st): State<AppState>, Member(s): Member, Path(slug): Path<String>, Form(pairs): Form<Vec<(String, String)>>) -> AppResult {
    let f = F::new(pairs);
    guard!(f.csrf(&s));
    let input = crate::site::PageInput {
        title: f.s("title"),
        body: f.0.get("body").cloned().unwrap_or_default().replace("\r\n", "\n"),
        audience: f.s("audience"),
        published: f.check("published"),
        sort: f.s("sort").parse().unwrap_or(0),
    };
    let (actor, slug2) = (s.user.actor(), slug.clone());
    let res = st
        .db
        .call(move |c| {
            if crate::site::get_page(c, &slug2)?.is_none() {
                anyhow::bail!("page not found");
            }
            crate::site::save_page(c, &actor, &slug2, &input)
        })
        .await;
    Ok(match res {
        Ok(()) => redirect(&format!("/admin/website/{slug}?m=page-saved")),
        Err(e) => ui::error_page(axum::http::StatusCode::BAD_REQUEST, "Couldn't save that page", &e.to_string()),
    })
}

async fn delete_page(State(st): State<AppState>, Member(s): Member, Path(slug): Path<String>, Form(pairs): Form<Vec<(String, String)>>) -> AppResult {
    let f = F::new(pairs);
    guard!(f.csrf(&s));
    let actor = s.user.actor();
    let res = st.db.call(move |c| crate::site::delete_page(c, &actor, &slug)).await;
    Ok(match res {
        Ok(()) => redirect("/admin/website?m=page-deleted"),
        Err(e) => ui::error_page(axum::http::StatusCode::BAD_REQUEST, "Couldn't delete that page", &e.to_string()),
    })
}

// ---------------------------------------------------------------------------
// Gallery

async fn gallery(State(st): State<AppState>, Member(s): Member, Query(fl): Query<Flash>) -> AppResult {
    let (photos, gpage, home) = st
        .db
        .call(|c| Ok((crate::site::photos(c)?, crate::site::get_page(c, "gallery")?, crate::site::home_photo(c)?.map(|p| p.id))))
        .await?;
    let published = gpage.as_ref().is_some_and(|p| p.published);
    let body = html! {
        @if !published {
            div.alert.warn { "The gallery page is hidden from guests. " a href="/admin/website/gallery" { "Publish it and write its description" } " when you're ready." }
        } @else {
            p.muted { "Guests see these photos, in this order, on the " a href="/admin/website/gallery" { "Gallery page" } " (where you can also edit the description above the photos)." }
        }
        section.card {
            h2 { "Add photos" }
            form.upload-form method="post" action="/admin/gallery/upload" enctype="multipart/form-data" {
                (csrf_input(&s.csrf))
                input type="file" name="photos" accept="image/jpeg,image/png,image/webp" multiple required;
                button.button.primary type="submit" { "Upload" }
            }
            p.muted.small { "JPEG, PNG or WebP, up to 25 MB each. Photos are resized for the web and their location and camera details are removed. On an iPhone, choosing photos here sends them as JPEG automatically." }
        }
        @if photos.is_empty() { p.muted { "No photos yet." } }
        div.admin-gallery {
            @for (i, p) in photos.iter().enumerate() {
                form.photo-card method="post" action={ "/admin/gallery/" (p.id) } {
                    (csrf_input(&s.csrf))
                    a href={ "/media/" (p.large()) } target="_blank" rel="noopener" { img src={ "/media/" (p.thumb()) } alt="" loading="lazy"; }
                    textarea name="caption" rows="2" maxlength="1000" placeholder="Caption (optional)" aria-label="Caption" { (p.caption) }
                    div.photo-actions {
                        button.button.small type="submit" name="action" value="caption" { "Save caption" }
                        button.button.small.ghost type="submit" name="action" value="up" disabled[i == 0] title="Move earlier" { "←" }
                        button.button.small.ghost type="submit" name="action" value="down" disabled[i + 1 == photos.len()] title="Move later" { "→" }
                        @if home == Some(p.id) {
                            button.button.small type="submit" name="action" value="home" title="Shown at the top of every invitation page. Click to stop." { "On invitation page ✓" }
                        } @else {
                            button.button.small.ghost type="submit" name="action" value="home" title="Show this photo on every invitation page" { "Show on invitation page" }
                        }
                        button.button.small.ghost type="submit" name="action" value="delete" data-confirm="Remove this photo from the gallery?" { "Remove" }
                    }
                }
            }
        }
    };
    page(&st, &s, "gallery", "Gallery", &fl, body).await
}

async fn gallery_upload(State(st): State<AppState>, Member(s): Member, mut mp: axum::extract::Multipart) -> AppResult {
    let mut csrf_ok = false;
    let (mut added, mut failed) = (0usize, Vec::new());
    while let Some(field) = mp.next_field().await.map_err(|e| anyhow::anyhow!("upload interrupted: {e}"))? {
        match field.name() {
            Some("_csrf") => {
                let v = field.text().await.unwrap_or_default();
                csrf_ok = s.check_csrf(Some(&v)).is_ok();
            }
            Some("photos") => {
                if !csrf_ok {
                    return Ok(ui::error_page(axum::http::StatusCode::FORBIDDEN, "Session expired", "Please reload the page and try again."));
                }
                let name = field.file_name().unwrap_or("photo").to_string();
                let bytes = match field.bytes().await {
                    Ok(b) => b,
                    Err(e) => {
                        failed.push(format!("{name}: {e}"));
                        continue;
                    }
                };
                if bytes.is_empty() {
                    continue;
                }
                if bytes.len() > 25 * 1024 * 1024 {
                    failed.push(format!("{name}: larger than 25 MB"));
                    continue;
                }
                let dir = st.env.data_dir.clone();
                match tokio::task::spawn_blocking(move || crate::site::process_photo(&dir, &bytes)).await? {
                    Ok((stem, w, h)) => {
                        let actor = s.user.actor();
                        st.db.call(move |c| crate::site::add_photo(c, &actor, &stem, w, h, "")).await?;
                        added += 1;
                    }
                    Err(e) => failed.push(format!("{name}: {e}")),
                }
            }
            _ => {}
        }
    }
    if !failed.is_empty() {
        return Ok(ui::error_page(
            axum::http::StatusCode::BAD_REQUEST,
            &format!("{added} photo(s) added, {} couldn't be", failed.len()),
            &failed.join(" · "),
        ));
    }
    Ok(redirect("/admin/gallery?m=uploaded"))
}

async fn gallery_photo(State(st): State<AppState>, Member(s): Member, Path(id): Path<i64>, Form(pairs): Form<Vec<(String, String)>>) -> AppResult {
    let f = F::new(pairs);
    guard!(f.csrf(&s));
    let (action, caption, actor) = (f.s("action"), f.0.get("caption").cloned().unwrap_or_default(), s.user.actor());
    st.db
        .call(move |c| match action.as_str() {
            "caption" => crate::site::set_caption(c, &actor, id, &caption),
            "up" => crate::site::move_photo(c, &actor, id, -1),
            "down" => crate::site::move_photo(c, &actor, id, 1),
            "delete" => crate::site::delete_photo(c, &actor, id),
            // Toggle: this photo on the invitation page, or none.
            "home" => {
                let current = crate::site::home_photo(c)?.map(|p| p.id);
                store::set_setting(c, &actor, "home_photo", &if current == Some(id) { String::new() } else { id.to_string() })
            }
            _ => Ok(()),
        })
        .await?;
    Ok(redirect("/admin/gallery?m=saved"))
}

async fn review_request(State(st): State<AppState>, Member(s): Member, Path(id): Path<i64>, Form(pairs): Form<Vec<(String, String)>>) -> AppResult {
    let f = F::new(pairs);
    guard!(f.csrf(&s));
    guard!(s.require_owner());
    let approve = f.s("decision") == "approve";
    let actor = s.user.actor();
    let res = st.db.call(move |c| store::review_request(c, &actor, id, approve)).await;
    Ok(match res {
        Ok(()) if approve => redirect(&format!("/admin/households/{id}?m=approved")),
        Ok(()) => redirect("/admin/households?m=declined"),
        Err(e) => ui::error_page(axum::http::StatusCode::BAD_REQUEST, "Couldn't do that", &e.to_string()),
    })
}

//! Guest-facing pages. Each household has one secret link: /i/{token}.

use std::collections::{BTreeMap, HashMap};

use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::IntoResponse;
use axum::routing::get;
use axum::{Extension, Form, Router};
use maud::{Markup, html};
use serde::Deserialize;

use super::ui::{self, guest_page};
use super::{AppResult, AppState, ClientIp, redirect};
use crate::assets::{self, Kind, Layout, words};
use crate::config::{Config, parse_local};
use crate::rsvp::{self, field};
use crate::store::{self, Guest, Household, Phase, Response as Resp};

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/i/{token}", get(page).post(submit))
        .route("/i/{token}/calendar.ics", get(calendar))
        .route("/i/{token}/address", axum::routing::post(save_address))
        .route("/i/{token}/address/suggest", get(address_suggest))
        .route("/i/{token}/address/place", get(address_place))
        .route("/i/{token}/{file}", get(download))
}

pub async fn home(State(st): State<AppState>, headers: HeaderMap) -> axum::response::Response {
    // A returning guest goes straight back to their invitation.
    if let Some(token) = guest_cookie(&headers) {
        let t = token.clone();
        if st.db.call(move |c| store::household_by_token(c, &t)).await.ok().flatten().is_some() {
            return redirect(&format!("/i/{token}"));
        }
    }
    guest_page(
        &st.cfg,
        &st.cfg.title(),
        None,
        html! {
            section.hero {
                p.kicker { "By invitation" }
                h1.names { (st.cfg.couple.a_first) span.amp { "&" } (st.cfg.couple.b_first) }
                p.lede { "Please use the personal link or QR code from your card to view your invitation." }
            }
        },
    )
    .into_response()
}

struct Ctx {
    h: Household,
    guests: Vec<Guest>,
    responses: Vec<Resp>,
    phase: Phase,
    pages: Vec<(String, String)>,
}

/// Remembers which household this browser belongs to, so the rest of the site
/// (schedule, travel, gallery) works without the invitation link in the URL.
pub const GUEST_COOKIE: &str = "__Host-rsvp_guest";

pub fn guest_cookie(headers: &HeaderMap) -> Option<String> {
    headers
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(';'))
        .find_map(|kv| kv.trim().strip_prefix(GUEST_COOKIE).and_then(|r| r.strip_prefix('=')).map(String::from))
        .filter(|t| crate::tokens::plausible_token(t))
}

fn remember(mut resp: axum::response::Response, token: &str) -> axum::response::Response {
    // Lax so that arriving from an emailed link still sends it; one year.
    let v = format!("{GUEST_COOKIE}={token}; Path=/; Secure; HttpOnly; SameSite=Lax; Max-Age=31536000");
    if let Ok(v) = axum::http::HeaderValue::from_str(&v) {
        resp.headers_mut().append(header::SET_COOKIE, v);
    }
    resp
}

/// Published pages this household can see, for the site navigation.
pub fn nav_pages(c: &rusqlite::Connection, h: &Household) -> anyhow::Result<Vec<(String, String)>> {
    Ok(crate::site::pages(c)?.into_iter().filter(|p| p.published && p.shown_to(h.australia)).map(|p| (p.slug, p.title)).collect())
}

async fn load(st: &AppState, token: &str) -> AppResult<Option<Ctx>> {
    if !crate::tokens::plausible_token(token) {
        return Ok(None);
    }
    let token = token.to_string();
    Ok(st
        .db
        .call(move |c| {
            let Some(h) = store::household_by_token(c, &token)? else { return Ok(None) };
            store::mark_viewed(c, h.id)?;
            let guests = store::guests_for(c, h.id)?;
            let responses = store::responses_for(c, h.id)?;
            let phase = store::phase(c)?;
            let pages = nav_pages(c, &h)?;
            Ok(Some(Ctx { h, guests, responses, phase, pages }))
        })
        .await?)
}

#[derive(Deserialize)]
struct PageQuery {
    sent: Option<String>,
    address: Option<String>,
}

async fn page(State(st): State<AppState>, Path(token): Path<String>, Query(q): Query<PageQuery>) -> AppResult {
    let Some(ctx) = load(&st, &token).await? else { return Ok(ui::not_found()) };
    let values = rsvp::current_values(&ctx.guests, &ctx.responses, &ctx.h);
    let addr = AddressView::Current { saved: q.address.is_some() };
    Ok(remember(render(&st, &token, &ctx, &values, &BTreeMap::new(), q.sent.is_some(), &addr).into_response(), &token))
}

async fn submit(
    State(st): State<AppState>,
    Path(token): Path<String>,
    Extension(ClientIp(ip)): Extension<ClientIp>,
    headers: HeaderMap,
    Form(pairs): Form<Vec<(String, String)>>,
) -> AppResult {
    let Some(ctx) = load(&st, &token).await? else { return Ok(ui::not_found()) };
    if ctx.phase != Phase::RsvpOpen || st.cfg.past_deadline() {
        return Ok(redirect(&format!("/i/{token}")));
    }
    if pairs.len() > 500 {
        return Ok(ui::error_page(StatusCode::BAD_REQUEST, "That didn't work", "The form was too large."));
    }
    let values: HashMap<String, String> = pairs.into_iter().collect();
    let form = match rsvp::parse(&st.cfg, &ctx.h, &ctx.guests, &values) {
        Ok(f) => f,
        Err(errors) => {
            return Ok((StatusCode::UNPROCESSABLE_ENTITY, render(&st, &token, &ctx, &values, &errors, false, &AddressView::Current { saved: false })).into_response());
        }
    };
    let ip_hash = crate::tokens::ip_hash(&st.env.secret, &ip);
    let ua = headers.get(header::USER_AGENT).and_then(|v| v.to_str().ok()).unwrap_or("").to_string();
    let (journal, cfg, h) = (st.journal.clone(), st.cfg.clone(), ctx.h.clone());
    let saved = st.db.call(move |c| rsvp::submit(c, &journal, &cfg, &h, &form, &ip_hash, &ua)).await;
    match saved {
        Ok(id) => {
            tracing::info!(household = ctx.h.id, submission = %id, "rsvp received");
            Ok(redirect(&format!("/i/{token}?sent=1#reply")))
        }
        Err(e) => {
            tracing::error!(error = ?e, household = ctx.h.id, "RSVP SAVE FAILED");
            Ok(ui::error_page(
                StatusCode::SERVICE_UNAVAILABLE,
                "We couldn't save your reply",
                "Sorry, something went wrong saving your RSVP. Please go back and try again in a minute, or let us know directly.",
            ))
        }
    }
}

fn render(
    st: &AppState,
    token: &str,
    ctx: &Ctx,
    values: &HashMap<String, String>,
    errors: &BTreeMap<String, String>,
    just_sent: bool,
    addr: &AddressView,
) -> Markup {
    let cfg = &st.cfg;
    let h = &ctx.h;
    let closed = ctx.phase == Phase::RsvpClosed || (ctx.phase == Phase::RsvpOpen && cfg.past_deadline());
    let date = cfg.wedding_date().ok();
    let events: Vec<_> = h.events.iter().filter_map(|e| cfg.event(e)).collect();

    let body = html! {
        section.hero {
            p.kicker { @if ctx.phase == Phase::SaveTheDate { "Save the date" } @else { "You're invited" } }
            h1.names { (cfg.couple.a_first) span.amp { "&" } (cfg.couple.b_first) }
            div.rule { span {} i {} span {} }
            @if let Some(d) = date {
                p.date { (words::weekday_name(d.weekday())) " · " (d.day()) " " (words::month_name(d.month())) " · " (d.year()) }
            }
            p.place { (cfg.wedding.location) }
        }

        section.letter {
            p.salutation { "Dear " (h.greeting) "," }
            @match ctx.phase {
                Phase::SaveTheDate => {
                    p { "We're getting married, and we would love nothing more than to have you there." }
                    p { "Please save the date. A formal invitation will follow." }
                }
                _ => {
                    p { "We would be honoured to have you celebrate with us. All the details are below." }
                    @if !closed {
                        p { "Kindly let us know whether you can make it by " strong { (date_medium(&cfg.wedding.rsvp_deadline)) } "." }
                    }
                }
            }
            @if let Some(n) = &cfg.wedding.celebration_note { p.celebration { (n) } }
            div.actions {
                a.button.ghost href={ "/i/" (token) "/calendar.ics" } { "Add to calendar" }
            }
        }

        // Ask for a mailing address before the formal invitation goes out (and whenever it's missing).
        @if !closed && (ctx.phase == Phase::SaveTheDate || h.address_lines().is_empty() || !matches!(addr, AddressView::Current { .. })) {
            (address_section(cfg, token, h, addr))
        }

        @if ctx.phase != Phase::SaveTheDate {
            section.events {
                h2.section-title { "Events" }
                div.event-grid {
                    @for e in &events {
                        article.event {
                            p.kicker { (e.name) }
                            @if let Some(t) = e.start() {
                                p.event-when { (words::date_long(t.date())) }
                                p.event-time {
                                    (words::time_short(t))
                                    @if let Some(end) = e.ends.as_deref().and_then(|x| parse_local(x).ok()) { " – " (words::time_short(end)) }
                                }
                            } @else {
                                p.event-when { "Date to be announced" }
                                p.event-time { "We'll ask for your RSVP once it's set." }
                            }
                            p.event-venue { (e.venue) }
                            @if !e.address.is_empty() {
                                p.event-address {
                                    a href=(e.map_url()) rel="noopener noreferrer" target="_blank" { (e.address) }
                                }
                            }
                            @if let Some(d) = &e.dress { p.event-dress { "Dress: " (d) } }
                            @if let Some(n) = &e.note { p.event-note { (n) } }
                        }
                    }
                }
            }

            section #reply .reply {
                h2.section-title { "Your reply" }
                @if just_sent {
                    div.notice.success role="status" {
                        p.notice-title { "Thank you, we've got your reply." }
                        p { "You can come back to this page any time before " (date_medium(&cfg.wedding.rsvp_deadline)) " to make changes." }
                    }
                }
                @if closed {
                    (summary(cfg, ctx))
                    p.muted { "RSVPs are now closed. If something has changed, please get in touch with us directly." }
                } @else {
                    @if !just_sent { @if let Some(at) = &h.rsvp_at {
                        div.notice role="status" { p { "We received your reply on " (ui::when(cfg, at)) ". You can update it below." } }
                    } }
                    (form(cfg, token, ctx, values, errors))
                }
            }
        }

    };
    let nav = ui::GuestNav { home: format!("/i/{token}"), pages: ctx.pages.clone(), active: "home".into() };
    guest_page(cfg, &format!("{} | {}", h.greeting, cfg.title()), Some(&nav), body)
}

fn date_medium(iso: &str) -> String {
    crate::config::parse_date(iso).map(words::date_medium).unwrap_or_else(|_| iso.to_string())
}

fn form(cfg: &Config, token: &str, ctx: &Ctx, values: &HashMap<String, String>, errors: &BTreeMap<String, String>) -> Markup {
    let v = |k: &str| values.get(k).cloned().unwrap_or_default();
    let events: Vec<_> = ctx.h.events.iter().filter_map(|e| cfg.event(e)).collect();
    let err = |k: &str| errors.get(k).map(|m| html! { p.field-error { (m) } });
    html! {
        @if !errors.is_empty() {
            div.notice.error role="alert" {
                p.notice-title { "Just a couple of things before we can save this:" }
                ul { @for m in errors.values() { li { (m) } } }
            }
        }
        form.rsvp method="post" action={ "/i/" (token) "#reply" } novalidate {
            @for g in rsvp::primary_guests(&ctx.guests) {
                fieldset.person {
                    legend { (g.full_name()) }
                    @for e in events.iter().filter(|e| e.dated() && g.invited(&ctx.h, &e.slug)) {
                        @let att_key = field::att(g.id, &e.slug);
                        @let cur = v(&att_key);
                        div.event-answer data-attending-group=(att_key) {
                            p.answer-label { (e.name) }
                            div.choice-row {
                                label.choice { input type="radio" name=(att_key) value="yes" checked[cur == "yes"]; span { "Joyfully accepts" } }
                                label.choice { input type="radio" name=(att_key) value="no" checked[cur == "no"]; span { "Regretfully declines" } }
                            }
                            @if let Some(m) = err(&att_key) { (m) }
                            @let meals = rsvp::meals_for(cfg, g.is_child);
                            @if e.meal && !meals.is_empty() {
                                @let mk = field::meal(g.id, &e.slug);
                                @let curm = v(&mk);
                                div.meals data-show-when=(att_key) {
                                    p.answer-sub { "Dinner" }
                                    div.meal-grid {
                                        @for m in &meals {
                                            label.meal {
                                                input type="radio" name=(mk) value=(m.slug) checked[curm == m.slug];
                                                span { strong { (m.name) } @if !m.description.is_empty() { small { (m.description) } } }
                                            }
                                        }
                                    }
                                    @if let Some(m) = err(&mk) { (m) }
                                }
                            }
                        }
                    }
                    // Name and dietary only matter once they accept something; guest.js hides them otherwise.
                    @let att_keys = events.iter().filter(|e| e.dated() && g.invited(&ctx.h, &e.slug)).map(|e| field::att(g.id, &e.slug)).collect::<Vec<_>>().join(" ");
                    @if g.is_plus_one() {
                        div.plus-one data-show-when=(att_keys) {
                            p.answer-sub { "Who are you bringing?" }
                            div.two {
                                label.field { span { "First name" } input type="text" name=(field::first(g.id)) value=(v(&field::first(g.id))) maxlength="80" autocomplete="off"; }
                                label.field { span { "Last name" } input type="text" name=(field::last(g.id)) value=(v(&field::last(g.id))) maxlength="80" autocomplete="off"; }
                            }
                            @if let Some(m) = err(&field::first(g.id)) { (m) }
                        }
                    }
                    @if cfg.rsvp.ask_dietary {
                        label.field data-show-when=(att_keys) {
                            span { "Dietary requirements or allergies" }
                            input type="text" name=(field::diet(g.id)) value=(v(&field::diet(g.id))) maxlength="500" placeholder="None";
                        }
                    }
                }
            }

            fieldset.extras {
                legend { "A few extras" }
                @if cfg.rsvp.ask_song {
                    label.field { span { "A song that will get you on the dance floor" } input type="text" name="song" value=(v("song")) maxlength="200"; }
                }
                @if cfg.rsvp.ask_accommodation {
                    label.field {
                        span { "Accommodation" }
                        select name="accommodation" {
                            @for (k, l) in rsvp::ACCOMMODATION_OPTIONS { option value=(k) selected[v("accommodation") == *k] { (l) } }
                        }
                    }
                }
                @if cfg.rsvp.ask_transport {
                    div.field {
                        span { (cfg.rsvp.transport_question) }
                        div.choice-row {
                            @for (k, l) in rsvp::TRANSPORT_OPTIONS.iter().filter(|(k, _)| !k.is_empty()) {
                                label.choice { input type="radio" name="transport" value=(k) checked[v("transport") == *k]; span { (l) } }
                            }
                        }
                    }
                }
                @if cfg.rsvp.ask_message {
                    label.field { span { "A note for us" } textarea name="message" rows="4" maxlength="2000" { (v("message")) } }
                }
            }
            div.submit-row {
                button.button.primary type="submit" { @if ctx.h.rsvp_at.is_some() { "Update our reply" } @else { "Send our reply" } }
            }
        }
    }
}

fn summary(cfg: &Config, ctx: &Ctx) -> Markup {
    let by: HashMap<(i64, &str), &Resp> = ctx.responses.iter().map(|r| ((r.guest_id, r.event_slug.as_str()), r)).collect();
    html! {
        div.summary {
            @if ctx.h.rsvp_at.is_none() { p { "We didn't receive a reply from you." } }
            @for g in &ctx.guests {
                div.summary-person {
                    p.summary-name { (g.full_name()) }
                    ul {
                        @for slug in ctx.h.events.iter().filter(|s| g.plus_one_of.is_some() || g.invited(&ctx.h, s)) {
                            @if let Some(e) = cfg.event(slug) {
                                li {
                                    (e.name) ": "
                                    @match by.get(&(g.id, slug.as_str())) {
                                        Some(r) if r.attending => { "attending" @if let Some(m) = cfg.meal(&r.meal) { " · " (m.name) } }
                                        Some(_) => { "not attending" }
                                        None => { "no reply" }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}

async fn calendar(State(st): State<AppState>, Path(token): Path<String>) -> AppResult {
    let Some(ctx) = load(&st, &token).await? else { return Ok(ui::not_found()) };
    let cfg = &st.cfg;
    let off = cfg.offset()?;
    let fmt = time::macros::format_description!("[year][month][day]T[hour][minute][second]Z");
    let stamp = time::OffsetDateTime::now_utc().format(&fmt)?;
    let host = cfg.site.base_url.trim_start_matches("https://").trim_start_matches("http://");
    let mut ics = String::from("BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:-//rsvp//wedding//EN\r\nCALSCALE:GREGORIAN\r\nMETHOD:PUBLISH\r\n");
    for slug in &ctx.h.events {
        let Some(e) = cfg.event(slug) else { continue };
        let Some(start) = e.start() else { continue }; // no date yet
        let start = start.assume_offset(off).to_offset(time::UtcOffset::UTC);
        let end = match &e.ends {
            Some(x) => parse_local(x)?.assume_offset(off).to_offset(time::UtcOffset::UTC),
            None => start + time::Duration::hours(2),
        };
        ics.push_str("BEGIN:VEVENT\r\n");
        ics.push_str(&fold(&format!("UID:{}-{}@{host}", e.slug, ctx.h.id)));
        ics.push_str(&format!("DTSTAMP:{stamp}\r\nDTSTART:{}\r\nDTEND:{}\r\n", start.format(&fmt)?, end.format(&fmt)?));
        ics.push_str(&fold(&format!("SUMMARY:{}", ics_escape(&format!("{} & {}: {}", cfg.couple.a_first, cfg.couple.b_first, e.name)))));
        ics.push_str(&fold(&format!("LOCATION:{}", ics_escape(&format!("{}, {}", e.venue, e.address)))));
        ics.push_str(&fold(&format!("DESCRIPTION:{}", ics_escape(&format!("Details and RSVP: {}", assets::link(cfg, &token))))));
        ics.push_str("END:VEVENT\r\n");
    }
    ics.push_str("END:VCALENDAR\r\n");
    Ok((
        [(header::CONTENT_TYPE, "text/calendar; charset=utf-8"), (header::CONTENT_DISPOSITION, "attachment; filename=\"wedding.ics\"")],
        ics,
    )
        .into_response())
}

fn ics_escape(s: &str) -> String {
    s.replace('\\', "\\\\").replace(';', "\\;").replace(',', "\\,").replace('\n', "\\n")
}

/// RFC 5545 line folding at 75 octets.
fn fold(line: &str) -> String {
    let mut out = String::new();
    let mut n = 0;
    for ch in line.chars() {
        let len = ch.len_utf8();
        if n + len > 75 {
            out.push_str("\r\n ");
            n = 1;
        }
        out.push(ch);
        n += len;
    }
    out.push_str("\r\n");
    out
}

/// The guest's own card as a PDF (or the front as PNG).
async fn download(State(st): State<AppState>, Path((token, file)): Path<(String, String)>) -> AppResult {
    let Some(ctx) = load(&st, &token).await? else { return Ok(ui::not_found()) };
    let (kind, ext) = match file.as_str() {
        "save-the-date.pdf" => (Kind::SaveTheDate, "pdf"),
        "save-the-date.png" => (Kind::SaveTheDate, "png"),
        "invitation.pdf" if ctx.phase != Phase::SaveTheDate => (Kind::Invitation, "pdf"),
        "invitation.png" if ctx.phase != Phase::SaveTheDate => (Kind::Invitation, "png"),
        _ => return Ok(ui::not_found()),
    };
    let (r, cfg, env) = (st.renderer.clone(), st.cfg.clone(), st.env.clone());
    let rows = vec![(ctx.h, ctx.guests, ctx.responses)];
    let bytes = tokio::task::spawn_blocking(move || -> anyhow::Result<Vec<u8>> {
        let doc = assets::render_households(&r, &cfg, &env, kind, Layout::Card, &rows)?;
        if ext == "pdf" { assets::to_pdf(&doc) } else { assets::to_png(&doc, 0, 3.0) }
    })
    .await??;
    let ct = if ext == "pdf" { "application/pdf" } else { "image/png" };
    Ok(([(header::CONTENT_TYPE, ct), (header::CONTENT_DISPOSITION, "inline")], bytes).into_response())
}

// ---------------------------------------------------------------------------
// Mailing address

pub enum AddressView {
    /// Show what we have (or the empty form). `saved` = just submitted.
    Current { saved: bool },
    /// Re-show the form with a problem message.
    Form { entered: Address, error: String },
    /// Google suggested a corrected version, or couldn't confirm the address.
    Confirm { entered: Address, suggestion: Option<Address>, action: AddrAction, detail: String },
}

use crate::address::{self, Action as AddrAction, Address};

fn address_fields(a: &Address, australia: bool) -> Markup {
    let default_country = if australia { "Australia" } else { "United States" };
    let country = if a.country.is_empty() { default_country } else { a.country.as_str() };
    let known = address::COUNTRIES.iter().any(|(n, _)| *n == country);
    html! {
        // Search box (shown by guest.js; without JavaScript the manual fields below are used).
        input type="hidden" name="picked" value="";
        div.address-search hidden {
            label.field.search-field {
                span { "Search for your address" }
                input.addr-search type="text" autocomplete="off" spellcheck="false" placeholder="Start typing your street address…"
                    role="combobox" aria-autocomplete="list" aria-expanded="false" maxlength="200";
            }
            ul.addr-results role="listbox" hidden {}
            p.addr-status.muted.small aria-live="polite" {}
            div.addr-picked hidden {
                address.postal {}
                button.linkish.addr-change type="button" { "Change" }
            }
            p.small { button.linkish.addr-manual type="button" { "Can't find it? Enter it manually" } }
        }
        label.field.unit-field { span { "Apartment, unit, suite " small.muted { "(optional)" } } input name="addr2" value=(a.addr2) autocomplete="address-line2" maxlength="200"; }
        div.manual-fields {
        p.small.addr-back-wrap hidden { button.linkish.addr-back type="button" { "← Search for my address instead" } }
        label.field { span { "Street address" } input name="addr1" value=(a.addr1) autocomplete="address-line1" maxlength="200"; }
        div.two {
            label.field { span { "City / suburb" } input name="city" value=(a.city) autocomplete="address-level2" required maxlength="100"; }
            label.field { span { "State" } input name="region" value=(a.region) autocomplete="address-level1" maxlength="100"; }
        }
        div.two {
            label.field { span { "ZIP / postcode" } input name="postcode" value=(a.postcode) autocomplete="postal-code" maxlength="20"; }
            label.field {
                span { "Country" }
                select name="country" autocomplete="country-name" {
                    @for (n, _) in address::COUNTRIES { option value=(n) selected[*n == country] { (n) } }
                    option value="other" selected[!known] { "Somewhere else" }
                }
            }
        }
        label.field.country-other hidden[known] { span { "Country" } input name="country_other" value=(if known { "" } else { country }) maxlength="100"; }
        }
    }
}

fn hidden_address(prefix: &str, a: &Address) -> Markup {
    html! {
        input type="hidden" name={ (prefix) "addr1" } value=(a.addr1);
        input type="hidden" name={ (prefix) "addr2" } value=(a.addr2);
        input type="hidden" name={ (prefix) "city" } value=(a.city);
        input type="hidden" name={ (prefix) "region" } value=(a.region);
        input type="hidden" name={ (prefix) "postcode" } value=(a.postcode);
        input type="hidden" name={ (prefix) "country" } value=(a.country);
    }
}

fn address_section(_cfg: &Config, token: &str, h: &Household, view: &AddressView) -> Markup {
    let action = format!("/i/{token}/address#address");
    let current = Address::from_household(h);
    html! {
        section #address .address {
            h2.section-title { "Your address" }
            @match view {
                AddressView::Current { saved } => {
                    @if *saved {
                        div.notice.success role="status" { p.notice-title { "Thank you, we've got your address." } }
                    }
                    @if !current.lines().is_empty() && !*saved {
                        p.center-text { "We'll send your formal invitation to:" }
                    } @else if current.lines().is_empty() {
                        p.center-text { "Please fill out your address so we can send you a formal invitation." }
                    }
                    @if !current.lines().is_empty() {
                        address.postal { @for l in current.lines() { (l) br; } }
                        details.edit-address {
                            summary { "Not right? Update it" }
                            form.address-form method="post" action=(action) { (address_fields(&current, h.australia)) div.submit-row { button.button.primary type="submit" { "Save address" } } }
                        }
                    } @else {
                        form.address-form method="post" action=(action) { (address_fields(&current, h.australia)) div.submit-row { button.button.primary type="submit" { "Save address" } } }
                    }
                }
                AddressView::Form { entered, error } => {
                    div.notice.error role="alert" { p { (error) } }
                    form.address-form method="post" action=(action) { (address_fields(entered, h.australia)) div.submit-row { button.button.primary type="submit" { "Save address" } } }
                }
                AddressView::Confirm { entered, suggestion, action: act, detail } => {
                    form.address-confirm method="post" action=(action) {
                        (hidden_address("", entered))
                        @if let Some(s) = suggestion.as_ref().filter(|_| *act != AddrAction::Fix) {
                            (hidden_address("s_", s))
                            p.center-text { @if *act == AddrAction::AddUnit { "That building has several units. Is there an apartment or unit number? Otherwise, which version should we use?" } @else { "We checked your address with the postal service. Which version should we use?" } }
                            div.address-choices {
                                button.address-choice.recommended type="submit" name="choice" value="suggested" {
                                    span.choice-label { "Suggested" }
                                    address.postal { @for l in s.lines() { (l) br; } }
                                }
                                button.address-choice type="submit" name="choice" value="entered" {
                                    span.choice-label { "As you typed it" }
                                    address.postal { @for l in entered.lines() { (l) br; } }
                                }
                            }
                        } @else {
                            div.notice.error role="alert" {
                                p.notice-title { "We couldn't find that address." }
                                @if !detail.is_empty() { p { (detail) } }
                            }
                            address.postal { @for l in entered.lines() { (l) br; } }
                            div.submit-row.two-buttons {
                                button.button.primary type="submit" name="choice" value="edit" { "Edit address" }
                                button.button.ghost type="submit" name="choice" value="entered" { "It's correct, save it" }
                            }
                        }
                    }
                }
            }
        }
    }
}

fn address_from(values: &HashMap<String, String>, prefix: &str) -> Address {
    let g = |k: &str| values.get(&format!("{prefix}{k}")).map(|v| v.trim().to_string()).unwrap_or_default();
    let mut country = g("country");
    if country == "other" {
        country = g("country_other");
    }
    Address { addr1: g("addr1"), addr2: g("addr2"), city: g("city"), region: g("region"), postcode: g("postcode"), country }
}

async fn save_address(
    State(st): State<AppState>,
    Path(token): Path<String>,
    Extension(ClientIp(ip)): Extension<ClientIp>,
    headers: HeaderMap,
    Form(pairs): Form<Vec<(String, String)>>,
) -> AppResult {
    let Some(ctx) = load(&st, &token).await? else { return Ok(ui::not_found()) };
    if ctx.phase == Phase::RsvpClosed || pairs.len() > 50 {
        return Ok(redirect(&format!("/i/{token}")));
    }
    let values: HashMap<String, String> = pairs.into_iter().collect();
    let entered = address_from(&values, "");
    let choice = values.get("choice").map(String::as_str);
    let show = |view: AddressView, status: StatusCode| {
        let v = rsvp::current_values(&ctx.guests, &ctx.responses, &ctx.h);
        Ok((status, render(&st, &token, &ctx, &v, &BTreeMap::new(), false, &view)).into_response())
    };

    if choice == Some("edit") {
        return show(AddressView::Form { entered, error: "Update the address below and save it again.".into() }, StatusCode::OK);
    }
    if let Err(msg) = entered.validate_fields() {
        return show(AddressView::Form { entered, error: msg }, StatusCode::UNPROCESSABLE_ENTITY);
    }

    let (address, status) = match choice {
        Some("suggested") => (address_from(&values, "s_"), "verified"),
        Some("entered") => (entered, "guest-confirmed"),
        _ => {
            // Check with Google (rate limited per household); any failure saves it as entered.
            let checked = match &st.env.google_maps_key {
                Some(key) if st.limiter.check(&format!("addrcheck:{}", ctx.h.id), 10.0, 10.0 / 3600.0) => {
                    match address::check(&st.http, key, &entered).await {
                        Ok(c) => c,
                        Err(e) => {
                            tracing::warn!(error = %e, household = ctx.h.id, "address validation unavailable");
                            None
                        }
                    }
                }
                _ => None,
            };
            match checked {
                None => (entered, "unverified"),
                Some(c) if c.action == AddrAction::Accept && c.suggestion.same_as(&entered) => (c.suggestion, "verified"),
                // Picked from search: Google already knows it; just take the tidy postal version.
                Some(c) if values.get("picked").is_some_and(|v| v == "1") && matches!(c.action, AddrAction::Accept | AddrAction::Confirm) => (c.suggestion, "verified"),
                Some(c) if c.action == AddrAction::Fix => {
                    let mut detail = vec![];
                    if !c.unconfirmed.is_empty() {
                        detail.push(format!("We couldn't confirm: {}.", c.unconfirmed.join(", ")));
                    }
                    if !c.missing.is_empty() {
                        detail.push(format!("It seems to be missing: {}.", c.missing.join(", ")));
                    }
                    return show(AddressView::Confirm { entered, suggestion: None, action: c.action, detail: detail.join(" ") }, StatusCode::OK);
                }
                Some(c) => {
                    return show(AddressView::Confirm { entered, suggestion: Some(c.suggestion), action: c.action, detail: String::new() }, StatusCode::OK);
                }
            }
        }
    };
    if address.validate_fields().is_err() {
        return Ok(redirect(&format!("/i/{token}#address")));
    }
    let sub = address::Submission { address, status: status.into() };
    let ip_hash = crate::tokens::ip_hash(&st.env.secret, &ip);
    let ua = headers.get(header::USER_AGENT).and_then(|v| v.to_str().ok()).unwrap_or("").to_string();
    let (journal, h) = (st.journal.clone(), ctx.h.clone());
    match st.db.call(move |c| address::submit(c, &journal, &h, &sub, &ip_hash, &ua)).await {
        Ok(()) => Ok(redirect(&format!("/i/{token}?address=1#address"))),
        Err(e) => {
            tracing::error!(error = ?e, household = ctx.h.id, "ADDRESS SAVE FAILED");
            Ok(ui::error_page(StatusCode::SERVICE_UNAVAILABLE, "We couldn't save your address", "Sorry, please go back and try again in a minute, or send it to us directly."))
        }
    }
}

#[derive(Deserialize)]
struct SuggestQuery {
    q: Option<String>,
    s: Option<String>,
    id: Option<String>,
}

/// Address search, proxied to Google so the guest's browser never talks to a third party.
async fn address_suggest(State(st): State<AppState>, Path(token): Path<String>, Query(q): Query<SuggestQuery>) -> AppResult {
    let Some(ctx) = load(&st, &token).await? else { return Ok(ui::not_found()) };
    let (Some(key), Some(input), Some(session)) = (st.env.google_maps_key.clone(), q.q, q.s) else {
        return Ok(axum::Json(serde_json::json!({"suggestions": [], "available": false})).into_response());
    };
    let input: String = input.trim().chars().take(200).collect();
    if input.chars().count() < 3 || !address::valid_session(&session) {
        return Ok(axum::Json(serde_json::json!({"suggestions": []})).into_response());
    }
    if !st.limiter.check(&format!("places:{}", ctx.h.id), 300.0, 300.0 / 3600.0) {
        return Ok((StatusCode::TOO_MANY_REQUESTS, axum::Json(serde_json::json!({"error": "slow down"}))).into_response());
    }
    let prefer = if ctx.h.australia { "au" } else { "us" };
    match address::autocomplete(&st.http, &key, &input, &session, prefer).await {
        Ok(list) => Ok(axum::Json(serde_json::json!({"suggestions": list})).into_response()),
        Err(e) => {
            tracing::warn!(error = %e, "address autocomplete failed");
            Ok(axum::Json(serde_json::json!({"suggestions": [], "available": false})).into_response())
        }
    }
}

async fn address_place(State(st): State<AppState>, Path(token): Path<String>, Query(q): Query<SuggestQuery>) -> AppResult {
    let Some(ctx) = load(&st, &token).await? else { return Ok(ui::not_found()) };
    let (Some(key), Some(id), Some(session)) = (st.env.google_maps_key.clone(), q.id, q.s) else {
        return Ok((StatusCode::BAD_REQUEST, axum::Json(serde_json::json!({"error": "missing"}))).into_response());
    };
    if !address::valid_place_id(&id) || !address::valid_session(&session) {
        return Ok((StatusCode::BAD_REQUEST, axum::Json(serde_json::json!({"error": "invalid"}))).into_response());
    }
    if !st.limiter.check(&format!("places:{}", ctx.h.id), 300.0, 300.0 / 3600.0) {
        return Ok((StatusCode::TOO_MANY_REQUESTS, axum::Json(serde_json::json!({"error": "slow down"}))).into_response());
    }
    match address::place(&st.http, &key, &id, &session).await {
        Ok(a) => Ok(axum::Json(serde_json::json!({"address": a, "lines": a.lines()})).into_response()),
        Err(e) => {
            tracing::warn!(error = %e, "place details failed");
            Ok((StatusCode::BAD_GATEWAY, axum::Json(serde_json::json!({"error": "lookup failed"}))).into_response())
        }
    }
}

//! Guest-facing pages. Each household has one secret link: /i/{token}.

use std::collections::{BTreeMap, HashMap};

use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::IntoResponse;
use axum::routing::get;
use axum::{Extension, Form, Router};
use maud::{Markup, PreEscaped, html};
use serde::Deserialize;

use super::ui::{self, guest_page};
use super::{AppResult, AppState, ClientIp, redirect};
use crate::prose;
use crate::assets::{self, Kind, Layout, words};
use crate::config::{Config, parse_local};
use crate::rsvp::{self, field};
use crate::store::{self, Guest, Household, Phase, Response as Resp};

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/privacy", get(privacy))
        .route("/hashtag", get(hashtag))
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
                p.kicker { (st.cfg.prose.t("website.landing.kicker")) }
                h1.names { (st.cfg.couple.a_first) span.amp { "&" } (st.cfg.couple.b_first) }
                p.lede { (st.cfg.prose.t("website.landing.text")) }
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
    photo: Option<crate::site::Photo>,
    calendar: store::CalendarDownloads,
}

/// Remembers which household this browser belongs to, so the rest of the site
/// (schedule, travel, gallery) works without the invitation link in the URL.
pub const GUEST_COOKIE: &str = "__Host-rsvp_guest";

/// What the footer hashtag means (copy.yaml `hashtag`). Public: there's nothing private in it.
async fn hashtag(State(st): State<AppState>) -> axum::response::Response {
    let cfg = &st.cfg;
    let Some(title) = cfg.prose.opt("hashtag.title") else { return ui::not_found() };
    let owned = prose::vars(cfg, "");
    let body = prose::fill(&cfg.prose.t("hashtag.body"), &prose::borrow(&owned));
    guest_page(
        cfg,
        &format!("{title} | {}", cfg.title()),
        None,
        html! {
            h1.page-title.center-text { (title) }
            article.prose.center-text { (PreEscaped(crate::site::markdown(&body))) }
        },
    )
    .into_response()
}

/// The privacy policy: public, so anyone (including Google's app review) can read it.
async fn privacy(State(st): State<AppState>) -> axum::response::Response {
    let cfg = &st.cfg;
    let owned = prose::vars(cfg, "");
    let vars = prose::borrow(&owned);
    let body = prose::fill(&cfg.prose.t("privacy.body"), &vars);
    guest_page(
        cfg,
        &format!("{} | {}", cfg.prose.t("privacy.title"), cfg.title()),
        None,
        html! {
            h1.page-title.center-text { (cfg.prose.t("privacy.title")) }
            article.prose { (PreEscaped(crate::site::markdown(&body))) }
        },
    )
    .into_response()
}

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
            let photo = crate::site::home_photo(c)?;
            let calendar = store::calendar_downloads(c, h.id)?;
            Ok(Some(Ctx { h, guests, responses, phase, pages, photo, calendar }))
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
        let p = &st.cfg.prose;
        return Ok(ui::error_page(StatusCode::BAD_REQUEST, &p.t("website.errors.form_too_large_title"), &p.t("website.errors.form_too_large_text")));
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
                &st.cfg.prose.t("website.errors.rsvp_failed_title"),
                &st.cfg.prose.t("website.errors.rsvp_failed_text"),
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
    let pr = &cfg.prose;
    let owned = prose::vars(cfg, &h.greeting);
    let vars = prose::borrow(&owned);
    let date = cfg.wedding_date().ok();
    let events: Vec<_> = h.events.iter().filter_map(|e| cfg.event(e)).collect();

    let body = html! {
        // Names and date, then the photo.
        section.hero {
            div.hero-text {
                p.kicker { @if ctx.phase == Phase::SaveTheDate { (pr.t("website.kicker_save_the_date")) } @else { (pr.t("website.kicker_invited")) } }
                @if let Some(w) = pr.opt("website.the_wedding_of") { p.wedding-of { (w) } }
                h1.names { (cfg.couple.a_first) span.amp { "&" } (cfg.couple.b_first) }
                div.rule { span {} i {} span {} }
                @if let Some(d) = date {
                    p.date { (words::weekday_name(d.weekday())) " · " (d.day()) " " (words::month_name(d.month())) " · " (d.year()) }
                }
                p.place { (cfg.wedding.location) }
            }
            @if let Some(ph) = &ctx.photo {
                figure.home-photo {
                    img src={ "/media/" (ph.large()) } alt=(if ph.caption.is_empty() { cfg.title() } else { ph.caption.clone() })
                        width=(ph.width) height=(ph.height) fetchpriority="high";
                    @if !ph.caption.is_empty() { figcaption { (ph.caption) } }
                }
            }
        }

        section.letter {
            p.salutation { (pr.f("website.salutation", &[("greeting", &h.greeting)])) }
            @match ctx.phase {
                Phase::SaveTheDate => {
                    @for para in pr.list("website.save_the_date", &vars) { p { (prose::rich(&para)) } }
                }
                _ => {
                    @for para in pr.list("website.invited", &vars) { p { (prose::rich(&para)) } }
                    @if !closed {
                        p { (prose::rich(&pr.f("website.rsvp_by", &vars))) }
                    }
                }
            }
            @if let Some(n) = pr.opt("notes.celebration") { p.celebration { (n) } }
            div.actions {
                // After a change to their events, say so, so they know to add them again.
                @let current: Vec<(String, i64)> = events.iter().filter(|e| e.dated()).map(|e| (e.slug.clone(), st.ics_sequence.get(&e.slug).copied().unwrap_or(0))).collect();
                @let stale = ctx.calendar.count > 0 && current.iter().any(|(slug, seq)| ctx.calendar.last.get(slug) != Some(seq));
                a.button.ghost.has-note[stale] href={ "/i/" (token) "/calendar.ics" } {
                    (pr.t("website.add_to_calendar"))
                    @if stale { span.button-note { (pr.t("website.calendar_updated")) } }
                }
            }
        }

        // Ask for a mailing address before the formal invitation goes out (and whenever it's missing).
        @if !closed && (ctx.phase == Phase::SaveTheDate || h.address_lines().is_empty() || !matches!(addr, AddressView::Current { .. })) {
            (address_section(cfg, token, h, addr))
        }

        @if ctx.phase != Phase::SaveTheDate {
            section.events {
                div.rule.full aria-hidden="true" { span {} i {} span {} }
                h2.section-title { (pr.t("website.events.title")) }
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
                                p.event-when { (pr.t("website.events.date_tba")) }
                                p.event-time { (pr.t("website.events.rsvp_when_set")) }
                            }
                            p.event-venue { (e.venue) }
                            @if !e.address.is_empty() {
                                p.event-address {
                                    a href=(e.map_url()) rel="noopener noreferrer" target="_blank" { (e.address) }
                                }
                            }
                            @if let Some(d) = &e.dress { p.event-dress { (pr.f("website.events.dress", &[("dress", d)])) } }
                            @if let Some(n) = &e.note { p.event-note { (n) } }
                        }
                    }
                }
            }

            section #reply .reply {
                div.rule.full aria-hidden="true" { span {} i {} span {} }
                h2.section-title { (pr.t("website.reply.title")) }
                @if just_sent {
                    div.notice.success role="status" {
                        p.notice-title { (pr.t("website.reply.thanks_title")) }
                        p { (prose::rich(&pr.f("website.reply.thanks_text", &vars))) }
                    }
                }
                @if closed {
                    (summary(cfg, ctx))
                    p.muted { (pr.t("website.reply.closed")) }
                } @else {
                    @if !just_sent { @if let Some(at) = &h.rsvp_at {
                        div.notice role="status" { p { (pr.f("website.reply.received", &[("when", &ui::when(cfg, at))])) } }
                    } }
                    (form(cfg, token, ctx, values, errors))
                }
            }
        }

    };
    let nav = ui::GuestNav { home: format!("/i/{token}"), pages: ctx.pages.clone(), active: "home".into() };
    guest_page(cfg, &format!("{} | {}", h.greeting, cfg.title()), Some(&nav), body)
}

fn form(cfg: &Config, token: &str, ctx: &Ctx, values: &HashMap<String, String>, errors: &BTreeMap<String, String>) -> Markup {
    let pr = &cfg.prose;
    let v = |k: &str| values.get(k).cloned().unwrap_or_default();
    let events: Vec<_> = ctx.h.events.iter().filter_map(|e| cfg.event(e)).collect();
    let err = |k: &str| errors.get(k).map(|m| html! { p.field-error { (m) } });
    html! {
        @if !errors.is_empty() {
            div.notice.error role="alert" {
                p.notice-title { (pr.t("website.reply.fix_first")) }
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
                                label.choice { input type="radio" name=(att_key) value="yes" checked[cur == "yes"]; span { (pr.t("website.reply.accept")) } }
                                label.choice { input type="radio" name=(att_key) value="no" checked[cur == "no"]; span { (pr.t("website.reply.decline")) } }
                            }
                            @if let Some(m) = err(&att_key) { (m) }
                            @let meals = rsvp::meals_for(cfg, g.is_child);
                            @if e.meal && !meals.is_empty() {
                                @let mk = field::meal(g.id, &e.slug);
                                @let curm = v(&mk);
                                div.meals data-show-when=(att_key) {
                                    p.answer-sub { (pr.t("website.reply.dinner")) }
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
                            p.answer-sub { (pr.t("website.reply.plus_one_who")) }
                            div.two {
                                label.field { span { (pr.t("website.reply.first_name")) } input type="text" name=(field::first(g.id)) value=(v(&field::first(g.id))) maxlength="80" autocomplete="off"; }
                                label.field { span { (pr.t("website.reply.last_name")) } input type="text" name=(field::last(g.id)) value=(v(&field::last(g.id))) maxlength="80" autocomplete="off"; }
                            }
                            @if let Some(m) = err(&field::first(g.id)) { (m) }
                        }
                    }
                    @if cfg.rsvp.ask_dietary {
                        label.field data-show-when=(att_keys) {
                            span { (pr.t("website.reply.dietary")) }
                            input type="text" name=(field::diet(g.id)) value=(v(&field::diet(g.id))) maxlength="500" placeholder=(pr.t("website.reply.dietary_placeholder"));
                        }
                    }
                }
            }

            fieldset.extras {
                legend { (pr.t("website.reply.extras_title")) }
                @if cfg.rsvp.ask_song {
                    label.field { span { (pr.t("website.reply.song")) } input type="text" name="song" value=(v("song")) maxlength="200"; }
                }
                @if cfg.rsvp.ask_accommodation {
                    label.field {
                        span { (pr.t("website.reply.accommodation")) }
                        select name="accommodation" {
                            @for (k, l) in pr.options("website.reply.accommodation_options") { option value=(k) selected[v("accommodation") == k] { (l) } }
                        }
                    }
                }
                @if cfg.rsvp.ask_transport {
                    div.field {
                        span { (cfg.rsvp.transport_question) }
                        div.choice-row {
                            @for (k, l) in pr.options("website.reply.transport_options") {
                                label.choice { input type="radio" name="transport" value=(k) checked[v("transport") == k]; span { (l) } }
                            }
                        }
                    }
                }
                @if cfg.rsvp.ask_message {
                    label.field { span { (pr.t("website.reply.message")) } textarea name="message" rows="4" maxlength="2000" { (v("message")) } }
                }
            }
            div.submit-row {
                button.button.primary type="submit" { @if ctx.h.rsvp_at.is_some() { (pr.t("website.reply.update")) } @else { (pr.t("website.reply.send")) } }
            }
        }
    }
}

fn summary(cfg: &Config, ctx: &Ctx) -> Markup {
    let pr = &cfg.prose;
    let by: HashMap<(i64, &str), &Resp> = ctx.responses.iter().map(|r| ((r.guest_id, r.event_slug.as_str()), r)).collect();
    html! {
        div.summary {
            @if ctx.h.rsvp_at.is_none() { p { (pr.t("website.reply.none_received")) } }
            @for g in &ctx.guests {
                div.summary-person {
                    p.summary-name { (g.full_name()) }
                    ul {
                        @for slug in ctx.h.events.iter().filter(|s| g.plus_one_of.is_some() || g.invited(&ctx.h, s)) {
                            @if let Some(e) = cfg.event(slug) {
                                li {
                                    (e.name) ": "
                                    @match by.get(&(g.id, slug.as_str())) {
                                        Some(r) if r.attending => { (pr.t("website.reply.attending")) @if let Some(m) = cfg.meal(&r.meal) { " · " (m.name) } }
                                        Some(_) => { (pr.t("website.reply.not_attending")) }
                                        None => { (pr.t("website.reply.no_reply")) }
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
    let mut held = std::collections::BTreeMap::new();
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
        // SEQUENCE goes up when the event changes, so calendars update it rather than keep the old one.
        let seq = st.ics_sequence.get(&e.slug).copied().unwrap_or(0);
        held.insert(e.slug.clone(), seq);
        ics.push_str(&format!("DTSTAMP:{stamp}\r\nSEQUENCE:{seq}\r\nDTSTART:{}\r\nDTEND:{}\r\n", start.format(&fmt)?, end.format(&fmt)?));
        ics.push_str(&fold(&format!("SUMMARY:{}", ics_escape(&format!("{} & {}: {}", cfg.couple.a_first, cfg.couple.b_first, e.name)))));
        ics.push_str(&fold(&format!("LOCATION:{}", ics_escape(&format!("{}, {}", e.venue, e.address)))));
        ics.push_str(&fold(&format!("DESCRIPTION:{}", ics_escape(&format!("Details and RSVP: {}", assets::link(cfg, &token))))));
        ics.push_str("END:VEVENT\r\n");
    }
    ics.push_str("END:VCALENDAR\r\n");
    let hid = ctx.h.id;
    st.db.call(move |c| store::record_calendar_download(c, hid, &held)).await?;
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

fn address_fields(pr: &prose::Prose, a: &Address, australia: bool) -> Markup {
    let default_country = if australia { "Australia" } else { "United States" };
    let country = if a.country.is_empty() { default_country } else { a.country.as_str() };
    let known = address::COUNTRIES.iter().any(|(n, _)| *n == country);
    html! {
        // Search box (shown by guest.js; without JavaScript the manual fields below are used).
        input type="hidden" name="picked" value="";
        div.address-search hidden {
            label.field.search-field {
                span { (pr.t("website.address.search_label")) }
                input.addr-search type="text" autocomplete="off" spellcheck="false" placeholder=(pr.t("website.address.search_placeholder"))
                    role="combobox" aria-autocomplete="list" aria-expanded="false" maxlength="200";
            }
            ul.addr-results role="listbox" hidden {}
            // guest.js shows these while searching
            p.addr-status.muted.small aria-live="polite"
                data-no-matches=(pr.t("website.address.search_no_matches")) data-broken=(pr.t("website.address.search_broken"))
                data-looking-up=(pr.t("website.address.search_looking_up")) data-lookup-failed=(pr.t("website.address.search_lookup_failed"))
                data-pick=(pr.t("website.address.search_pick")) {}
            div.addr-picked hidden {
                address.postal {}
                button.linkish.addr-change type="button" { (pr.t("website.address.change")) }
            }
            p.small { button.linkish.addr-manual type="button" { (pr.t("website.address.manual")) } }
        }
        label.field.unit-field { span { (pr.t("website.address.unit")) " " small.muted { (pr.t("website.address.optional")) } } input name="addr2" value=(a.addr2) autocomplete="address-line2" maxlength="200"; }
        div.manual-fields {
        p.small.addr-back-wrap hidden { button.linkish.addr-back type="button" { (pr.t("website.address.back_to_search")) } }
        label.field { span { (pr.t("website.address.street")) } input name="addr1" value=(a.addr1) autocomplete="address-line1" maxlength="200"; }
        div.two {
            label.field { span { (pr.t("website.address.city")) } input name="city" value=(a.city) autocomplete="address-level2" required maxlength="100"; }
            label.field { span { (pr.t("website.address.state")) } input name="region" value=(a.region) autocomplete="address-level1" maxlength="100"; }
        }
        div.two {
            label.field { span { (pr.t("website.address.postcode")) } input name="postcode" value=(a.postcode) autocomplete="postal-code" maxlength="20"; }
            label.field {
                span { (pr.t("website.address.country")) }
                select name="country" autocomplete="country-name" {
                    @for (n, _) in address::COUNTRIES { option value=(n) selected[*n == country] { (n) } }
                    option value="other" selected[!known] { (pr.t("website.address.country_other")) }
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

fn address_section(cfg: &Config, token: &str, h: &Household, view: &AddressView) -> Markup {
    let pr = &cfg.prose;
    let action = format!("/i/{token}/address#address");
    let current = Address::from_household(h);
    html! {
        // The divider sits outside the (narrower) address section so it matches the others.
        div.rule.full aria-hidden="true" { span {} i {} span {} }
        section #address .address {
            h2.section-title { (pr.t("website.address.title")) }
            @match view {
                AddressView::Current { saved } => {
                    @if *saved {
                        div.notice.success role="status" { p.notice-title { (pr.t("website.address.thanks")) } }
                    }
                    @if !current.lines().is_empty() && !*saved {
                        p.center-text { (pr.t("website.address.sending_to")) }
                    } @else if current.lines().is_empty() {
                        p.center-text { (pr.t("website.address.ask")) }
                    }
                    @if !current.lines().is_empty() {
                        address.postal { @for l in current.lines() { (l) br; } }
                        details.edit-address {
                            summary { (pr.t("website.address.update")) }
                            form.address-form method="post" action=(action) { (address_fields(pr, &current, h.australia)) div.submit-row { button.button.primary type="submit" { (pr.t("website.address.save")) } } }
                        }
                    } @else {
                        form.address-form method="post" action=(action) { (address_fields(pr, &current, h.australia)) div.submit-row { button.button.primary type="submit" { (pr.t("website.address.save")) } } }
                    }
                }
                AddressView::Form { entered, error } => {
                    div.notice.error role="alert" { p { (error) } }
                    form.address-form method="post" action=(action) { (address_fields(pr, entered, h.australia)) div.submit-row { button.button.primary type="submit" { (pr.t("website.address.save")) } } }
                }
                AddressView::Confirm { entered, suggestion, action: act, detail } => {
                    form.address-confirm method="post" action=(action) {
                        (hidden_address("", entered))
                        @if let Some(s) = suggestion.as_ref().filter(|_| *act != AddrAction::Fix) {
                            (hidden_address("s_", s))
                            p.center-text { @if *act == AddrAction::AddUnit { (pr.t("website.address.which_unit")) } @else { (pr.t("website.address.which_version")) } }
                            div.address-choices {
                                button.address-choice.recommended type="submit" name="choice" value="suggested" {
                                    span.choice-label { (pr.t("website.address.suggested")) }
                                    address.postal { @for l in s.lines() { (l) br; } }
                                }
                                button.address-choice type="submit" name="choice" value="entered" {
                                    span.choice-label { (pr.t("website.address.as_typed")) }
                                    address.postal { @for l in entered.lines() { (l) br; } }
                                }
                            }
                        } @else {
                            div.notice.error role="alert" {
                                p.notice-title { (pr.t("website.address.not_found")) }
                                @if !detail.is_empty() { p { (detail) } }
                            }
                            address.postal { @for l in entered.lines() { (l) br; } }
                            div.submit-row.two-buttons {
                                button.button.primary type="submit" name="choice" value="edit" { (pr.t("website.address.edit")) }
                                button.button.ghost type="submit" name="choice" value="entered" { (pr.t("website.address.keep")) }
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
        return show(AddressView::Form { entered, error: st.cfg.prose.t("website.address.update_and_save") }, StatusCode::OK);
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
                        detail.push(st.cfg.prose.f("website.address.not_confirmed", &[("parts", &c.unconfirmed.join(", "))]));
                    }
                    if !c.missing.is_empty() {
                        detail.push(st.cfg.prose.f("website.address.missing", &[("parts", &c.missing.join(", "))]));
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
            Ok(ui::error_page(StatusCode::SERVICE_UNAVAILABLE, &st.cfg.prose.t("website.errors.address_failed_title"), &st.cfg.prose.t("website.errors.address_failed_text")))
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

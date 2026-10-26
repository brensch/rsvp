//! Guest website pages (schedule, travel, gallery, custom pages) and photo files.
//! Only browsers that have opened a valid invitation link (see `guest::GUEST_COOKIE`)
//! can see them; admins can also view photos.

use axum::Router;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use maud::{Markup, PreEscaped, html};

use super::guest::{guest_cookie, nav_pages};
use super::ui::{self, GuestNav, guest_page};
use super::{AppResult, AppState};
use crate::assets::words;
use crate::site::{self, Photo};
use crate::store::{self, Household};

pub fn routes() -> Router<AppState> {
    Router::new().route("/media/{file}", get(media)).route("/e/{file}", get(email_card)).route("/{slug}", get(page))
}

/// The household this browser belongs to, if it has opened a valid invitation link.
async fn household(st: &AppState, headers: &HeaderMap) -> anyhow::Result<Option<(Household, String)>> {
    let Some(token) = guest_cookie(headers) else { return Ok(None) };
    let t = token.clone();
    Ok(st.db.call(move |c| store::household_by_token(c, &t)).await?.map(|h| (h, token)))
}

fn invite_only() -> Response {
    ui::error_page(
        StatusCode::FORBIDDEN,
        "This page is for our guests",
        "Please open the personal link or QR code from your invitation first. After that, this page will work on this device.",
    )
}

async fn page(State(st): State<AppState>, Path(slug): Path<String>, headers: HeaderMap) -> AppResult {
    if !site::valid_slug(&slug) {
        return Ok(ui::not_found());
    }
    let Some((h, token)) = household(&st, &headers).await? else { return Ok(invite_only()) };
    let (page, pages, photos) = {
        let (slug, h2) = (slug.clone(), h.clone());
        st.db.call(move |c| Ok((site::get_page(c, &slug)?, nav_pages(c, &h2)?, site::photos(c)?))).await?
    };
    let Some(page) = page.filter(|p| p.published && p.shown_to(h.australia)) else { return Ok(ui::not_found()) };
    let cfg = &st.cfg;
    let body = html! {
        section.page-head {
            h1.page-title { (page.title) }
            div.rule { span {} i {} span {} }
        }
        @if !site::for_household(&page.body, h.australia).trim().is_empty() {
            article.prose { (PreEscaped(site::markdown(&site::for_household(&page.body, h.australia)))) }
        }
        @match page.slug.as_str() {
            "schedule" => { (schedule(cfg, &h)) }
            "gallery" => { (gallery(&photos)) }
            _ => {}
        }
    };
    let nav = GuestNav { home: format!("/i/{token}"), pages, active: page.slug.clone() };
    Ok(guest_page(cfg, &format!("{} | {}", page.title, cfg.title()), Some(&nav), body).into_response())
}

/// The official events this household is invited to, in date order.
fn schedule(cfg: &crate::config::Config, h: &Household) -> Markup {
    let mut events: Vec<_> = h.events.iter().filter_map(|e| cfg.event(e)).collect();
    events.sort_by_key(|e| (e.start().is_none(), e.start()));
    html! {
        @if !events.is_empty() {
            ol.timeline {
                @for e in events {
                    li.timeline-item {
                        div.timeline-when {
                            @if let Some(t) = e.start() {
                                span.t-day { (words::weekday_name(t.date().weekday())) }
                                span.t-date { (t.date().day()) " " (words::month_name(t.date().month())) }
                                span.t-time {
                                    (words::time_short(t))
                                    @if let Some(end) = e.ends.as_deref().and_then(|x| crate::config::parse_local(x).ok()) { " – " (words::time_short(end)) }
                                }
                            } @else {
                                span.t-date { "Date to be announced" }
                            }
                        }
                        div.timeline-what {
                            h3 { (e.name) }
                            p.t-venue { (e.venue) }
                            @if !e.address.is_empty() {
                                p.t-address {
                                    a href=(e.map_url()) rel="noopener noreferrer" target="_blank" { (e.address) }
                                }
                            }
                            @if let Some(d) = &e.dress { p.t-dress { "Dress: " (d) } }
                            @if let Some(n) = &e.note { p.t-note { (n) } }
                        }
                    }
                }
            }
        }
    }
}

fn gallery(photos: &[Photo]) -> Markup {
    html! {
        @if photos.is_empty() {
            p.center-text.muted { "Photos are coming soon." }
        } @else {
            div.gallery {
                @for p in photos {
                    figure.photo {
                        a href={ "/media/" (p.large()) } target="_blank" rel="noopener" {
                            img src={ "/media/" (p.thumb()) } alt=(if p.caption.is_empty() { "Photo" } else { &p.caption }) loading="lazy"
                                width=(p.width.min(800)) height=(if p.width > 800 { p.height * 800 / p.width } else { p.height });
                        }
                        @if !p.caption.is_empty() { figcaption { (p.caption) } }
                    }
                }
            }
        }
    }
}

/// The card picture inside an email. Loading it is how we know the email was opened. Anyone
/// with the (unguessable, per-address) link can load it; it shows only the card itself.
async fn email_card(State(st): State<AppState>, Path(file): Path<String>) -> AppResult {
    let Some(id) = file.strip_suffix(".png").filter(|id| crate::tokens::plausible_token(id)).map(String::from) else {
        return Ok(ui::not_found());
    };
    let Some(send) = st.db.call(move |c| store::record_open(c, &id)).await? else { return Ok(ui::not_found()) };
    if send.opens == 1 {
        tracing::info!(household = send.household_id, kind = %send.kind, test = send.test, "email opened");
    }
    match tokio::fs::read(crate::email::card_file(&st.env, send.household_id, &send.kind)).await {
        Ok(bytes) => Ok(([(header::CONTENT_TYPE, "image/png"), (header::CACHE_CONTROL, "no-store, private")], bytes).into_response()),
        Err(_) => Ok(ui::not_found()),
    }
}

/// Photo files: invited guests and signed-in admins only.
async fn media(State(st): State<AppState>, Path(file): Path<String>, headers: HeaderMap) -> AppResult {
    if !site::valid_media_name(&file) {
        return Ok(ui::not_found());
    }
    let allowed = household(&st, &headers).await?.is_some()
        || super::auth::session_from(&st, &headers).await?.is_some_and(|s| s.user.is_active());
    if !allowed {
        return Ok(invite_only());
    }
    let path = site::media_dir(&st.env.data_dir).join(&file);
    match tokio::fs::read(&path).await {
        Ok(bytes) => Ok((
            [(header::CONTENT_TYPE, "image/jpeg"), (header::CACHE_CONTROL, "private, max-age=604800, immutable")],
            bytes,
        )
            .into_response()),
        Err(_) => Ok(ui::not_found()),
    }
}

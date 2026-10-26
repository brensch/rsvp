//! Shared HTML shells, static assets, small view helpers.

use std::sync::LazyLock;

use axum::extract::Path;
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};
use maud::{DOCTYPE, Markup, PreEscaped, html};

use crate::config::Config;
use crate::store::User;

static STATIC: &[(&str, &str, &str)] = &[
    ("site.css", "text/css; charset=utf-8", include_str!("../../static/site.css")),
    ("admin.css", "text/css; charset=utf-8", include_str!("../../static/admin.css")),
    ("passkey.js", "text/javascript; charset=utf-8", include_str!("../../static/passkey.js")),
    ("admin.js", "text/javascript; charset=utf-8", include_str!("../../static/admin.js")),
    ("guest.js", "text/javascript; charset=utf-8", include_str!("../../static/guest.js")),
    // Chrome/Edge prepare a guest page as soon as someone hovers a link to it, so the click is instant.
    ("speculation.json", "application/speculationrules+json", include_str!("../../static/speculation.json")),
];

/// Cache-busting version derived from the static file contents.
static VERSION: LazyLock<String> = LazyLock::new(|| {
    let mut all = String::new();
    for (_, _, body) in STATIC {
        all.push_str(body);
    }
    crate::tokens::sha256_hex(&all)[..10].to_string()
});

pub fn asset(name: &str) -> String {
    format!("/static/{name}?v={}", *VERSION)
}

pub async fn static_file(Path(file): Path<String>) -> Response {
    match STATIC.iter().find(|(n, _, _)| *n == file) {
        Some((_, ct, body)) => {
            ([(header::CONTENT_TYPE, *ct), (header::CACHE_CONTROL, "public, max-age=31536000, immutable")], *body).into_response()
        }
        None => not_found(),
    }
}

pub async fn font_file(Path(file): Path<String>) -> Response {
    match crate::assets::WEB_FONTS.iter().find(|(n, _)| *n == file) {
        Some((_, bytes)) => {
            ([(header::CONTENT_TYPE, "font/woff2"), (header::CACHE_CONTROL, "public, max-age=31536000, immutable")], *bytes).into_response()
        }
        None => not_found(),
    }
}

fn head(title: &str, css: &str) -> Markup {
    // Fetch the fonts straight away, alongside the CSS, so text draws once in the right font.
    let fonts: &[&str] = if css == "site.css" {
        // The faces every guest page draws above the fold (EB Garamond italic loads only if used).
        &["CormorantGaramond-v2.woff2", "CormorantGaramond-Italic-v2.woff2", "EBGaramond-v2.woff2"]
    } else {
        &["CormorantGaramond-v2.woff2", "CormorantGaramond-Italic-v2.woff2"]
    };
    html! {
        meta charset="utf-8";
        meta name="viewport" content="width=device-width, initial-scale=1, viewport-fit=cover";
        meta name="robots" content="noindex, nofollow";
        meta name="referrer" content="same-origin";
        meta name="color-scheme" content="light";
        title { (title) }
        @for f in fonts { link rel="preload" href={ "/fonts/" (f) } as="font" type="font/woff2" crossorigin; }
        link rel="stylesheet" href=(asset(css));
    }
}

/// The wattle and olive wreath (tools/draw_emblem.py), placed straight into each page so it paints
/// with the text instead of popping in after a separate image load.
static EMBLEM_SVG: LazyLock<String> = LazyLock::new(|| {
    let raw = include_str!("../../templates/art/emblem.svg");
    let svg = &raw[raw.find("<svg").unwrap_or(0)..];
    svg.replacen("<svg ", "<svg aria-hidden=\"true\" focusable=\"false\" ", 1)
});

/// Guest site navigation: the household's own invitation link plus published pages.
pub struct GuestNav {
    pub home: String,
    pub pages: Vec<(String, String)>,
    pub active: String,
}

pub fn guest_page(cfg: &Config, title: &str, nav: Option<&GuestNav>, body: Markup) -> Markup {
    let initials: Vec<char> = cfg.monogram().chars().filter(|c| c.is_alphanumeric()).collect();
    html! {
        (DOCTYPE)
        html lang="en" {
            head { (head(title, "site.css")) }
            body.guest {
                // A quiet strip of links at the very top, then the emblem as the crest.
                @if let Some(n) = nav.filter(|n| !n.pages.is_empty()) {
                    nav.site-nav aria-label="Wedding website" {
                        a href=(n.home) aria-current=[(n.active == "home").then_some("page")] { (cfg.prose.t("website.nav_invitation")) }
                        @for (slug, title) in &n.pages {
                            a href={ "/" (slug) } aria-current=[(n.active == *slug).then_some("page")] { (title) }
                        }
                    }
                }
                header.site-head {
                    a.monogram href=(nav.map(|n| n.home.as_str()).unwrap_or("/")) aria-label="Home" {
                        (PreEscaped(EMBLEM_SVG.as_str()))
                        span.initials {
                            @if let [a, b] = initials.as_slice() { (a) span.amp { "&" } (b) } @else { (cfg.monogram()) }
                        }
                    }
                }
                main { (body) }
                @if let Some(f) = cfg.prose.opt("website.footer") {
                    footer.site-foot {
                        div.rule.full aria-hidden="true" { span {} i {} span {} }
                        // The footer line links to its explanation when copy.yaml has one.
                        p { @if cfg.prose.opt("hashtag.title").is_some() { a href="/hashtag" { (f) } } @else { (f) } }
                    }
                }
                script src=(asset("guest.js")) defer {}
            }
        }
    }
}

pub struct Nav {
    pub user: User,
    pub csrf: String,
    pub pending_users: i64,
}

pub fn admin_page(cfg: &Config, nav: &Nav, active: &str, title: &str, flash: Option<&str>, body: Markup) -> Response {
    let link = |href: &str, key: &str, label: &str, badge: i64| {
        html! {
            a href=(href) class=@if key == active { "active" } {
                (label)
                @if badge > 0 { span.badge { (badge) } }
            }
        }
    };
    let owner = nav.user.is_owner();
    let page = html! {
        (DOCTYPE)
        html lang="en" {
            head { (head(&format!("{title} · {}", cfg.title()), "admin.css")) meta name="csrf" content=(nav.csrf); }
            body.admin {
                input #nav-toggle type="checkbox" hidden;
                aside.sidebar {
                    div.brand { a href="/admin" { span.brand-names { (cfg.couple.a_first) " & " (cfg.couple.b_first) } span.brand-sub { "Wedding HQ" } } }
                    nav {
                        (link("/admin", "dash", "Overview", 0))
                        (link("/admin/households", "households", "Households", 0))
                        (link("/admin/guests", "guests", "People", 0))
                        (link("/admin/website", "website", "Website", 0))
                        (link("/admin/gallery", "gallery", "Gallery", 0))
                        (link("/admin/stationery", "stationery", "Stationery", 0))
                        @if owner {
                            (link("/admin/send", "send", "Send", 0))
                            (link("/admin/users", "users", "Team", nav.pending_users))
                            (link("/admin/settings", "settings", "Settings", 0))
                        }
                        (link("/admin/audit", "audit", "Activity", 0))
                    }
                    div.me {
                        a href="/admin/account" { (nav.user.name) }
                        span.role { (nav.user.role) }
                        form method="post" action="/admin/logout" { (csrf_input(&nav.csrf)) button.linkish type="submit" { "Sign out" } }
                    }
                }
                div.main {
                    header.topbar {
                        label.burger for="nav-toggle" aria-label="Menu" { span {} span {} span {} }
                        h1 { (title) }
                    }
                    @if let Some(f) = flash { div.flash role="status" { (f) } }
                    (body)
                }
                script src=(asset("admin.js")) defer {}
            }
        }
    };
    page.into_response()
}

/// Bare page (login, register, pending) without the admin chrome.
pub fn auth_page(cfg: &Config, title: &str, body: Markup) -> Response {
    html! {
        (DOCTYPE)
        html lang="en" {
            head { (head(&format!("{title} · {}", cfg.title()), "admin.css")) }
            body.auth {
                main.auth-card {
                    p.brand-names { (cfg.couple.a_first) " & " (cfg.couple.b_first) }
                    (body)
                }
                script src=(asset("passkey.js")) defer {}
            }
        }
    }
    .into_response()
}

pub fn csrf_input(csrf: &str) -> Markup {
    html! { input type="hidden" name="_csrf" value=(csrf); }
}

pub fn error_page(status: StatusCode, title: &str, message: &str) -> Response {
    let body = html! {
        (DOCTYPE)
        html lang="en" {
            head { (head(title, "site.css")) }
            body.guest {
                main.narrow.center {
                    h1.display { (title) }
                    p.lede { (message) }
                }
            }
        }
    };
    (status, body).into_response()
}

pub fn not_found() -> Response {
    error_page(
        StatusCode::NOT_FOUND,
        &crate::prose::global().t("website.errors.not_found_title"),
        &crate::prose::global().t("website.errors.not_found_text"),
    )
}

/// RFC3339 UTC -> "12 Jun 2027, 3:04 pm" in the venue's offset.
pub fn when(cfg: &Config, ts: &str) -> String {
    let Ok(t) = time::OffsetDateTime::parse(ts, &time::format_description::well_known::Rfc3339) else { return ts.to_string() };
    let t = cfg.offset().map(|o| t.to_offset(o)).unwrap_or(t);
    let f = time::macros::format_description!("[day padding:none] [month repr:short] [year], [hour repr:12 padding:none]:[minute] [period case:lower]");
    t.format(&f).unwrap_or_else(|_| ts.to_string())
}

pub fn when_opt(cfg: &Config, ts: &Option<String>) -> String {
    ts.as_deref().map(|t| when(cfg, t)).unwrap_or_else(|| "Not yet".into())
}

pub fn side_label(cfg: &Config, side: &str) -> String {
    match side {
        "a" => format!("{}'s side", cfg.couple.a_first),
        "b" => format!("{}'s side", cfg.couple.b_first),
        _ => "Both".into(),
    }
}

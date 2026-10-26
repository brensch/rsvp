//! HTTP server: routing, security middleware, shared state.

mod admin;
mod auth;
mod guest;
mod site;
mod ui;

use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::Result;
use axum::Router;
use axum::body::Body;
use axum::extract::{ConnectInfo, Request, State};
use axum::http::{HeaderMap, HeaderValue, Method, StatusCode, header};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use webauthn_rs::prelude::*;

use crate::assets::Renderer;
use crate::config::{Config, Env};
use crate::db::Db;
use crate::journal::Journal;

#[derive(Clone)]
pub struct AppState {
    pub cfg: Arc<Config>,
    pub env: Arc<Env>,
    pub db: Db,
    pub journal: Journal,
    pub renderer: Arc<Renderer>,
    pub webauthn: Arc<Webauthn>,
    pub ceremonies: Arc<Mutex<HashMap<String, (Instant, auth::Ceremony)>>>,
    pub limiter: Arc<RateLimiter>,
    pub http: reqwest::Client,
}

impl AppState {
    pub fn new(cfg: Config, env: Env, db: Db) -> Result<Self> {
        let origin = Url::parse(&cfg.site.base_url)?;
        let webauthn = WebauthnBuilder::new(&cfg.site.rp_id, &origin)?.rp_name(&cfg.title()).build()?;
        let renderer = Renderer::new(&env.data_dir);
        Ok(Self {
            journal: Journal::new(env.journal_path()),
            cfg: Arc::new(cfg),
            env: Arc::new(env),
            db,
            renderer: Arc::new(renderer),
            webauthn: Arc::new(webauthn),
            ceremonies: Arc::new(Mutex::new(HashMap::new())),
            limiter: Arc::new(RateLimiter::default()),
            http: reqwest::Client::builder().timeout(Duration::from_secs(15)).build()?,
        })
    }
}

pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/", get(guest::home))
        .route("/robots.txt", get(|| async { "User-agent: *\nDisallow: /\n" }))
        .route("/healthz", get(healthz))
        .route("/static/{file}", get(ui::static_file))
        .route("/fonts/{file}", get(ui::font_file))
        .merge(guest::routes())
        .merge(auth::routes())
        .merge(admin::routes())
        .merge(site::routes()) // last: it has the catch-all /{slug} page route
        .fallback(|| async { ui::not_found() })
        .layer(middleware::from_fn_with_state(state.clone(), guard))
        // Small bodies everywhere; the photo upload route raises its own limit.
        .layer(axum::extract::DefaultBodyLimit::max(256 * 1024))
        .layer(tower_http::timeout::TimeoutLayer::with_status_code(StatusCode::REQUEST_TIMEOUT, Duration::from_secs(180)))
        .with_state(state)
}

async fn healthz(State(st): State<AppState>) -> impl IntoResponse {
    match st.db.call(|c| Ok(c.query_row("SELECT 1", [], |r| r.get::<_, i64>(0))?)).await {
        Ok(_) => (StatusCode::OK, "ok"),
        Err(_) => (StatusCode::SERVICE_UNAVAILABLE, "db unavailable"),
    }
}

/// Client IP. We only trust X-Forwarded-For when the TCP peer is loopback (Caddy).
pub fn client_ip(headers: &HeaderMap, peer: Option<SocketAddr>) -> String {
    let peer_ip = peer.map(|p| p.ip());
    if peer_ip.is_none_or(|ip| ip.is_loopback()) {
        if let Some(xff) = headers.get("x-forwarded-for").and_then(|v| v.to_str().ok()) {
            if let Some(last) = xff.split(',').map(str::trim).filter(|s| s.parse::<IpAddr>().is_ok()).next_back() {
                return last.to_string();
            }
        }
    }
    peer_ip.map(|i| i.to_string()).unwrap_or_else(|| "unknown".into())
}

/// Every request passes through here: rate limiting, cross-origin write
/// protection, and security headers on every response.
async fn guard(State(st): State<AppState>, req: Request, next: Next) -> Response {
    let peer = req.extensions().get::<ConnectInfo<SocketAddr>>().map(|c| c.0);
    let ip = client_ip(req.headers(), peer);
    let path = req.uri().path().to_string();
    let is_write = !matches!(*req.method(), Method::GET | Method::HEAD | Method::OPTIONS);

    // Rate limits (per IP): generous for humans, tight for brute force.
    let bucket = if path.starts_with("/admin/api/") {
        Some(("auth", 30.0, 30.0 / 60.0))
    } else if path.starts_with("/i/") {
        Some(("guest", 60.0, 1.0))
    } else if is_write {
        Some(("write", 60.0, 1.0))
    } else {
        None
    };
    if let Some((name, burst, per_sec)) = bucket {
        if !st.limiter.check(&format!("{name}:{ip}"), burst, per_sec) {
            tracing::warn!(%ip, %path, "rate limited");
            return with_headers(ui::error_page(StatusCode::TOO_MANY_REQUESTS, "Slow down", "Too many requests. Please wait a minute and try again."), &path);
        }
    }
    if path.starts_with("/i/") && !st.limiter.has_tokens(&format!("badtoken:{ip}")) {
        return with_headers(ui::error_page(StatusCode::TOO_MANY_REQUESTS, "Slow down", "Too many attempts. Please try again later."), &path);
    }

    // Writes must come from our own pages.
    if is_write && !same_origin(req.headers(), &st.cfg.site.base_url) {
        tracing::warn!(%ip, %path, "cross-origin write blocked");
        return with_headers((StatusCode::FORBIDDEN, "cross-origin request blocked").into_response(), &path);
    }

    let mut req = req;
    req.extensions_mut().insert(ClientIp(ip.clone()));
    let resp = next.run(req).await;

    if path.starts_with("/i/") && resp.status() == StatusCode::NOT_FOUND {
        // Unknown guest token: count toward a lockout (15 misses / ~15 min).
        st.limiter.check(&format!("badtoken:{ip}"), 15.0, 1.0 / 60.0);
    }
    with_headers(resp, &path)
}

#[derive(Clone)]
pub struct ClientIp(pub String);

fn same_origin(headers: &HeaderMap, base_url: &str) -> bool {
    let fetch_site = headers.get("sec-fetch-site").and_then(|v| v.to_str().ok());
    match headers.get(header::ORIGIN).and_then(|v| v.to_str().ok()) {
        // Browsers send `Origin: null` on form posts under some referrer policies and
        // privacy settings; then trust fetch metadata, which they always send.
        Some("null") => fetch_site == Some("same-origin"),
        Some(origin) => origin == base_url,
        None => match fetch_site {
            Some(site) => site == "same-origin" || site == "none",
            None => true, // non-browser client; cookies are SameSite=Strict anyway
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn h(pairs: &[(&'static str, &'static str)]) -> HeaderMap {
        let mut m = HeaderMap::new();
        for (k, v) in pairs {
            m.insert(*k, HeaderValue::from_static(v));
        }
        m
    }

    #[test]
    fn origin_checks() {
        let base = "https://rsvp.example.com";
        assert!(same_origin(&h(&[("origin", "https://rsvp.example.com")]), base));
        assert!(!same_origin(&h(&[("origin", "https://evil.example")]), base));
        // Form posts with Origin: null are fine only when fetch metadata says same-origin.
        assert!(same_origin(&h(&[("origin", "null"), ("sec-fetch-site", "same-origin")]), base));
        assert!(!same_origin(&h(&[("origin", "null"), ("sec-fetch-site", "cross-site")]), base));
        assert!(!same_origin(&h(&[("origin", "null")]), base));
        assert!(!same_origin(&h(&[("sec-fetch-site", "cross-site")]), base));
    }
}

fn with_headers(mut resp: Response, path: &str) -> Response {
    let h = resp.headers_mut();
    let set = |h: &mut HeaderMap, k: &'static str, v: &'static str| {
        h.insert(k, HeaderValue::from_static(v));
    };
    set(
        h,
        "content-security-policy",
        "default-src 'none'; script-src 'self'; style-src 'self'; img-src 'self' data:; font-src 'self'; \
         connect-src 'self'; form-action 'self'; frame-ancestors 'none'; base-uri 'none'; manifest-src 'self'",
    );
    set(h, "x-content-type-options", "nosniff");
    set(h, "x-frame-options", "DENY");
    // Guest links carry secrets in the path: never send them to another site. (`same-origin`
    // rather than `no-referrer`, which makes browsers send `Origin: null` on our own form posts.)
    set(h, "referrer-policy", "same-origin");
    set(h, "permissions-policy", "camera=(), microphone=(), geolocation=(), payment=(), usb=(), interest-cohort=()");
    set(h, "cross-origin-opener-policy", "same-origin");
    set(h, "cross-origin-resource-policy", "same-origin");
    set(h, "strict-transport-security", "max-age=63072000; includeSubDomains");
    set(h, "x-robots-tag", "noindex, nofollow, noarchive");
    if path.starts_with("/admin") {
        set(h, header::CACHE_CONTROL.as_str(), "no-store");
    } else if !(path.starts_with("/static/") || path.starts_with("/fonts/") || path.starts_with("/media/") || h.contains_key(header::CACHE_CONTROL)) {
        // Guest pages: always check with the server, but the browser may keep the page in memory
        // so Back and Forward are instant (`no-store` would rule that out).
        set(h, header::CACHE_CONTROL.as_str(), "private, no-cache");
        let html = h.get(header::CONTENT_TYPE).and_then(|v| v.to_str().ok()).is_some_and(|v| v.starts_with("text/html"));
        if html {
            if let Ok(v) = HeaderValue::from_str(&format!("\"{}\"", ui::asset("speculation.json"))) {
                h.insert("speculation-rules", v);
            }
        }
    }
    resp
}

// ---------------------------------------------------------------------------
// Rate limiting: token buckets in memory.

#[derive(Default)]
pub struct RateLimiter {
    // key -> (tokens, last refill, refill per second, burst)
    buckets: Mutex<HashMap<String, (f64, Instant, f64, f64)>>,
}

impl RateLimiter {
    /// Take one token; false if the bucket is empty.
    pub fn check(&self, key: &str, burst: f64, per_sec: f64) -> bool {
        let mut b = self.buckets.lock().unwrap_or_else(|p| p.into_inner());
        let now = Instant::now();
        if b.len() > 50_000 {
            b.retain(|_, (_, t, _, _)| now.duration_since(*t) < Duration::from_secs(3600));
        }
        let (tokens, last, rate, cap) = b.entry(key.to_string()).or_insert((burst, now, per_sec, burst));
        *tokens = (*tokens + now.duration_since(*last).as_secs_f64() * *rate).min(*cap);
        *last = now;
        if *tokens >= 1.0 {
            *tokens -= 1.0;
            true
        } else {
            false
        }
    }

    /// True if the bucket has at least one token, without taking it.
    pub fn has_tokens(&self, key: &str) -> bool {
        let b = self.buckets.lock().unwrap_or_else(|p| p.into_inner());
        b.get(key).is_none_or(|(tokens, last, rate, cap)| {
            (*tokens + Instant::now().duration_since(*last).as_secs_f64() * *rate).min(*cap) >= 1.0
        })
    }
}

// ---------------------------------------------------------------------------
// Errors

pub struct AppError(anyhow::Error);

impl<E: Into<anyhow::Error>> From<E> for AppError {
    fn from(e: E) -> Self {
        Self(e.into())
    }
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        let id = uuid::Uuid::new_v4().to_string()[..8].to_string();
        tracing::error!(error = ?self.0, error_id = %id, "request failed");
        ui::error_page(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Something went wrong",
            &format!("Sorry, something went wrong on our side. Please try again in a moment. (ref {id})"),
        )
    }
}

pub type AppResult<T = Response> = Result<T, AppError>;

pub fn redirect(to: &str) -> Response {
    Response::builder().status(StatusCode::SEE_OTHER).header(header::LOCATION, to).body(Body::empty()).unwrap()
}

pub async fn serve(state: AppState) -> Result<()> {
    let bind = state.cfg.site.bind.clone();
    let app = router(state);
    let listener = tokio::net::TcpListener::bind(&bind).await?;
    tracing::info!(%bind, "listening");
    axum::serve(listener, app.into_make_service_with_connect_info::<SocketAddr>())
        .with_graceful_shutdown(async {
            let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()).unwrap();
            tokio::select! { _ = tokio::signal::ctrl_c() => {}, _ = term.recv() => {} }
            tracing::info!("shutting down");
        })
        .await?;
    Ok(())
}

//! Passkey-only admin authentication.
//!
//! * Sign-in is usernameless (discoverable credentials): one button, the
//!   browser/phone offers the passkeys it has for this site.
//! * Sign-up: choose a display name + create a passkey. The account is
//!   `pending` until an owner approves it, unless the person used an invite
//!   link (created by an owner or `rsvp invite` on the server).
//! * Sessions are random 160-bit ids in a `__Host-` cookie (Secure, HttpOnly,
//!   SameSite=Strict); only their SHA-256 is stored. Every admin form carries a
//!   per-session CSRF token as well.

use std::time::{Duration, Instant};

use anyhow::anyhow;
use axum::extract::{FromRequestParts, Query, State};
use axum::http::request::Parts;
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Extension, Form, Json, Router};
use data_encoding::BASE64URL_NOPAD;
use maud::html;
use rusqlite::{OptionalExtension, params};
use serde::Deserialize;
use serde_json::json;
use webauthn_rs::prelude::*;

use super::ui::{self, auth_page};
use super::{AppResult, AppState, ClientIp, redirect};
use crate::db::now;
use crate::store::{self, User};
use crate::tokens;

const SESSION_COOKIE: &str = "__Host-rsvp_session";
// `__Host-` cookies must be Secure, have no Domain, and use Path=/ or browsers drop them.
const CEREMONY_COOKIE: &str = "__Host-rsvp_ceremony";
const SESSION_DAYS: i64 = 14;
const CEREMONY_TTL: Duration = Duration::from_secs(300);

pub enum Ceremony {
    Register { reg: PasskeyRegistration, user_id: Uuid, name: String, invite: Option<String> },
    AddPasskey { reg: PasskeyRegistration, user_id: String },
    Login { auth: DiscoverableAuthentication },
}

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/admin/login", get(login_page))
        .route("/admin/register", get(register_page))
        .route("/admin/pending", get(pending_page))
        .route("/admin/logout", post(logout))
        .route("/admin/api/login/start", post(login_start))
        .route("/admin/api/login/finish", post(login_finish))
        .route("/admin/api/register/start", post(register_start))
        .route("/admin/api/register/finish", post(register_finish))
        .route("/admin/api/passkey/start", post(add_passkey_start))
        .route("/admin/api/passkey/finish", post(add_passkey_finish))
}

// ---------------------------------------------------------------------------
// Session extraction

#[derive(Clone)]
pub struct Session {
    pub user: User,
    pub csrf: String,
}

pub(super) fn cookie<'a>(headers: &'a HeaderMap, name: &str) -> Option<&'a str> {
    headers
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(';'))
        .map(str::trim)
        .find_map(|kv| kv.strip_prefix(name).and_then(|r| r.strip_prefix('=')))
}

pub(super) async fn session_from(st: &AppState, headers: &HeaderMap) -> anyhow::Result<Option<Session>> {
    let Some(raw) = cookie(headers, SESSION_COOKIE) else { return Ok(None) };
    if raw.len() > 64 {
        return Ok(None);
    }
    let hash = tokens::sha256_hex(raw);
    st.db
        .call(move |c| {
            let row: Option<(String, String, String, String)> = c
                .query_row(
                    "SELECT user_id, csrf, expires_at, last_seen_at FROM sessions WHERE id_hash = ?1",
                    [&hash],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
                )
                .optional()?;
            let Some((uid, csrf, exp, seen)) = row else { return Ok(None) };
            let ts = now();
            if exp < ts {
                c.execute("DELETE FROM sessions WHERE id_hash = ?1", [&hash])?;
                return Ok(None);
            }
            // Touch at most once a minute.
            if seen[..16] != ts[..16] {
                c.execute("UPDATE sessions SET last_seen_at = ?1 WHERE id_hash = ?2", params![ts, hash])?;
            }
            Ok(store::get_user(c, &uid)?.map(|user| Session { user, csrf }))
        })
        .await
}

/// Any signed-in user, including pending ones.
pub struct AnyUser(pub Session);

/// An approved helper or owner.
pub struct Member(pub Session);

pub struct Reject(Response);
impl IntoResponse for Reject {
    fn into_response(self) -> Response {
        self.0
    }
}

impl FromRequestParts<AppState> for AnyUser {
    type Rejection = Reject;
    async fn from_request_parts(parts: &mut Parts, st: &AppState) -> Result<Self, Self::Rejection> {
        match session_from(st, &parts.headers).await {
            Ok(Some(s)) if s.user.role != "disabled" => Ok(AnyUser(s)),
            Ok(_) => Err(Reject(redirect("/admin/login"))),
            Err(e) => Err(Reject(super::AppError::from(e).into_response())),
        }
    }
}

impl FromRequestParts<AppState> for Member {
    type Rejection = Reject;
    async fn from_request_parts(parts: &mut Parts, st: &AppState) -> Result<Self, Self::Rejection> {
        let AnyUser(s) = AnyUser::from_request_parts(parts, st).await?;
        if s.user.is_active() { Ok(Member(s)) } else { Err(Reject(redirect("/admin/pending"))) }
    }
}

impl Session {
    pub fn check_csrf(&self, provided: Option<&str>) -> Result<(), Response> {
        match provided {
            Some(t) if tokens::constant_time_eq(t, &self.csrf) => Ok(()),
            _ => Err(ui::error_page(StatusCode::FORBIDDEN, "Session expired", "Please reload the page and try again.")),
        }
    }
    pub fn require_owner(&self) -> Result<(), Response> {
        if self.user.is_owner() {
            Ok(())
        } else {
            Err(ui::error_page(StatusCode::FORBIDDEN, "Owners only", "Ask one of the couple to do this for you."))
        }
    }
}

fn session_cookie(value: &str, max_age_secs: i64) -> HeaderValue {
    HeaderValue::from_str(&format!("{SESSION_COOKIE}={value}; Path=/; Secure; HttpOnly; SameSite=Strict; Max-Age={max_age_secs}")).unwrap()
}

async fn create_session(st: &AppState, user_id: &str, ip: &str) -> anyhow::Result<HeaderValue> {
    let id = tokens::random_secret(20);
    let csrf = tokens::random_secret(20);
    let (hash, uid, ip) = (tokens::sha256_hex(&id), user_id.to_string(), ip.to_string());
    st.db
        .call(move |c| {
            let ts = now();
            let exp = (time::OffsetDateTime::now_utc() + time::Duration::days(SESSION_DAYS))
                .format(&time::format_description::well_known::Rfc3339)?;
            c.execute(
                "INSERT INTO sessions (id_hash, user_id, csrf, created_at, expires_at, last_seen_at, ip) VALUES (?1, ?2, ?3, ?4, ?5, ?4, ?6)",
                params![hash, uid, csrf, ts, exp, ip],
            )?;
            c.execute("UPDATE users SET last_login_at = ?1 WHERE id = ?2", params![ts, uid])?;
            c.execute("DELETE FROM sessions WHERE expires_at < ?1", [ts])?;
            Ok(())
        })
        .await?;
    Ok(session_cookie(&id, SESSION_DAYS * 86400))
}

// ---------------------------------------------------------------------------
// Ceremony state (short-lived, in memory, keyed by a cookie)

fn put_ceremony(st: &AppState, c: Ceremony) -> HeaderValue {
    let key = tokens::random_secret(16);
    let mut m = st.ceremonies.lock().unwrap_or_else(|p| p.into_inner());
    m.retain(|_, (t, _)| t.elapsed() < CEREMONY_TTL);
    if m.len() > 10_000 {
        m.clear();
    }
    m.insert(key.clone(), (Instant::now(), c));
    HeaderValue::from_str(&format!("{CEREMONY_COOKIE}={key}; Path=/; Secure; HttpOnly; SameSite=Strict; Max-Age=300")).unwrap()
}

fn take_ceremony(st: &AppState, headers: &HeaderMap) -> Option<Ceremony> {
    let key = cookie(headers, CEREMONY_COOKIE)?;
    let mut m = st.ceremonies.lock().unwrap_or_else(|p| p.into_inner());
    let (t, c) = m.remove(key)?;
    (t.elapsed() < CEREMONY_TTL).then_some(c)
}

fn api_err(status: StatusCode, msg: &str) -> Response {
    (status, Json(json!({"error": msg}))).into_response()
}

fn json_with_cookie(body: serde_json::Value, cookie: HeaderValue) -> Response {
    let mut r = Json(body).into_response();
    r.headers_mut().append(header::SET_COOKIE, cookie);
    r
}

fn cred_key(id: &[u8]) -> String {
    BASE64URL_NOPAD.encode(id)
}

// ---------------------------------------------------------------------------
// Pages

async fn login_page(State(st): State<AppState>, headers: HeaderMap) -> AppResult {
    if let Ok(Some(_)) = session_from(&st, &headers).await {
        return Ok(redirect("/admin"));
    }
    Ok(auth_page(
        &st.cfg,
        "Sign in",
        html! {
            h1 { "Wedding HQ" }
            p.muted { "Sign in with the passkey you created for this site." }
            button.button.primary.wide #passkey-login type="button" { "Sign in with passkey" }
            p.error #auth-error role="alert" hidden {}
            p.small { "New here? " a href="/admin/register" { "Create your access" } }
        },
    ))
}

#[derive(Deserialize)]
struct RegisterQuery {
    invite: Option<String>,
}

async fn register_page(State(st): State<AppState>, Query(q): Query<RegisterQuery>) -> AppResult {
    let invite = q.invite.filter(|s| s.len() < 100);
    let role = match &invite {
        Some(code) => {
            let code = code.clone();
            st.db.call(move |c| store::check_invite(c, &code)).await?
        }
        None => None,
    };
    Ok(auth_page(
        &st.cfg,
        "Create access",
        html! {
            h1 { "Create your access" }
            @if invite.is_some() && role.is_none() {
                p.error { "That invite link has expired or was already used. You can still sign up, and the couple will need to approve you." }
            }
            @if let Some((r, rel)) = &role {
                @let what = match (r.as_str(), rel.as_str()) {
                    ("owner", _) => "an owner".to_string(),
                    (_, "a_parent") => format!("{}'s parent", st.cfg.couple.a_first),
                    (_, "b_parent") => format!("{}'s parent", st.cfg.couple.b_first),
                    _ => "a helper".to_string(),
                };
                p.muted { "You've been invited as " strong { (what) } ". Choose the name you'll be known by, then create a passkey on this device." }
            } @else {
                p.muted { "Choose the name the couple will recognise, then create a passkey. They'll approve you before you can see anything." }
            }
            form #register-form {
                label.field { span { "Your name" } input #reg-name type="text" name="name" required minlength="2" maxlength="40" autocomplete="nickname"; }
                @if let Some(code) = invite.filter(|_| role.is_some()) { input #reg-invite type="hidden" value=(code); }
                button.button.primary.wide type="submit" { "Create passkey" }
            }
            p.error #auth-error role="alert" hidden {}
            p.small { "Already set up? " a href="/admin/login" { "Sign in" } }
        },
    ))
}

async fn pending_page(State(st): State<AppState>, AnyUser(s): AnyUser) -> AppResult {
    if s.user.is_active() {
        return Ok(redirect("/admin"));
    }
    Ok(auth_page(
        &st.cfg,
        "Awaiting approval",
        html! {
            h1 { "Hi " (s.user.name) }
            p { "Your access is waiting for approval. Once the couple lets you in, this page will take you straight to the dashboard." }
            p { a.button.wide href="/admin/pending" { "Check again" } }
            form method="post" action="/admin/logout" { (ui::csrf_input(&s.csrf)) button.linkish type="submit" { "Sign out" } }
        },
    ))
}

#[derive(Deserialize)]
struct CsrfForm {
    _csrf: Option<String>,
}

async fn logout(State(st): State<AppState>, AnyUser(s): AnyUser, headers: HeaderMap, Form(f): Form<CsrfForm>) -> AppResult {
    if let Err(r) = s.check_csrf(f._csrf.as_deref()) {
        return Ok(r);
    }
    if let Some(raw) = cookie(&headers, SESSION_COOKIE) {
        let hash = tokens::sha256_hex(raw);
        st.db.call(move |c| Ok(c.execute("DELETE FROM sessions WHERE id_hash = ?1", [hash])?)).await?;
    }
    let mut r = redirect("/admin/login");
    r.headers_mut().append(header::SET_COOKIE, session_cookie("", 0));
    Ok(r)
}

// ---------------------------------------------------------------------------
// Login

async fn login_start(State(st): State<AppState>) -> AppResult {
    let (rcr, auth) = st.webauthn.start_discoverable_authentication().map_err(|e| anyhow!("{e:?}"))?;
    Ok(json_with_cookie(serde_json::to_value(rcr)?, put_ceremony(&st, Ceremony::Login { auth })))
}

async fn login_finish(
    State(st): State<AppState>,
    Extension(ClientIp(ip)): Extension<ClientIp>,
    headers: HeaderMap,
    Json(cred): Json<PublicKeyCredential>,
) -> AppResult {
    let Some(Ceremony::Login { auth }) = take_ceremony(&st, &headers) else {
        return Ok(api_err(StatusCode::BAD_REQUEST, "Sign-in timed out. Please try again."));
    };
    let fail = || api_err(StatusCode::UNAUTHORIZED, "That passkey isn't registered here.");
    let Ok((user_uuid, cred_id)) = st.webauthn.identify_discoverable_authentication(&cred) else { return Ok(fail()) };
    let key = cred_key(cred_id);
    let uid = user_uuid.to_string();
    let stored: Option<(String, String)> = {
        let key = key.clone();
        st.db
            .call(move |c| {
                Ok(c.query_row("SELECT user_id, passkey FROM credentials WHERE cred_id = ?1", [key], |r| Ok((r.get(0)?, r.get(1)?)))
                    .optional()?)
            })
            .await?
    };
    let Some((owner, pk_json)) = stored else { return Ok(fail()) };
    if owner != uid {
        return Ok(fail());
    }
    let mut passkey: Passkey = serde_json::from_str(&pk_json)?;
    let result = match st.webauthn.finish_discoverable_authentication(&cred, auth, &[DiscoverableKey::from(&passkey)]) {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!(error = ?e, %ip, "passkey verification failed");
            return Ok(fail());
        }
    };
    let user = {
        let uid = uid.clone();
        st.db.call(move |c| store::get_user(c, &uid)).await?
    };
    let Some(user) = user else { return Ok(fail()) };
    if user.role == "disabled" {
        return Ok(api_err(StatusCode::FORBIDDEN, "This account has been disabled."));
    }
    let changed = passkey.update_credential(&result).unwrap_or(false);
    {
        let (key, pk) = (key.clone(), serde_json::to_string(&passkey)?);
        let actor = user.actor();
        let ip = ip.clone();
        st.db
            .call(move |c| {
                if changed {
                    c.execute("UPDATE credentials SET passkey = ?1 WHERE cred_id = ?2", params![pk, key])?;
                }
                c.execute("UPDATE credentials SET last_used_at = ?1 WHERE cred_id = ?2", params![now(), key])?;
                store::audit(c, &actor, "auth.login", "user", "", &json!({"ip": ip}))?;
                Ok(())
            })
            .await?;
    }
    let cookie = create_session(&st, &uid, &ip).await?;
    let to = if user.is_active() { "/admin" } else { "/admin/pending" };
    Ok(json_with_cookie(json!({"redirect": to}), cookie))
}

// ---------------------------------------------------------------------------
// Registration

#[derive(Deserialize)]
struct RegisterStart {
    name: String,
    invite: Option<String>,
}

fn clean_name(s: &str) -> Option<String> {
    let s: String = s.split_whitespace().collect::<Vec<_>>().join(" ");
    let ok = (2..=40).contains(&s.chars().count()) && s.chars().all(|c| c.is_alphanumeric() || " .'-&()".contains(c));
    ok.then_some(s)
}

async fn register_start(State(st): State<AppState>, Json(req): Json<RegisterStart>) -> AppResult {
    let Some(name) = clean_name(&req.name) else {
        return Ok(api_err(StatusCode::BAD_REQUEST, "Use 2–40 letters, numbers or spaces for your name."));
    };
    let taken = {
        let name = name.clone();
        st.db.call(move |c| store::user_by_name(c, &name)).await?.is_some()
    };
    if taken {
        return Ok(api_err(StatusCode::CONFLICT, "That name is taken. Try adding a surname or initial."));
    }
    let user_id = Uuid::new_v4();
    let (ccr, reg) = st.webauthn.start_passkey_registration(user_id, &name, &name, None).map_err(|e| anyhow!("{e:?}"))?;
    let invite = req.invite.filter(|s| !s.is_empty() && s.len() < 100);
    Ok(json_with_cookie(serde_json::to_value(ccr)?, put_ceremony(&st, Ceremony::Register { reg, user_id, name, invite })))
}

async fn register_finish(
    State(st): State<AppState>,
    Extension(ClientIp(ip)): Extension<ClientIp>,
    headers: HeaderMap,
    Json(cred): Json<RegisterPublicKeyCredential>,
) -> AppResult {
    let Some(Ceremony::Register { reg, user_id, name, invite }) = take_ceremony(&st, &headers) else {
        return Ok(api_err(StatusCode::BAD_REQUEST, "Sign-up timed out. Please try again."));
    };
    let passkey = match st.webauthn.finish_passkey_registration(&cred, &reg) {
        Ok(p) => p,
        Err(e) => {
            tracing::warn!(error = ?e, "passkey registration failed");
            return Ok(api_err(StatusCode::BAD_REQUEST, "Your device didn't create a passkey. Please try again."));
        }
    };
    let uid = user_id.to_string();
    let key = cred_key(passkey.cred_id().as_ref());
    let pk = serde_json::to_string(&passkey)?;
    let role = {
        let (uid, name) = (uid.clone(), name.clone());
        st.db
            .call(move |c| {
                let tx = crate::db::begin(c)?;
                if store::user_by_name(&tx, &name)?.is_some() {
                    anyhow::bail!("name taken");
                }
                let role = match &invite {
                    Some(code) => store::check_invite(&tx, code)?,
                    None => None,
                };
                let (role_str, relation) = role.clone().unwrap_or_else(|| ("pending".into(), String::new()));
                let ts = now();
                tx.execute(
                    "INSERT INTO users (id, name, role, relation, created_at, approved_by, approved_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                    params![uid, name, role_str, relation, ts, role.as_ref().map(|_| "invite"), role.as_ref().map(|_| ts.clone())],
                )?;
                tx.execute(
                    "INSERT INTO credentials (cred_id, user_id, passkey, created_at) VALUES (?1, ?2, ?3, ?4)",
                    params![key, uid, pk, ts],
                )?;
                if let Some(code) = &invite {
                    if role.is_some() {
                        store::consume_invite(&tx, code, &uid)?;
                    }
                }
                store::audit(&tx, &format!("user:{name}"), "user.register", "user", &uid, &json!({"role": role_str}))?;
                store::enqueue(&tx, "discord", &json!({"type": "signup", "name": name, "role": role_str}))?;
                tx.commit()?;
                Ok(role_str)
            })
            .await
    };
    let role = match role {
        Ok(r) => r,
        Err(e) if e.to_string() == "name taken" => return Ok(api_err(StatusCode::CONFLICT, "That name was just taken.")),
        Err(e) => return Err(e.into()),
    };
    let cookie = create_session(&st, &uid, &ip).await?;
    let to = if role == "pending" { "/admin/pending" } else { "/admin" };
    Ok(json_with_cookie(json!({"redirect": to}), cookie))
}

// ---------------------------------------------------------------------------
// Additional passkeys for a signed-in user (e.g. laptop + phone)

async fn add_passkey_start(State(st): State<AppState>, AnyUser(s): AnyUser, headers: HeaderMap) -> AppResult {
    if let Err(r) = s.check_csrf(headers.get("x-csrf").and_then(|v| v.to_str().ok())) {
        return Ok(r);
    }
    let uid = s.user.id.clone();
    let existing: Vec<CredentialID> = st
        .db
        .call(move |c| {
            let mut stmt = c.prepare("SELECT passkey FROM credentials WHERE user_id = ?1")?;
            let rows = stmt.query_map([uid], |r| r.get::<_, String>(0))?;
            let mut out = vec![];
            for r in rows {
                let pk: Passkey = serde_json::from_str(&r?)?;
                out.push(pk.cred_id().clone());
            }
            Ok(out)
        })
        .await?;
    let uuid = Uuid::parse_str(&s.user.id)?;
    let (ccr, reg) = st
        .webauthn
        .start_passkey_registration(uuid, &s.user.name, &s.user.name, Some(existing))
        .map_err(|e| anyhow!("{e:?}"))?;
    Ok(json_with_cookie(serde_json::to_value(ccr)?, put_ceremony(&st, Ceremony::AddPasskey { reg, user_id: s.user.id.clone() })))
}

async fn add_passkey_finish(
    State(st): State<AppState>,
    AnyUser(s): AnyUser,
    headers: HeaderMap,
    Json(cred): Json<RegisterPublicKeyCredential>,
) -> AppResult {
    if let Err(r) = s.check_csrf(headers.get("x-csrf").and_then(|v| v.to_str().ok())) {
        return Ok(r);
    }
    let Some(Ceremony::AddPasskey { reg, user_id }) = take_ceremony(&st, &headers) else {
        return Ok(api_err(StatusCode::BAD_REQUEST, "Timed out. Please try again."));
    };
    if user_id != s.user.id {
        return Ok(api_err(StatusCode::FORBIDDEN, "Session mismatch."));
    }
    let passkey = match st.webauthn.finish_passkey_registration(&cred, &reg) {
        Ok(p) => p,
        Err(_) => return Ok(api_err(StatusCode::BAD_REQUEST, "Your device didn't create a passkey.")),
    };
    let (key, pk, actor) = (cred_key(passkey.cred_id().as_ref()), serde_json::to_string(&passkey)?, s.user.actor());
    st.db
        .call(move |c| {
            c.execute(
                "INSERT INTO credentials (cred_id, user_id, passkey, created_at) VALUES (?1, ?2, ?3, ?4)",
                params![key, user_id, pk, now()],
            )?;
            store::audit(c, &actor, "user.add_passkey", "user", &user_id, &json!({}))?;
            Ok(())
        })
        .await?;
    Ok(Json(json!({"redirect": "/admin/account?m=passkey"})).into_response())
}

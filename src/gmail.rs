//! Sending through the couple's Gmail account with the Gmail API.
//!
//! The account is connected once from Settings with Google's OAuth consent screen, asking only
//! for `gmail.send` (send mail; can't read the mailbox) plus the account's email address. The
//! refresh token lives in `<data>/gmail-token.json` (mode 0600): not in the database, so it
//! never reaches Litestream replicas or snapshots.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use base64::Engine;
use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;

use crate::config::{Config, Env};

pub const SCOPES: &str = "openid email https://www.googleapis.com/auth/gmail.send";
const AUTH_URL: &str = "https://accounts.google.com/o/oauth2/v2/auth";
const TOKEN_URL: &str = "https://oauth2.googleapis.com/token";
const SEND_URL: &str = "https://gmail.googleapis.com/gmail/v1/users/me/messages/send";

/// Google's token and send endpoints. `GMAIL_TEST_BASE` points both at a local fake for testing.
fn endpoint(real: &str) -> String {
    match std::env::var("GMAIL_TEST_BASE") {
        Ok(base) if !base.is_empty() => format!("{base}{}", reqwest::Url::parse(real).map(|u| u.path().to_string()).unwrap_or_default()),
        _ => real.to_string(),
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Stored {
    pub email: String,
    pub refresh_token: String,
    pub connected_at: String,
}

fn token_path(env: &Env) -> PathBuf {
    env.data_dir.join("gmail-token.json")
}

pub fn redirect_uri(cfg: &Config) -> String {
    format!("{}/admin/gmail/callback", cfg.site.base_url)
}

/// The connected account, if any.
pub fn connected(env: &Env) -> Option<Stored> {
    let raw = std::fs::read_to_string(token_path(env)).ok()?;
    serde_json::from_str(&raw).ok()
}

pub fn disconnect(env: &Env) -> Result<()> {
    match std::fs::remove_file(token_path(env)) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e.into()),
        _ => Ok(()),
    }
}

fn client(env: &Env) -> Result<(&str, &str)> {
    let id = env.gmail_client_id.as_deref().context("GMAIL_CLIENT_ID is not set in the env file")?;
    let secret = env.gmail_client_secret.as_deref().context("GMAIL_CLIENT_SECRET is not set in the env file")?;
    Ok((id, secret))
}

/// Google's consent screen. `state` is checked on the way back.
pub fn auth_url(cfg: &Config, env: &Env, state: &str, login_hint: Option<&str>) -> Result<String> {
    let (id, _) = client(env)?;
    let mut url = reqwest::Url::parse(AUTH_URL)?;
    url.query_pairs_mut()
        .append_pair("client_id", id)
        .append_pair("redirect_uri", &redirect_uri(cfg))
        .append_pair("response_type", "code")
        .append_pair("scope", SCOPES)
        .append_pair("access_type", "offline")
        .append_pair("prompt", "consent")
        .append_pair("include_granted_scopes", "false")
        .append_pair("state", state);
    if let Some(h) = login_hint {
        url.query_pairs_mut().append_pair("login_hint", h);
    }
    Ok(url.into())
}

#[derive(Deserialize)]
struct TokenResponse {
    access_token: String,
    #[serde(default)]
    expires_in: u64,
    #[serde(default)]
    refresh_token: Option<String>,
    #[serde(default)]
    id_token: Option<String>,
    #[serde(default)]
    scope: String,
}

/// Finish the consent flow: swap the code for tokens and store the refresh token.
pub async fn connect(http: &reqwest::Client, cfg: &Config, env: &Env, code: &str) -> Result<Stored> {
    let (id, secret) = client(env)?;
    let resp = http
        .post(endpoint(TOKEN_URL))
        .form(&[
            ("code", code),
            ("client_id", id),
            ("client_secret", secret),
            ("redirect_uri", &redirect_uri(cfg)),
            ("grant_type", "authorization_code"),
        ])
        .send()
        .await?;
    if !resp.status().is_success() {
        bail!("Google token exchange failed: {} {}", resp.status(), resp.text().await.unwrap_or_default().chars().take(300).collect::<String>());
    }
    let t: TokenResponse = resp.json().await?;
    if !t.scope.split(' ').any(|s| s == "https://www.googleapis.com/auth/gmail.send") {
        bail!("Google didn't grant permission to send email. Tick the \"Send email on your behalf\" box on the consent screen.");
    }
    let refresh_token = t.refresh_token.context("Google didn't return a refresh token; remove the app's access at myaccount.google.com/permissions and connect again")?;
    // The ID token came straight from Google over TLS, so its claims can be read without re-verifying.
    let email = t
        .id_token
        .as_deref()
        .and_then(|jwt| jwt.split('.').nth(1))
        .and_then(|p| base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(p.trim_end_matches('=')).ok())
        .and_then(|b| serde_json::from_slice::<serde_json::Value>(&b).ok())
        .and_then(|v| v["email"].as_str().map(String::from))
        .context("Google didn't say which account was connected")?;
    let stored = Stored { email, refresh_token, connected_at: crate::db::now() };
    write_private(&token_path(env), &serde_json::to_vec_pretty(&stored)?)?;
    Ok(stored)
}

fn write_private(path: &PathBuf, bytes: &[u8]) -> Result<()> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let tmp = path.with_extension("tmp");
    let mut f = std::fs::OpenOptions::new().write(true).create(true).truncate(true).mode(0o600).open(&tmp)?;
    f.write_all(bytes)?;
    f.sync_all()?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}

/// Short-lived access tokens, refreshed from the stored refresh token as needed.
#[derive(Default)]
pub struct Sender {
    cached: Mutex<Option<(String, Instant)>>,
}

pub enum SendError {
    /// Google asked us to slow down; try again after this long.
    RateLimited(Duration),
    Other(anyhow::Error),
}

impl From<anyhow::Error> for SendError {
    fn from(e: anyhow::Error) -> Self {
        SendError::Other(e)
    }
}

impl Sender {
    async fn access_token(&self, http: &reqwest::Client, env: &Env) -> Result<String> {
        let mut cached = self.cached.lock().await;
        if let Some((tok, until)) = cached.as_ref() {
            if Instant::now() < *until {
                return Ok(tok.clone());
            }
        }
        let stored = connected(env).context("Gmail isn't connected (Settings, Connect Gmail)")?;
        let (id, secret) = client(env)?;
        let resp = http
            .post(endpoint(TOKEN_URL))
            .form(&[("refresh_token", stored.refresh_token.as_str()), ("client_id", id), ("client_secret", secret), ("grant_type", "refresh_token")])
            .send()
            .await?;
        if !resp.status().is_success() {
            bail!(
                "Gmail access was refused ({}); reconnect Gmail in Settings. {}",
                resp.status(),
                resp.text().await.unwrap_or_default().chars().take(200).collect::<String>()
            );
        }
        let t: TokenResponse = resp.json().await?;
        let ttl = Duration::from_secs(t.expires_in.max(120) - 60);
        *cached = Some((t.access_token.clone(), Instant::now() + ttl));
        Ok(t.access_token)
    }

    /// Send one RFC 5322 message. Returns Gmail's message id.
    pub async fn send(&self, http: &reqwest::Client, env: &Env, raw: &[u8]) -> Result<String, SendError> {
        let token = self.access_token(http, env).await?;
        let body = serde_json::json!({ "raw": base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(raw) });
        let resp = http.post(endpoint(SEND_URL)).bearer_auth(&token).json(&body).send().await.map_err(anyhow::Error::from)?;
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        if status.as_u16() == 429 {
            return Err(SendError::RateLimited(Duration::from_secs(120)));
        }
        if status.as_u16() == 401 {
            *self.cached.lock().await = None;
        }
        if !status.is_success() {
            return Err(SendError::Other(anyhow::anyhow!("gmail {status}: {}", text.chars().take(300).collect::<String>())));
        }
        let id = serde_json::from_str::<serde_json::Value>(&text).ok().and_then(|v| v["id"].as_str().map(String::from)).unwrap_or_default();
        Ok(id)
    }
}

// ---------------------------------------------------------------------------
// MIME

pub struct Attachment {
    pub filename: String,
    pub mime: String,
    pub bytes: Vec<u8>,
}

/// A multipart message: text + HTML alternatives, plus any attachments.
pub fn mime(from: &str, to: &str, reply_to: Option<&str>, subject: &str, text: &str, html: &str, attachments: &[Attachment]) -> Vec<u8> {
    let b64 = base64::engine::general_purpose::STANDARD;
    let wrap = |data: &[u8]| -> String {
        let enc = b64.encode(data);
        enc.as_bytes().chunks(76).map(|c| std::str::from_utf8(c).unwrap()).collect::<Vec<_>>().join("\r\n")
    };
    let boundary = |tag: &str| format!("=_{tag}_{}", uuid::Uuid::new_v4().simple());
    let alt = boundary("alt");
    let mut alt_part = format!("Content-Type: multipart/alternative; boundary=\"{alt}\"\r\n\r\n");
    for (ty, body) in [("text/plain", text), ("text/html", html)] {
        alt_part.push_str(&format!(
            "--{alt}\r\nContent-Type: {ty}; charset=utf-8\r\nContent-Transfer-Encoding: base64\r\n\r\n{}\r\n",
            wrap(body.as_bytes())
        ));
    }
    alt_part.push_str(&format!("--{alt}--\r\n"));

    let mut out = String::new();
    out.push_str(&format!("From: {}\r\nTo: {}\r\n", header_value(from), header_value(to)));
    if let Some(r) = reply_to {
        out.push_str(&format!("Reply-To: {}\r\n", header_value(r)));
    }
    out.push_str(&format!("Subject: {}\r\nMIME-Version: 1.0\r\n", encode_word(subject)));
    if attachments.is_empty() {
        out.push_str(&alt_part);
    } else {
        let mixed = boundary("mixed");
        out.push_str(&format!("Content-Type: multipart/mixed; boundary=\"{mixed}\"\r\n\r\n--{mixed}\r\n{alt_part}"));
        for a in attachments {
            let name = a.filename.replace(['"', '\r', '\n'], "");
            out.push_str(&format!(
                "--{mixed}\r\nContent-Type: {}; name=\"{name}\"\r\nContent-Disposition: attachment; filename=\"{name}\"\r\nContent-Transfer-Encoding: base64\r\n\r\n{}\r\n",
                a.mime,
                wrap(&a.bytes)
            ));
        }
        out.push_str(&format!("--{mixed}--\r\n"));
    }
    out.into_bytes()
}

/// `Name <addr>` with a non-ASCII display name encoded; no header injection.
fn header_value(v: &str) -> String {
    let v = v.replace(['\r', '\n'], " ");
    match v.rsplit_once('<') {
        Some((name, addr)) if !name.trim().is_empty() => {
            let name = name.trim().trim_matches('"');
            let name = if name.is_ascii() { format!("\"{}\"", name.replace('"', "")) } else { encode_word(name) };
            format!("{name} <{}", addr.trim())
        }
        _ => v.trim().to_string(),
    }
}

fn encode_word(s: &str) -> String {
    let s = s.replace(['\r', '\n'], " ");
    if s.is_ascii() { s } else { format!("=?UTF-8?B?{}?=", base64::engine::general_purpose::STANDARD.encode(s.as_bytes())) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mime_is_well_formed_and_injection_safe() {
        let raw = mime(
            "Alyssa & Brend <fam@example.com>",
            "guest@example.com\r\nBcc: evil@example.com",
            None,
            "Save the date: Alyssa & Brend, 9 July 2027 ✨",
            "hi",
            "<p>hi</p>",
            &[Attachment { filename: "invitation.pdf".into(), mime: "application/pdf".into(), bytes: vec![1, 2, 3] }],
        );
        let s = String::from_utf8(raw).unwrap();
        assert!(s.starts_with("From: \"Alyssa & Brend\" <fam@example.com>\r\n"));
        assert!(!s.contains("\r\nBcc:"), "no header injection");
        assert!(s.contains("Subject: =?UTF-8?B?"));
        assert!(s.contains("multipart/mixed") && s.contains("multipart/alternative") && s.contains("filename=\"invitation.pdf\""));
    }
}

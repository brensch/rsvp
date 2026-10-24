//! Outbox worker: delivers Discord notifications and emails queued by
//! transactions elsewhere. At-least-once with backoff; emails carry an
//! idempotency key so a retry after a crash can't double-send.

use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use rusqlite::{Connection, params};
use serde::Serialize;
use serde_json::{Value, json};

use crate::assets::Renderer;
use crate::config::{Config, Env};
use crate::db::{Db, now};
use crate::store;

#[derive(Clone)]
pub struct Worker {
    pub db: Db,
    pub cfg: Arc<Config>,
    pub env: Arc<Env>,
    pub renderer: Arc<Renderer>,
    pub http: reqwest::Client,
}

struct Job {
    id: i64,
    kind: String,
    payload: Value,
    attempts: i64,
}

fn max_attempts(kind: &str) -> i64 {
    if kind == "email" { 8 } else { 40 }
}

pub async fn run(w: Worker) {
    loop {
        match w.tick().await {
            Ok(0) => tokio::time::sleep(Duration::from_secs(3)).await,
            Ok(_) => {}
            Err(e) => {
                tracing::error!(error = ?e, "outbox tick failed");
                tokio::time::sleep(Duration::from_secs(10)).await;
            }
        }
    }
}

impl Worker {
    async fn tick(&self) -> Result<usize> {
        let jobs: Vec<Job> = self
            .db
            .call(|c| {
                let mut st = c.prepare(
                    "SELECT id, kind, payload, attempts FROM outbox
                     WHERE sent_at IS NULL AND failed_at IS NULL AND next_attempt_at <= ?1
                     ORDER BY id LIMIT 10",
                )?;
                let rows = st
                    .query_map([now()], |r| {
                        let p: String = r.get(2)?;
                        Ok(Job { id: r.get(0)?, kind: r.get(1)?, payload: serde_json::from_str(&p).unwrap_or(Value::Null), attempts: r.get(3)? })
                    })?
                    .collect::<rusqlite::Result<Vec<_>>>()?;
                Ok(rows)
            })
            .await?;
        let n = jobs.len();
        for job in jobs {
            let result = match job.kind.as_str() {
                "discord" => self.discord(&job).await,
                "email" => self.email(&job).await,
                other => Err(anyhow::anyhow!("unknown outbox kind {other}")),
            };
            self.finish(job, result).await?;
        }
        Ok(n)
    }

    async fn finish(&self, job: Job, result: Result<Option<Duration>>) -> Result<()> {
        let cfg = self.cfg.clone();
        self.db
            .call(move |c| {
                match result {
                    Ok(None) => {
                        c.execute("UPDATE outbox SET sent_at = ?1, attempts = attempts + 1, last_error = '' WHERE id = ?2", params![now(), job.id])?;
                    }
                    // Rate limited: retry later without counting an attempt.
                    Ok(Some(wait)) => {
                        let next = time::OffsetDateTime::now_utc() + wait;
                        c.execute("UPDATE outbox SET next_attempt_at = ?1 WHERE id = ?2", params![rfc(next), job.id])?;
                    }
                    Err(e) => {
                        let attempts = job.attempts + 1;
                        let msg: String = format!("{e:#}").chars().take(500).collect();
                        tracing::warn!(id = job.id, kind = %job.kind, attempts, error = %msg, "outbox delivery failed");
                        if attempts >= max_attempts(&job.kind) {
                            let tx = crate::db::begin(c)?;
                            tx.execute(
                                "UPDATE outbox SET failed_at = ?1, attempts = ?2, last_error = ?3 WHERE id = ?4",
                                params![now(), attempts, msg, job.id],
                            )?;
                            if job.kind != "discord" {
                                store::enqueue(&tx, "discord", &json!({"type": "alert", "text": format!("⚠️ {} #{} failed permanently: {msg}\n{}/admin/send", job.kind, job.id, cfg.site.base_url)}))?;
                            }
                            tx.commit()?;
                        } else {
                            let backoff = (30i64 << attempts.min(7)).min(3600);
                            let next = time::OffsetDateTime::now_utc() + time::Duration::seconds(backoff);
                            c.execute(
                                "UPDATE outbox SET attempts = ?1, last_error = ?2, next_attempt_at = ?3 WHERE id = ?4",
                                params![attempts, msg, rfc(next), job.id],
                            )?;
                        }
                    }
                }
                Ok(())
            })
            .await
    }

    async fn discord(&self, job: &Job) -> Result<Option<Duration>> {
        let Some(url) = &self.env.discord_webhook else {
            tracing::info!(payload = %job.payload, "discord not configured; notification logged only");
            return Ok(None);
        };
        let content = discord_text(&self.cfg, &job.payload);
        let content: String = content.chars().take(1990).collect();
        let resp = self
            .http
            .post(url)
            .query(&[("wait", "true")])
            .json(&json!({"content": content, "username": "RSVP", "allowed_mentions": {"parse": []}}))
            .send()
            .await?;
        if resp.status().as_u16() == 429 {
            let body: Value = resp.json().await.unwrap_or(Value::Null);
            let secs = body["retry_after"].as_f64().unwrap_or(5.0).clamp(1.0, 600.0);
            return Ok(Some(Duration::from_secs_f64(secs)));
        }
        if !resp.status().is_success() {
            bail!("discord {}: {}", resp.status(), resp.text().await.unwrap_or_default().chars().take(200).collect::<String>());
        }
        Ok(None)
    }

    async fn email(&self, job: &Job) -> Result<Option<Duration>> {
        let kind = job.payload["kind"].as_str().context("email kind")?.to_string();
        let hid = job.payload["household_id"].as_i64().context("household_id")?;
        let test_to = job.payload["test_to"].as_str().map(String::from);
        let (h, guests, responses) = self
            .db
            .call(move |c| Ok((store::must_household(c, hid)?, store::guests_for(c, hid)?, store::responses_for(c, hid)?)))
            .await?;
        if h.archived_at.is_some() && test_to.is_none() {
            bail!("household {hid} is archived");
        }
        let (r, cfg, env) = (self.renderer.clone(), self.cfg.clone(), self.env.clone());
        let is_test = test_to.is_some();
        let msg = tokio::task::spawn_blocking(move || crate::email::build(&r, &cfg, &env, &kind, h, guests, responses, test_to)).await??;
        let id = crate::email::send(&self.http, &self.cfg, &self.env, &msg, &format!("rsvp-outbox-{}", job.id)).await?;
        tracing::info!(outbox = job.id, resend_id = %id, to = ?msg.to, "email sent");
        if !is_test {
            let kind = job.payload["kind"].as_str().unwrap_or_default().to_string();
            self.db
                .call(move |c| {
                    store::mark_sent(c, hid, &kind)?;
                    store::audit(c, "system", "email.sent", "household", &hid.to_string(), &json!({"kind": kind, "resend_id": id}))
                })
                .await?;
        }
        Ok(None)
    }
}

fn rfc(t: time::OffsetDateTime) -> String {
    t.format(&time::format_description::well_known::Rfc3339).unwrap()
}

pub fn discord_text(cfg: &Config, p: &Value) -> String {
    let base = &cfg.site.base_url;
    let s = |k: &str| p[k].as_str().unwrap_or("").to_string();
    match p["type"].as_str().unwrap_or("") {
        "rsvp" => format!("💌 {}\n{base}/admin", s("text")),
        "signup" if s("role") == "pending" => format!("🔐 **{}** signed up for Wedding HQ and is waiting for approval.\n{base}/admin/users", s("name")),
        "signup" => format!("🔐 **{}** joined Wedding HQ via invite as {}.", s("name"), s("role")),
        "test" => format!("👋 Test notification from {}. Notifications are working.", s("name")),
        "backup" => s("text"),
        "alert" => s("text"),
        _ => format!("{p}"),
    }
}

#[derive(Serialize)]
pub struct OutboxRow {
    pub id: i64,
    pub kind: String,
    pub what: String,
    pub created_at: String,
    pub sent_at: Option<String>,
    pub failed_at: Option<String>,
    pub attempts: i64,
    pub last_error: String,
}

pub fn recent(c: &Connection) -> Result<Vec<OutboxRow>> {
    let mut st = c.prepare(
        "SELECT o.id, o.kind, o.payload, o.created_at, o.sent_at, o.failed_at, o.attempts, o.last_error, h.name
         FROM outbox o LEFT JOIN households h ON h.id = json_extract(o.payload, '$.household_id')
         ORDER BY o.id DESC LIMIT 100",
    )?;
    let rows = st
        .query_map([], |r| {
            let p: Value = serde_json::from_str(&r.get::<_, String>(2)?).unwrap_or(Value::Null);
            let hname: Option<String> = r.get(8)?;
            let what = match r.get::<_, String>(1)?.as_str() {
                "email" => format!(
                    "{} → {}{}",
                    p["kind"].as_str().unwrap_or(""),
                    hname.unwrap_or_default(),
                    p["test_to"].as_str().map(|t| format!(" (test to {t})")).unwrap_or_default()
                ),
                _ => p["type"].as_str().unwrap_or("").to_string(),
            };
            Ok(OutboxRow {
                id: r.get(0)?,
                kind: r.get(1)?,
                what,
                created_at: r.get(3)?,
                sent_at: r.get(4)?,
                failed_at: r.get(5)?,
                attempts: r.get(6)?,
                last_error: r.get(7)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

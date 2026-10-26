//! Outbox worker: delivers Discord notifications and emails queued by
//! transactions elsewhere. At-least-once with backoff; emails carry an
//! email is split into one job per address, and each records its send before retrying, so a
//! retry can't double-send. Emails go out at most one every `EMAIL_GAP` to look like a person.

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
    pub gmail: Arc<crate::gmail::Sender>,
    pub last_email: Arc<tokio::sync::Mutex<Option<std::time::Instant>>>,
}

/// Gap between emails, so a bulk send trickles out rather than arriving as a blast.
const EMAIL_GAP: Duration = Duration::from_secs(12);

struct Job {
    id: i64,
    kind: String,
    payload: Value,
    attempts: i64,
}

fn max_attempts(kind: &str) -> i64 {
    if kind.starts_with("email") { 8 } else { 40 }
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
                "email" => self.fan_out(&job).await,
                "email_one" => self.email_one(&job).await,
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
                        if job.kind.starts_with("email") {
                            let entity = job.payload["send_id"].as_str().map(|s| ("email_send", s.to_string())).unwrap_or(("outbox", job.id.to_string()));
                            store::audit(c, "system", "email.error", entity.0, &entity.1, &json!({
                                "error": msg, "attempt": attempts, "max_attempts": max_attempts(&job.kind),
                                "will_retry": attempts < max_attempts(&job.kind), "outbox": job.id, "job": job.payload,
                            }))?;
                        }
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

    /// A household email (or a test) becomes one `email_one` job per address, in one transaction.
    async fn fan_out(&self, job: &Job) -> Result<Option<Duration>> {
        let kind = job.payload["kind"].as_str().context("email kind")?.to_string();
        let hid = job.payload["household_id"].as_i64().context("household_id")?;
        let test_to = job.payload["test_to"].as_str().map(String::from);
        let n = self
            .db
            .call(move |c| {
                let h = store::must_household(c, hid)?;
                if h.archived_at.is_some() && test_to.is_none() {
                    bail!("household {hid} is archived");
                }
                let to = match &test_to {
                    Some(t) => vec![t.clone()],
                    None => crate::email::recipients(&h, &store::guests_for(c, hid)?),
                };
                if to.is_empty() {
                    bail!("household {hid} has no email address");
                }
                let tx = crate::db::begin(c)?;
                let mut sends = vec![];
                for addr in &to {
                    let send_id = store::create_send(&tx, hid, addr, &kind, test_to.is_some())?;
                    store::enqueue(&tx, "email_one", &json!({"send_id": send_id}))?;
                    sends.push(json!({"send": send_id, "to": addr}));
                }
                store::audit(&tx, "system", "email.queued", "household", &hid.to_string(), &json!({"kind": kind, "test": test_to.is_some(), "emails": sends}))?;
                tx.commit()?;
                Ok(to.len())
            })
            .await?;
        tracing::info!(outbox = job.id, household = hid, emails = n, "email split per address");
        Ok(None)
    }

    async fn email_one(&self, job: &Job) -> Result<Option<Duration>> {
        let send_id = job.payload["send_id"].as_str().context("send_id")?.to_string();
        let sid = send_id.clone();
        let (send, h, guests, responses) = self
            .db
            .call(move |c| {
                let s = store::get_send(c, &sid)?.context("email send row missing")?;
                let hid = s.household_id;
                Ok((s, store::must_household(c, hid)?, store::guests_for(c, hid)?, store::responses_for(c, hid)?))
            })
            .await?;
        if send.sent_at.is_some() {
            return Ok(None); // already went out; this is a retry after a crash
        }
        // Space emails out.
        {
            let mut last = self.last_email.lock().await;
            if let Some(t) = *last {
                let since = t.elapsed();
                if since < EMAIL_GAP {
                    return Ok(Some(EMAIL_GAP - since));
                }
            }
            *last = Some(std::time::Instant::now());
        }
        let connected = crate::gmail::connected(&self.env).context("Gmail isn't connected (Settings, Connect Gmail)")?;
        let (r, cfg, env) = (self.renderer.clone(), self.cfg.clone(), self.env.clone());
        let (kind, to, sid) = (send.kind.clone(), send.address.clone(), send_id.clone());
        let hid = h.id;
        let msg = tokio::task::spawn_blocking(move || crate::email::build(&r, &cfg, &env, &kind, h, guests, responses, &to, &sid)).await??;
        // The picture the email loads (and counts) from /e/<id>.png.
        let card = crate::email::card_file(&self.env, hid, &send.kind);
        std::fs::create_dir_all(card.parent().unwrap())?;
        std::fs::write(&card, &msg.card_png)?;

        let names = format!("{} & {}", self.cfg.couple.a_first, self.cfg.couple.b_first);
        let display = self.cfg.email.from.clone().unwrap_or(names);
        let from = format!("{} <{}>", display.split('<').next().unwrap_or(&display).trim(), connected.email);
        // Safety lock: send it to the lock address instead, saying who it would have gone to.
        let (to, subject) = match self.cfg.email_lock() {
            Some(lock) => (lock.to_string(), format!("[Not sent to {}] {}", msg.to, msg.subject)),
            None => (msg.to.clone(), msg.subject.clone()),
        };
        let raw = crate::gmail::mime(&from, &to, self.cfg.email.reply_to.as_deref(), &subject, &msg.text, &msg.html, &msg.attachments);
        let provider_id = match self.gmail.send(&self.http, &self.env, &raw).await {
            Ok(id) => id,
            Err(crate::gmail::SendError::RateLimited(wait)) => {
                let (sid, w) = (send_id.clone(), wait.as_secs());
                self.db
                    .call(move |c| store::audit(c, "system", "email.rate_limited", "email_send", &sid, &json!({"retry_in_secs": w})))
                    .await?;
                return Ok(Some(wait));
            }
            Err(crate::gmail::SendError::Other(e)) => return Err(e),
        };
        let detail = json!({
            "kind": send.kind, "household": hid, "send": send_id, "to": msg.to, "delivered_to": to,
            "redirected": self.cfg.email_lock().is_some(), "subject": subject, "gmail_id": provider_id,
            "test": send.test, "attempt": job.attempts + 1,
        });
        tracing::info!(outbox = job.id, send = %send_id, household = hid, "email sent");
        let (kind, test) = (send.kind.clone(), send.test);
        self.db
            .call(move |c| {
                let tx = crate::db::begin(c)?;
                store::mark_send_sent(&tx, &send_id, &provider_id)?;
                if !test {
                    store::mark_sent(&tx, hid, &kind)?;
                }
                store::audit(&tx, "system", "email.sent", "household", &hid.to_string(), &detail)?;
                tx.commit()
            })
            .await?;
        Ok(None)
    }
}

/// Every 10 minutes: invitations opened 48+ hours ago with no reply get flagged and pinged.
pub async fn unanswered_invitations(db: Db, cfg: Arc<Config>) {
    loop {
        let cutoff = rfc(time::OffsetDateTime::now_utc() - time::Duration::hours(48));
        let base = cfg.site.base_url.clone();
        match db.call(move |c| store::flag_unanswered_invitations(c, &cutoff, &base)).await {
            Ok(0) => {}
            Ok(n) => tracing::info!(n, "flagged opened invitations without a reply"),
            Err(e) => tracing::error!(error = ?e, "checking unanswered invitations failed"),
        }
        tokio::time::sleep(Duration::from_secs(600)).await;
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
        "nudge" => s("text"),
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
         FROM outbox o
         LEFT JOIN email_sends s ON s.id = json_extract(o.payload, '$.send_id')
         LEFT JOIN households h ON h.id = COALESCE(json_extract(o.payload, '$.household_id'), s.household_id)
         ORDER BY o.id DESC LIMIT 100",
    )?;
    let rows = st
        .query_map([], |r| {
            let p: Value = serde_json::from_str(&r.get::<_, String>(2)?).unwrap_or(Value::Null);
            let hname: Option<String> = r.get(8)?;
            let what = match r.get::<_, String>(1)?.as_str() {
                "email_one" => format!("email → {}", hname.clone().unwrap_or_default()),
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

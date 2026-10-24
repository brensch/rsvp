//! Personalised save-the-date / invitation emails, sent through Resend.

use std::collections::BTreeSet;

use anyhow::{Context, Result, bail};
use base64::Engine;
use maud::{DOCTYPE, html};
use serde_json::json;

use crate::assets::{self, Kind, Layout, Renderer};
use crate::config::{Config, Env};
use crate::store::{Guest, Household, Response};

/// Everyone in the household with an email address.
pub fn recipients(h: &Household, guests: &[Guest]) -> Vec<String> {
    let mut set = BTreeSet::new();
    if !h.email.trim().is_empty() {
        set.insert(h.email.trim().to_lowercase());
    }
    for g in guests.iter().filter(|g| g.plus_one_of.is_none()) {
        if !g.email.trim().is_empty() {
            set.insert(g.email.trim().to_lowercase());
        }
    }
    set.into_iter().collect()
}

pub struct Message {
    pub to: Vec<String>,
    pub subject: String,
    pub html: String,
    pub text: String,
    pub attachments: Vec<serde_json::Value>,
}

pub fn build(
    renderer: &Renderer,
    cfg: &Config,
    env: &Env,
    kind: &str,
    h: Household,
    guests: Vec<Guest>,
    responses: Vec<Response>,
    test_to: Option<String>,
) -> Result<Message> {
    let asset_kind = match kind {
        "save_the_date" => Kind::SaveTheDate,
        "invitation" => Kind::Invitation,
        _ => bail!("unknown email kind {kind}"),
    };
    let to = match test_to {
        Some(t) => vec![t],
        None => recipients(&h, &guests),
    };
    if to.is_empty() {
        bail!("household {} has no email address", h.id);
    }
    let token = crate::store::household_token(&env.secret, &h);
    let url = assets::link(cfg, &token);
    let greeting = h.greeting.clone();
    let australia_note = if h.australia { cfg.wedding.australia_note.clone() } else { None };
    let rows = vec![(h, guests, responses)];
    let doc = assets::render_households(renderer, cfg, env, asset_kind, Layout::Card, &rows)?;
    let png = assets::to_png(&doc, 0, 2.0)?;
    let b64 = base64::engine::general_purpose::STANDARD;
    let mut attachments = vec![json!({"filename": "card.png", "content": b64.encode(&png), "content_id": "card"})];
    if asset_kind == Kind::Invitation {
        attachments.push(json!({"filename": "invitation.pdf", "content": b64.encode(assets::to_pdf(&doc)?)}));
    }

    let names = format!("{} & {}", cfg.couple.a_first, cfg.couple.b_first);
    let date = cfg.wedding_date().map(assets::words::date_medium).unwrap_or_default();
    let deadline = crate::config::parse_date(&cfg.wedding.rsvp_deadline).map(assets::words::date_medium).unwrap_or_default();
    let (subject, kicker, mut lines, button): (String, &str, Vec<String>, &str) = if asset_kind == Kind::SaveTheDate {
        (
            format!("Save the date: {names}, {date}"),
            "Save the date",
            vec![
                format!("We're getting married on {date} in {}, and we would love you to be there.", cfg.wedding.location),
                "A formal invitation will follow. In the meantime, your personal page has the details and a calendar link.".into(),
            ],
            "Your save the date",
        )
    } else {
        (
            format!("You're invited: {names}"),
            "You're invited",
            vec![
                format!("We would be honoured to have you celebrate with us on {date} in {}.", cfg.wedding.location),
                format!("Your invitation is attached. Please let us know if you can make it by {deadline}. It only takes a minute."),
            ],
            "View details & RSVP",
        )
    };
    if let Some(n) = australia_note {
        lines.push(n);
    }
    let t = &cfg.theme;
    let html = html! {
        (DOCTYPE)
        html lang="en" {
            head {
                meta charset="utf-8";
                meta name="viewport" content="width=device-width, initial-scale=1";
                meta name="color-scheme" content="light only";
                title { (subject) }
            }
            body style={ "margin:0;padding:0;background:" (t.paper) ";" } {
                div style="display:none;max-height:0;overflow:hidden;opacity:0" { (lines[0]) }
                table role="presentation" width="100%" cellpadding="0" cellspacing="0" style={ "background:" (t.paper) ";" } {
                    tr { td align="center" style="padding:32px 16px" {
                        table role="presentation" width="100%" cellpadding="0" cellspacing="0" style="max-width:520px" {
                            tr { td align="center" style={ "font-family:Georgia,'Times New Roman',serif;font-size:12px;letter-spacing:4px;text-transform:uppercase;color:" (t.accent) ";padding-bottom:20px" } { (kicker) } }
                            tr { td align="center" {
                                a href=(url) { img src="cid:card" width="440" alt={ (names) ", " (date) } style="display:block;width:100%;max-width:440px;height:auto;border:0;box-shadow:0 8px 30px rgba(0,0,0,.12)"; }
                            } }
                            tr { td style={ "font-family:Georgia,'Times New Roman',serif;color:" (t.ink) ";font-size:17px;line-height:1.6;padding:32px 8px 8px" } {
                                p style="margin:0 0 16px;font-style:italic;font-size:20px" { "Dear " (greeting) "," }
                                @for l in &lines { p style="margin:0 0 16px" { (l) } }
                            } }
                            tr { td align="center" style="padding:16px 0 28px" {
                                a href=(url) style={ "display:inline-block;background:" (t.ink) ";color:" (t.paper) ";font-family:Georgia,serif;font-size:15px;letter-spacing:1px;text-decoration:none;padding:14px 28px;border-radius:2px" } { (button) }
                            } }
                            tr { td align="center" style={ "font-family:Georgia,serif;font-size:12px;color:" (t.muted) ";line-height:1.6" } {
                                "Or copy this link into your browser:" br;
                                a href=(url) style={ "color:" (t.muted) ";word-break:break-all" } { (url) }
                                br; br;
                                "With love, " (names)
                                br;
                                span style="font-size:11px" { "This link is personal to you, so please don't share it." }
                            } }
                        }
                    } }
                }
            }
        }
    }
    .into_string();
    let text = format!(
        "Dear {greeting},\n\n{}\n\n{button}: {url}\n\nWith love,\n{names}\n\n(This link is personal to you, so please don't share it.)\n",
        lines.join("\n\n")
    );
    Ok(Message { to, subject, html, text, attachments })
}

pub async fn send(http: &reqwest::Client, cfg: &Config, env: &Env, msg: &Message, idempotency_key: &str) -> Result<String> {
    let key = env.resend_key.as_deref().context("RESEND_API_KEY is not set")?;
    let from = cfg.email.from.as_deref().context("[email] from is not set in wedding.toml")?;
    let mut body = json!({
        "from": from,
        "to": msg.to,
        "subject": msg.subject,
        "html": msg.html,
        "text": msg.text,
        "attachments": msg.attachments,
    });
    if let Some(r) = &cfg.email.reply_to {
        body["reply_to"] = json!(r);
    }
    let resp = http
        .post("https://api.resend.com/emails")
        .bearer_auth(key)
        .header("Idempotency-Key", idempotency_key)
        .json(&body)
        .send()
        .await?;
    let status = resp.status();
    let text = resp.text().await.unwrap_or_default();
    if !status.is_success() {
        bail!("resend {status}: {}", text.chars().take(300).collect::<String>());
    }
    let id = serde_json::from_str::<serde_json::Value>(&text).ok().and_then(|v| v["id"].as_str().map(String::from)).unwrap_or_default();
    Ok(id)
}

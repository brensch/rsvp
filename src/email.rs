//! Personalised save-the-date / invitation emails, one per address, sent through Gmail.
//! The card picture loads from `/e/{send_id}.png`, which records that the email was opened.

use std::collections::BTreeSet;

use anyhow::{Result, bail};
use maud::{DOCTYPE, html};

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
    pub to: String,
    pub subject: String,
    pub html: String,
    pub text: String,
    pub attachments: Vec<crate::gmail::Attachment>,
    /// The card picture, served (and counted) at `/e/{send_id}.png`.
    pub card_png: Vec<u8>,
}

/// Where the card picture for a household's email is kept for `/e/...` to serve.
pub fn card_file(env: &Env, household_id: i64, kind: &str) -> std::path::PathBuf {
    env.data_dir.join("mail").join(format!("{household_id}-{kind}.png"))
}

pub fn tracking_url(cfg: &Config, send_id: &str) -> String {
    format!("{}/e/{send_id}.png", cfg.site.base_url)
}

pub fn build(
    renderer: &Renderer,
    cfg: &Config,
    env: &Env,
    kind: &str,
    h: Household,
    guests: Vec<Guest>,
    responses: Vec<Response>,
    to: &str,
    send_id: &str,
) -> Result<Message> {
    let asset_kind = match kind {
        "save_the_date" => Kind::SaveTheDate,
        "invitation" => Kind::Invitation,
        _ => bail!("unknown email kind {kind}"),
    };
    let token = crate::store::household_token(&env.secret, &h);
    let url = assets::link(cfg, &token);
    let greeting = h.greeting.clone();
    let australia = h.australia;

    let mut attachments = vec![];
    if asset_kind == Kind::Invitation {
        let rows = vec![(h, guests, responses)];
        let doc = assets::render_households(renderer, cfg, env, asset_kind, Layout::Card, &rows)?;
        attachments.push(crate::gmail::Attachment { filename: "invitation.pdf".into(), mime: "application/pdf".into(), bytes: assets::to_pdf(&doc)? });
    }
    let card_src = tracking_url(cfg, send_id);

    let pr = &cfg.prose;
    let owned = crate::prose::vars(cfg, &greeting);
    let vars = crate::prose::borrow(&owned);
    let k = if asset_kind == Kind::SaveTheDate { "email.save_the_date" } else { "email.invitation" };
    let subject = pr.f(&format!("{k}.subject"), &vars);
    let kicker = pr.f(&format!("{k}.kicker"), &vars);
    let sentence = pr.f(&format!("{k}.text"), &vars);
    let button = pr.f(&format!("{k}.button"), &vars);
    let wedding_of = pr.opt("website.the_wedding_of");
    let mut extra = Vec::new();
    if australia {
        extra.extend(pr.opt("notes.australia"));
    }
    let date_line = cfg
        .wedding_date()
        .map(|d| format!("{} · {} {} · {}", assets::words::weekday_name(d.weekday()), d.day(), assets::words::month_name(d.month()), d.year()))
        .unwrap_or_default();
    // The top of the email is one picture typeset in the site's own fonts (served from
    // /e/<id>.png, which records the open). Its alt text carries the same words.
    let card_png = renderer.email_header_png(cfg, &kicker, 1080.0)?;
    let header_alt = format!(
        "{kicker}. {}{} & {}, {date_line}, {}",
        wedding_of.as_deref().map(|w| format!("{w} ")).unwrap_or_default(),
        cfg.couple.a_first,
        cfg.couple.b_first,
        cfg.wedding.location
    );
    let t = &cfg.theme;
    let serif = "Georgia,'Times New Roman',serif";
    // Laid out like the save the date page: kicker, names, divider, date and place, then the
    // sentence and the button. Email apps don't load web fonts, so it's Georgia throughout.
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
                table role="presentation" width="100%" cellpadding="0" cellspacing="0" style={ "background:" (t.paper) ";" } {
                    tr { td align="center" style="padding:16px 6px 40px" {
                        table role="presentation" width="100%" cellpadding="0" cellspacing="0" style={ "max-width:540px;font-family:" (serif) ";text-align:center" } {
                            tr { td align="center" {
                                a href=(url) { img src=(card_src) width="540" alt=(header_alt) style={ "display:block;margin:0 auto;width:100%;max-width:540px;height:auto;border:0;font-family:" (serif) ";font-size:16px;color:" (t.ink) }; }
                            } }
                            @if !sentence.trim().is_empty() || !extra.is_empty() {
                                tr { td style={ "font-size:17px;line-height:1.6;color:" (t.ink) ";padding:30px 4px 0" } {
                                    (sentence)
                                    @for n in &extra { p style={ "margin:14px 0 0;font-size:15px;font-style:italic;color:" (t.muted) } { (n) } }
                                } }
                            }
                            tr { td style="padding:28px 0 0" {
                                a href=(url) style={ "display:inline-block;background:" (t.accent) ";color:" (t.paper) ";font-size:15px;letter-spacing:2px;text-transform:uppercase;text-decoration:none;padding:15px 30px;border-radius:2px" } { (button) }
                            } }
                        }
                    } }
                }
            }
        }
    }
    .into_string();
    let names = format!("{} & {}", cfg.couple.a_first, cfg.couple.b_first);
    let mut text = format!("{}\n\n{names}\n{date_line}\n{}\n\n", kicker.to_uppercase(), cfg.wedding.location);
    if !sentence.trim().is_empty() {
        text.push_str(&format!("{sentence}\n\n"));
    }
    text.push_str(&format!("{button}: {url}\n"));
    for n in &extra {
        text.push_str(&format!("\n{n}\n"));
    }
    Ok(Message { to: to.to_string(), subject, html, text, attachments, card_png })
}

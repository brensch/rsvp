//! Wedding configuration (`wedding.toml`) and runtime secrets (environment).

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use time::{Date, PrimitiveDateTime, UtcOffset};

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Config {
    /// Guest-facing wording (copy.yaml next to this file, over the built-in defaults).
    #[serde(skip)]
    pub prose: crate::prose::Prose,
    pub couple: Couple,
    pub wedding: Wedding,
    pub site: Site,
    #[serde(default)]
    pub theme: Theme,
    #[serde(default)]
    pub events: Vec<Event>,
    #[serde(default)]
    pub meals: Vec<Meal>,
    #[serde(default)]
    pub rsvp: RsvpOptions,
    #[serde(default)]
    pub email: EmailConfig,
    #[serde(default)]
    pub backup: BackupConfig,
    #[serde(default)]
    pub print: PrintConfig,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Couple {
    /// First names, used in large display type ("Alex").
    pub a_first: String,
    pub a_last: String,
    pub b_first: String,
    pub b_last: String,
    /// Optional custom monogram; defaults to initials "A & S".
    #[serde(default)]
    pub monogram: Option<String>,
    /// Lines that open the formal invitation, e.g. "Together with their families".
    #[serde(default = "default_host_line")]
    pub host_line: String,
}

fn default_host_line() -> String {
    "Together with their families".into()
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Wedding {
    /// ISO date, e.g. 2027-06-12.
    pub date: String,
    /// "Hobart, Tasmania"
    pub location: String,
    /// UTC offset of the venue, e.g. "+10:00". Used for .ics files and deadlines.
    #[serde(default = "default_offset")]
    pub utc_offset: String,
    /// ISO date after which the RSVP form locks.
    pub rsvp_deadline: String,
    #[serde(default)]
    pub return_address: Vec<String>,
    /// Free-form markdown-ish paragraphs shown on the guest page (travel, accommodation, etc).
    #[serde(default)]
    pub info: Vec<InfoSection>,
}

fn default_offset() -> String {
    "+00:00".into()
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct InfoSection {
    pub title: String,
    pub body: String,
    /// "all" (default), "australia" (only Australia-based households) or "us" (everyone else).
    #[serde(default)]
    pub audience: Option<String>,
}



#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Site {
    /// Public origin, no trailing slash: https://wedding.example.com
    pub base_url: String,
    /// WebAuthn relying party ID; normally the bare domain.
    pub rp_id: String,
    #[serde(default = "default_bind")]
    pub bind: String,
    /// Display title of the site.
    #[serde(default)]
    pub title: Option<String>,
}

fn default_bind() -> String {
    "127.0.0.1:8080".into()
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Theme {
    /// The American flower in the emblem: "poppy" (California) or "rose" (US national flower).
    #[serde(default = "default_us_flower")]
    pub us_flower: String,
    pub ink: String,
    pub muted: String,
    pub accent: String,
    pub paper: String,
    pub rule: String,
}

fn default_us_flower() -> String {
    "poppy".into()
}

impl Default for Theme {
    fn default() -> Self {
        Self {
            us_flower: default_us_flower(),
            ink: "#24211d".into(),
            muted: "#6f675c".into(),
            accent: "#8c6a3f".into(),
            paper: "#fbf8f1".into(),
            rule: "#c9b99a".into(),
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Event {
    pub slug: String,
    pub name: String,
    /// Short label for compact admin views ("Rehearsal"); defaults to `name`.
    #[serde(default)]
    pub short: Option<String>,
    /// Local time at venue, "2027-06-12T15:00". Omit while the date is still to be decided:
    /// the event shows as "date to be announced" and isn't on the RSVP form yet.
    #[serde(default)]
    pub starts: Option<String>,
    #[serde(default)]
    pub ends: Option<String>,
    pub venue: String,
    #[serde(default)]
    pub address: String,
    /// What the map link searches for. Defaults to "venue, address", which finds named places
    /// well; set it to just the address for e.g. a private home.
    #[serde(default)]
    pub map_query: Option<String>,
    /// An exact map link (e.g. Google Maps with the venue's place ID). Takes priority over the search.
    #[serde(default)]
    pub map_url: Option<String>,
    #[serde(default)]
    pub dress: Option<String>,
    #[serde(default)]
    pub note: Option<String>,
    /// Whether guests choose a meal for this event.
    #[serde(default)]
    pub meal: bool,
    /// Whether new households are invited to this event by default.
    #[serde(default = "yes")]
    pub default_invite: bool,
    /// Printed on the wedding invitation's details card (false for e.g. engagement parties).
    #[serde(default = "yes")]
    pub on_invitation: bool,
}

impl Event {
    pub fn map_url(&self) -> String {
        if let Some(u) = self.map_url.as_deref().filter(|u| u.starts_with("https://")) {
            return u.to_string();
        }
        let q = self.map_query.clone().unwrap_or_else(|| format!("{}, {}", self.venue, self.address));
        format!("https://www.google.com/maps/search/?api=1&query={}", urlencoding::encode(&q))
    }

    pub fn short_name(&self) -> &str {
        self.short.as_deref().unwrap_or(&self.name)
    }
    pub fn start(&self) -> Option<PrimitiveDateTime> {
        self.starts.as_deref().and_then(|s| parse_local(s).ok())
    }
    /// Guests can RSVP once the event has a date.
    pub fn dated(&self) -> bool {
        self.start().is_some()
    }
}

fn yes() -> bool {
    true
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Meal {
    pub slug: String,
    pub name: String,
    #[serde(default)]
    pub description: String,
    /// Kids' menu items are only offered to guests flagged as children.
    #[serde(default)]
    pub kids: bool,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct RsvpOptions {
    #[serde(default = "yes")]
    pub ask_dietary: bool,
    #[serde(default = "yes")]
    pub ask_song: bool,
    #[serde(default = "yes")]
    pub ask_message: bool,
    #[serde(default = "yes")]
    pub ask_accommodation: bool,
    #[serde(default = "yes")]
    pub ask_transport: bool,
    #[serde(default = "default_transport_q")]
    pub transport_question: String,
}

fn default_transport_q() -> String {
    "Would you like a seat on the shuttle between venues?".into()
}

impl Default for RsvpOptions {
    fn default() -> Self {
        Self {
            ask_dietary: true,
            ask_song: true,
            ask_message: true,
            ask_accommodation: true,
            ask_transport: true,
            transport_question: default_transport_q(),
        }
    }
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct EmailConfig {
    /// Display name on emails, e.g. "Alex & Sam" (the address is the connected Gmail account's).
    #[serde(default)]
    pub from: Option<String>,
    /// The Gmail account emails must come from; connecting any other account is refused.
    #[serde(default)]
    pub account: Option<String>,
    /// Safety lock: while set, every email goes to this address instead of its real recipient
    /// (the subject says who it was for). Sends are otherwise recorded as normal, so the whole
    /// flow can be tested; wipe that state before going live. Remove to send for real.
    #[serde(default)]
    pub redirect_all_to: Option<String>,
    #[serde(default)]
    pub reply_to: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct BackupConfig {
    /// GCS bucket for daily snapshots (retention locked). Empty = local only.
    #[serde(default)]
    pub bucket: Option<String>,
    /// UTC hour to take the daily snapshot.
    #[serde(default = "default_hour")]
    pub hour_utc: u8,
    #[serde(default = "default_keep")]
    pub keep_local: usize,
}

fn default_hour() -> u8 {
    17
}
fn default_keep() -> usize {
    14
}

impl Default for BackupConfig {
    fn default() -> Self {
        Self { bucket: None, hour_utc: default_hour(), keep_local: default_keep() }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct PrintConfig {
    /// "a4" or "us-letter"; used for home-print sheets and address labels.
    #[serde(default = "default_paper")]
    pub paper: String,
}

fn default_paper() -> String {
    "a4".into()
}

impl Default for PrintConfig {
    fn default() -> Self {
        Self { paper: default_paper() }
    }
}

impl Config {
    /// The email safety lock's address, if the lock is on.
    pub fn email_lock(&self) -> Option<&str> {
        self.email.redirect_all_to.as_deref().map(str::trim).filter(|s| !s.is_empty())
    }

    pub fn load(path: &Path) -> Result<Self> {
        let raw = std::fs::read_to_string(path)
            .with_context(|| format!("reading config {}", path.display()))?;
        let mut cfg: Config = toml::from_str(&raw).context("parsing wedding.toml")?;
        cfg.prose = crate::prose::Prose::load(&path.with_file_name("copy.yaml"))?;
        cfg.validate()?;
        Ok(cfg)
    }

    fn validate(&self) -> Result<()> {
        self.wedding_date()?;
        self.deadline()?;
        self.offset()?;
        if self.site.base_url.ends_with('/') {
            bail!("site.base_url must not end with '/'");
        }
        let mut seen = std::collections::HashSet::new();
        for e in &self.events {
            if !valid_slug(&e.slug) || !seen.insert(&e.slug) {
                bail!("event slug '{}' must be unique, lowercase a-z0-9-", e.slug);
            }
            if let Some(st) = &e.starts {
                parse_local(st).with_context(|| format!("event {} starts", e.slug))?;
            } else if e.ends.is_some() {
                bail!("event {} has an end time but no start", e.slug);
            }
            if let Some(end) = &e.ends {
                parse_local(end).with_context(|| format!("event {} ends", e.slug))?;
            }
        }
        let mut seen = std::collections::HashSet::new();
        for m in &self.meals {
            if !valid_slug(&m.slug) || !seen.insert(&m.slug) {
                bail!("meal slug '{}' must be unique, lowercase a-z0-9-", m.slug);
            }
        }
        for s in &self.wedding.info {
            if !matches!(s.audience.as_deref(), None | Some("all") | Some("australia") | Some("us")) {
                bail!("wedding.info '{}': audience must be all, australia or us", s.title);
            }
        }
        if self.events.is_empty() {
            bail!("at least one [[events]] entry is required");
        }
        Ok(())
    }

    pub fn wedding_date(&self) -> Result<Date> {
        parse_date(&self.wedding.date).context("wedding.date")
    }
    pub fn deadline(&self) -> Result<Date> {
        parse_date(&self.wedding.rsvp_deadline).context("wedding.rsvp_deadline")
    }
    pub fn offset(&self) -> Result<UtcOffset> {
        let f = time::macros::format_description!("[offset_hour sign:mandatory]:[offset_minute]");
        UtcOffset::parse(&self.wedding.utc_offset, &f).context("wedding.utc_offset")
    }
    pub fn event(&self, slug: &str) -> Option<&Event> {
        self.events.iter().find(|e| e.slug == slug)
    }
    pub fn meal(&self, slug: &str) -> Option<&Meal> {
        self.meals.iter().find(|m| m.slug == slug)
    }
    pub fn title(&self) -> String {
        self.site
            .title
            .clone()
            .unwrap_or_else(|| format!("{} & {}", self.couple.a_first, self.couple.b_first))
    }
    pub fn monogram(&self) -> String {
        self.couple.monogram.clone().unwrap_or_else(|| {
            format!(
                "{}{}",
                self.couple.a_first.chars().next().unwrap_or('A'),
                self.couple.b_first.chars().next().unwrap_or('B')
            )
        })
    }
    /// True once the RSVP deadline (end of day, venue time) has passed.
    pub fn past_deadline(&self) -> bool {
        let (Ok(d), Ok(off)) = (self.deadline(), self.offset()) else { return false };
        let end = d.with_hms(23, 59, 59).unwrap().assume_offset(off);
        time::OffsetDateTime::now_utc() > end
    }
}

pub fn valid_slug(s: &str) -> bool {
    !s.is_empty() && s.len() <= 40 && s.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

pub fn parse_date(s: &str) -> Result<Date> {
    let f = time::macros::format_description!("[year]-[month]-[day]");
    Ok(Date::parse(s, &f)?)
}

pub fn parse_local(s: &str) -> Result<PrimitiveDateTime> {
    let f = time::macros::format_description!("[year]-[month]-[day]T[hour]:[minute]");
    Ok(PrimitiveDateTime::parse(s, &f)?)
}

/// Secrets and paths that never live in the config file.
#[derive(Clone)]
pub struct Env {
    pub data_dir: PathBuf,
    /// 32+ byte key for deriving guest link tokens. Rotating it invalidates every link.
    pub secret: Vec<u8>,
    pub discord_webhook: Option<String>,
    /// OAuth client for sending through Gmail (Settings, Connect Gmail).
    pub gmail_client_id: Option<String>,
    pub gmail_client_secret: Option<String>,
    /// Google Address Validation API key (server-side only; restrict it to the server's IP).
    pub google_maps_key: Option<String>,
}

impl Env {
    pub fn from_env(data_dir: PathBuf) -> Result<Self> {
        let secret_hex = std::env::var("RSVP_SECRET").context(
            "RSVP_SECRET is not set (generate one with `rsvp gen-secret` and put it in the env file)",
        )?;
        let secret = hex::decode(secret_hex.trim()).context("RSVP_SECRET must be hex")?;
        if secret.len() < 32 {
            bail!("RSVP_SECRET must be at least 32 bytes (64 hex chars)");
        }
        let opt = |k: &str| std::env::var(k).ok().filter(|v| !v.trim().is_empty());
        Ok(Self {
            data_dir,
            secret,
            discord_webhook: opt("DISCORD_WEBHOOK_URL"),
            gmail_client_id: opt("GMAIL_CLIENT_ID"),
            gmail_client_secret: opt("GMAIL_CLIENT_SECRET"),
            google_maps_key: opt("GOOGLE_MAPS_API_KEY"),
        })
    }

    pub fn db_path(&self) -> PathBuf {
        self.data_dir.join("rsvp.db")
    }
    pub fn journal_path(&self) -> PathBuf {
        self.data_dir.join("journal.jsonl")
    }
    pub fn backup_dir(&self) -> PathBuf {
        self.data_dir.join("backups")
    }
}

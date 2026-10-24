//! Command line: runs the server, and lets operators (and the Claude agent on
//! the box) manage the guest list directly. Every change is audited with the
//! actor from RSVP_ACTOR (default "cli:$USER").

use std::collections::HashMap;
use std::io::Read;
use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use clap::{Args, Parser, Subcommand};
use serde_json::{Value, json};

use crate::assets::{self, Kind, Layout, Renderer};
use crate::config::{Config, Env};
use crate::db::Db;
use crate::journal::Journal;
use crate::store::{self, GuestInput, HouseholdInput};

#[derive(Parser)]
#[command(name = "rsvp", version, about = "Wedding RSVP portal and guest-list tool")]
pub struct Cli {
    /// Path to wedding.toml
    #[arg(long, env = "RSVP_CONFIG", default_value = "/etc/rsvp/wedding.toml", global = true)]
    pub config: PathBuf,
    /// Data directory (database, journal, backups, template overrides)
    #[arg(long, env = "RSVP_DATA", default_value = "/var/lib/rsvp", global = true)]
    pub data: PathBuf,
    #[command(subcommand)]
    pub cmd: Cmd,
}

#[derive(Subcommand)]
pub enum Cmd {
    /// Run the web server (plus outbox worker and backup scheduler)
    Serve,
    /// Print a new random RSVP_SECRET
    GenSecret,
    /// Validate config, database and journal
    Check,
    /// Create a one-time admin invite link
    Invite {
        #[arg(long, default_value = "owner")]
        role: String,
        #[arg(long, default_value_t = 48)]
        hours: i64,
    },
    /// Admin users
    #[command(subcommand)]
    Users(UsersCmd),
    /// Show or set the guest-facing phase: save-the-date | rsvp-open | rsvp-closed
    Phase { value: Option<String> },
    /// Households (invitation units)
    #[command(subcommand)]
    Household(HouseholdCmd),
    /// Guests (people)
    #[command(subcommand)]
    Guest(GuestCmd),
    /// RSVP summary
    Rsvps {
        #[arg(long)]
        json: bool,
    },
    /// Show configured events and meals (valid slugs)
    Events,
    /// Render stationery to a file
    Render(RenderArgs),
    /// Take a backup snapshot now
    Backup,
    /// RSVP journal tools
    #[command(subcommand)]
    Journal(JournalCmd),
    /// Recent audit log
    Audit {
        #[arg(long, default_value_t = 50)]
        limit: i64,
    },
    /// Re-derive guest link hashes from RSVP_SECRET (after a restore)
    RehashTokens,
    /// Queue a test Discord message through the outbox (the running server delivers it)
    NotifyTest,
}

#[derive(Subcommand)]
pub enum UsersCmd {
    List,
    /// Set role: owner | helper | pending | disabled
    SetRole { name: String, role: String },
}

#[derive(Subcommand)]
pub enum HouseholdCmd {
    /// List households (with guests and status)
    List {
        #[arg(long)]
        json: bool,
        #[arg(long)]
        all: bool,
    },
    /// Show one household with guests, responses and link
    Show { id: i64 },
    /// Create from JSON (HouseholdInput, may include "guests": [...]). Use '-' to read stdin.
    Add {
        #[arg(long)]
        json: String,
    },
    /// Create many from a JSON array file of HouseholdInput objects ('-' for stdin)
    Import { file: String },
    /// Update fields from JSON (only the keys you pass change)
    Update {
        id: i64,
        #[arg(long)]
        json: String,
    },
    Archive { id: i64 },
    Restore { id: i64 },
    /// Print the guest link
    Link { id: i64 },
    /// Replace the guest link (old link stops working)
    RotateLink { id: i64 },
    /// Search by name, greeting, guest name, email or tag
    Find { query: String },
}

#[derive(Subcommand)]
pub enum GuestCmd {
    /// Add a person to a household from JSON (GuestInput)
    Add {
        household: i64,
        #[arg(long)]
        json: String,
    },
    Update {
        id: i64,
        #[arg(long)]
        json: String,
    },
    /// Move a person to another household
    Move { id: i64, household: i64 },
    Archive { id: i64 },
}

#[derive(Subcommand)]
pub enum JournalCmd {
    /// List journal entries missing from the database (should be none)
    Verify,
    /// Apply missing journal entries to the database
    Replay,
}

#[derive(Args)]
pub struct RenderArgs {
    /// save-the-date | invitation | envelope | labels | place-cards | report
    pub kind: String,
    /// Only this household (default: all)
    #[arg(long)]
    pub household: Option<i64>,
    /// card (print shop, bleed) | sheet (home print, 2-up)
    #[arg(long, default_value = "card")]
    pub layout: String,
    /// pdf | png (first page)
    #[arg(long, default_value = "pdf")]
    pub format: String,
    #[arg(short, long)]
    pub out: PathBuf,
}

fn actor() -> String {
    std::env::var("RSVP_ACTOR").unwrap_or_else(|_| format!("cli:{}", std::env::var("USER").unwrap_or_else(|_| "unknown".into())))
}

fn read_json(arg: &str) -> Result<Value> {
    let raw = if arg == "-" {
        let mut s = String::new();
        std::io::stdin().read_to_string(&mut s)?;
        s
    } else if let Some(path) = arg.strip_prefix('@') {
        std::fs::read_to_string(path)?
    } else {
        arg.to_string()
    };
    serde_json::from_str(&raw).context("invalid JSON")
}

fn print(v: &Value) {
    println!("{}", serde_json::to_string_pretty(v).unwrap());
}

/// The secret is only needed for link operations; load it on demand.
fn env(data: &PathBuf) -> Result<Env> {
    Env::from_env(data.clone())
}

pub fn run_sync(cli: Cli) -> Result<()> {
    if let Cmd::GenSecret = cli.cmd {
        let mut b = [0u8; 32];
        rand::RngCore::fill_bytes(&mut rand::rng(), &mut b);
        println!("{}", hex::encode(b));
        return Ok(());
    }
    let cfg = Config::load(&cli.config)?;
    let db = Db::open(&cli.data.join("rsvp.db"))?;
    let events: Vec<String> = cfg.events.iter().map(|e| e.slug.clone()).collect();
    let defaults: Vec<String> = cfg.events.iter().filter(|e| e.default_invite).map(|e| e.slug.clone()).collect();
    let who = actor();

    match cli.cmd {
        Cmd::Serve | Cmd::GenSecret | Cmd::Backup => unreachable!("handled in main"),
        Cmd::Check => {
            let env = env(&cli.data)?;
            let journal = Journal::new(env.journal_path());
            let (integrity, missing, households, schema, columns) = db.with(|c| {
                let ok: String = c.query_row("PRAGMA integrity_check", [], |r| r.get(0))?;
                let schema: i64 = c.query_row("PRAGMA user_version", [], |r| r.get(0))?;
                let mut columns = serde_json::Map::new();
                for t in ["households", "guests"] {
                    let cols: Vec<String> =
                        c.prepare(&format!("PRAGMA table_info({t})"))?.query_map([], |r| r.get(1))?.collect::<rusqlite::Result<_>>()?;
                    columns.insert(t.into(), json!(cols));
                }
                Ok((ok, crate::rsvp::journal_missing(c, &journal)?.len(), store::list_households(c, false)?.len(), schema, columns))
            })?;
            Renderer::new(&cli.data).monogram_svg(&cfg).context("rendering templates")?;
            print(&json!({"config": "ok", "integrity": integrity, "journal_missing": missing, "households": households,
                "schema_version": schema, "columns": columns, "templates": "ok"}));
            if integrity != "ok" || missing > 0 {
                bail!("check failed");
            }
        }
        Cmd::Invite { role, hours } => {
            let code = db.with(|c| store::create_invite(c, &who, &role, hours))?;
            println!("{}/admin/register?invite={code}", cfg.site.base_url);
            eprintln!("One-time link for a new {role}; expires in {hours}h.");
        }
        Cmd::Users(UsersCmd::List) => print(&serde_json::to_value(db.with(|c| store::users(c))?)?),
        Cmd::Users(UsersCmd::SetRole { name, role }) => {
            db.with(|c| {
                let u = store::user_by_name(c, &name)?.with_context(|| format!("no user named {name}"))?;
                store::set_user_role(c, &who, &u.id, &role)
            })?;
            println!("{name} is now {role}");
        }
        Cmd::Phase { value } => match value {
            Some(p) => {
                let phase = store::Phase::parse(&p).context("phase must be save-the-date, rsvp-open or rsvp-closed")?;
                db.with(|c| store::set_setting(c, &who, "phase", phase.as_str()))?;
                println!("{}", phase.as_str());
            }
            None => println!("{}", db.with(|c| store::phase(c))?.as_str()),
        },
        Cmd::Events => print(&json!({"events": cfg.events, "meals": cfg.meals})),
        Cmd::Household(cmd) => household(&db, &cfg, &cli.data, &who, &events, &defaults, cmd)?,
        Cmd::Guest(cmd) => match cmd {
            GuestCmd::Add { household, json } => {
                let input: GuestInput = serde_json::from_value(read_json(&json)?)?;
                let id = db.with(|c| store::create_guest(c, &who, household, &input))?;
                print(&json!({"created_guest": id}));
            }
            GuestCmd::Update { id, json } => {
                let input: GuestInput = serde_json::from_value(read_json(&json)?)?;
                db.with(|c| store::update_guest(c, &who, id, &input))?;
                print(&serde_json::to_value(db.with(|c| store::must_guest(c, id))?)?);
            }
            GuestCmd::Move { id, household } => {
                db.with(|c| store::move_guest(c, &who, id, household))?;
                println!("moved guest {id} to household {household}");
            }
            GuestCmd::Archive { id } => {
                db.with(|c| store::archive_guest(c, &who, id))?;
                println!("archived guest {id}");
            }
        },
        Cmd::Rsvps { json: as_json } => rsvps(&db, &cfg, as_json)?,
        Cmd::Render(args) => {
            let env = env(&cli.data)?;
            let kind = Kind::parse(&args.kind).with_context(|| format!("unknown kind {}", args.kind))?;
            let renderer = Renderer::new(&cli.data);
            let doc = db.with(|c| {
                if kind == Kind::Report {
                    let data = assets::report_data(&cfg, &store::list_households(c, false)?, &store::all_guests(c)?, &store::all_responses(c)?)?;
                    return renderer.compile(kind, &data, HashMap::new());
                }
                let mut rows = vec![];
                for h in store::list_households(c, false)? {
                    if args.household.is_some_and(|id| id != h.id) {
                        continue;
                    }
                    let (g, r) = (store::guests_for(c, h.id)?, store::responses_for(c, h.id)?);
                    rows.push((h, g, r));
                }
                assets::render_households(&renderer, &cfg, &env, kind, Layout::parse(&args.layout), &rows)
            })?;
            let bytes = if args.format == "png" { assets::to_png(&doc, 0, 3.0)? } else { assets::to_pdf(&doc)? };
            std::fs::write(&args.out, bytes)?;
            println!("wrote {} ({} pages)", args.out.display(), doc.pages().len());
        }
        Cmd::Journal(cmd) => {
            let env = env(&cli.data)?;
            let journal = Journal::new(env.journal_path());
            let missing = db.with(|c| crate::rsvp::journal_missing(c, &journal))?;
            match cmd {
                JournalCmd::Verify => {
                    print(&json!({"missing": missing.iter().map(|e| json!({"id": e.id, "household_id": e.household_id, "received_at": e.received_at})).collect::<Vec<_>>()}));
                    if !missing.is_empty() {
                        bail!("{} journal entries missing from the database; run `rsvp journal replay`", missing.len());
                    }
                }
                JournalCmd::Replay => {
                    let mut n = 0;
                    db.with(|c| {
                        for e in &missing {
                            if crate::rsvp::apply(c, &cfg, e, false)? {
                                n += 1;
                            }
                        }
                        store::audit(c, &who, "journal.replay", "", "", &json!({"applied": n}))
                    })?;
                    println!("replayed {n} entries");
                }
            }
        }
        Cmd::Audit { limit } => print(&serde_json::to_value(db.with(|c| store::audit_log(c, limit, None))?)?),
        Cmd::NotifyTest => {
            let id = db.with(|c| store::enqueue(c, "discord", &json!({"type": "test", "name": who})))?;
            println!("queued outbox #{id}; the server delivers it within a few seconds");
        }
        Cmd::RehashTokens => {
            let env = env(&cli.data)?;
            let n = db.with(|c| store::rehash_all_tokens(c, &env.secret))?;
            println!("rehashed {n} households");
        }
    }
    Ok(())
}

fn household(db: &Db, cfg: &Config, data: &PathBuf, who: &str, events: &[String], defaults: &[String], cmd: HouseholdCmd) -> Result<()> {
    match cmd {
        HouseholdCmd::List { json: as_json, all } => {
            let (hs, guests, resp) = db.with(|c| Ok((store::list_households(c, all)?, store::all_guests(c)?, store::all_responses(c)?)))?;
            if as_json {
                let out: Vec<Value> = hs
                    .iter()
                    .map(|h| {
                        let gs: Vec<_> = guests.iter().filter(|g| g.household_id == h.id).collect();
                        let status = store::household_status(h, &gs.iter().map(|g| (*g).clone()).collect::<Vec<_>>(), &resp);
                        json!({"household": h, "guests": gs, "status": status})
                    })
                    .collect();
                print(&json!(out));
            } else {
                println!("{:>4}  {:<28} {:<4} {:<10} {:<5} {:<14} PEOPLE", "ID", "NAME", "TIER", "STATUS", "SIDE", "EVENTS");
                for h in &hs {
                    let gs: Vec<_> = guests.iter().filter(|g| g.household_id == h.id).cloned().collect();
                    let status = store::household_status(h, &gs, &resp);
                    let people: Vec<String> = gs.iter().map(|g| format!("{}#{}", g.full_name(), g.id)).collect();
                    println!(
                        "{:>4}  {:<28} {:<4} {:<10} {:<5} {:<14} {}",
                        h.id,
                        h.name.chars().take(28).collect::<String>(),
                        h.tier,
                        format!("{status:?}"),
                        h.side,
                        h.events.join(","),
                        people.join(", ")
                    );
                }
                println!("{} households, {} people", hs.len(), guests.len());
            }
        }
        HouseholdCmd::Show { id } => {
            let env = env(data)?;
            let (h, guests, responses, subs) =
                db.with(|c| Ok((store::must_household(c, id)?, store::guests_for(c, id)?, store::responses_for(c, id)?, store::submissions(c, Some(id), 5)?)))?;
            let link = assets::link(cfg, &store::household_token(&env.secret, &h));
            print(&json!({"household": h, "guests": guests, "responses": responses, "recent_submissions": subs, "link": link}));
        }
        HouseholdCmd::Add { json } => {
            let env = env(data)?;
            let input: HouseholdInput = serde_json::from_value(read_json(&json)?)?;
            input.validate(events)?;
            let id = db.with(|c| store::create_household(c, who, &env.secret, &input, defaults))?;
            let h = db.with(|c| store::must_household(c, id))?;
            print(&json!({"created_household": id, "link": assets::link(cfg, &store::household_token(&env.secret, &h))}));
        }
        HouseholdCmd::Import { file } => {
            let env = env(data)?;
            let list: Vec<HouseholdInput> = serde_json::from_value(read_json(&if file == "-" { file } else { format!("@{file}") })?)?;
            for (i, h) in list.iter().enumerate() {
                h.validate(events).with_context(|| format!("entry {i}"))?;
            }
            let ids = db.with(|c| {
                let tx = crate::db::begin(c)?;
                let mut ids = vec![];
                for h in &list {
                    ids.push(store::create_household(&tx, who, &env.secret, h, defaults)?);
                }
                tx.commit()?;
                Ok(ids)
            })?;
            print(&json!({"created_households": ids}));
        }
        HouseholdCmd::Update { id, json } => {
            let input: HouseholdInput = serde_json::from_value(read_json(&json)?)?;
            input.validate(events)?;
            db.with(|c| store::update_household(c, who, id, &input))?;
            print(&serde_json::to_value(db.with(|c| store::must_household(c, id))?)?);
        }
        HouseholdCmd::Archive { id } => {
            db.with(|c| store::set_household_archived(c, who, id, true))?;
            println!("archived household {id}");
        }
        HouseholdCmd::Restore { id } => {
            db.with(|c| store::set_household_archived(c, who, id, false))?;
            println!("restored household {id}");
        }
        HouseholdCmd::Link { id } => {
            let env = env(data)?;
            let h = db.with(|c| store::must_household(c, id))?;
            println!("{}", assets::link(cfg, &store::household_token(&env.secret, &h)));
        }
        HouseholdCmd::RotateLink { id } => {
            let env = env(data)?;
            let token = db.with(|c| store::rotate_token(c, who, &env.secret, id))?;
            println!("{}", assets::link(cfg, &token));
        }
        HouseholdCmd::Find { query } => {
            let q = query.to_lowercase();
            let (hs, guests) = db.with(|c| Ok((store::list_households(c, true)?, store::all_guests(c)?)))?;
            let out: Vec<Value> = hs
                .iter()
                .filter(|h| {
                    [&h.name, &h.greeting, &h.email, &h.tags].iter().any(|f| f.to_lowercase().contains(&q))
                        || guests.iter().any(|g| g.household_id == h.id && (g.full_name().to_lowercase().contains(&q) || g.email.to_lowercase().contains(&q)))
                })
                .map(|h| json!({"id": h.id, "name": h.name, "archived": h.archived_at.is_some(), "guests": guests.iter().filter(|g| g.household_id == h.id).map(|g| json!({"id": g.id, "name": g.full_name()})).collect::<Vec<_>>()}))
                .collect();
            print(&json!(out));
        }
    }
    Ok(())
}

fn rsvps(db: &Db, cfg: &Config, as_json: bool) -> Result<()> {
    let (hs, guests, resp) = db.with(|c| Ok((store::list_households(c, false)?, store::all_guests(c)?, store::all_responses(c)?)))?;
    let mut events = vec![];
    for e in &cfg.events {
        let invited: Vec<_> = guests.iter().filter(|g| hs.iter().any(|h| h.id == g.household_id && g.invited(h, &e.slug))).collect();
        let yes = invited.iter().filter(|g| resp.get(&(g.id, e.slug.clone())).is_some_and(|r| r.attending)).count();
        let no = invited.iter().filter(|g| resp.get(&(g.id, e.slug.clone())).is_some_and(|r| !r.attending)).count();
        let meals: HashMap<String, usize> = cfg
            .meals
            .iter()
            .map(|m| (m.slug.clone(), resp.values().filter(|r| r.event_slug == e.slug && r.attending && r.meal == m.slug).count()))
            .filter(|(_, n)| *n > 0)
            .collect();
        events.push(json!({"event": e.slug, "invited": invited.len(), "yes": yes, "no": no, "waiting": invited.len() - yes - no, "meals": meals}));
    }
    let replied = hs.iter().filter(|h| h.rsvp_at.is_some()).count();
    let summary = json!({"households": hs.len(), "replied": replied, "events": events});
    if as_json {
        print(&summary);
    } else {
        println!("{replied}/{} households replied", hs.len());
        for e in &events {
            println!("  {:<14} yes {:>3}  no {:>3}  waiting {:>3}  meals {}", e["event"].as_str().unwrap(), e["yes"], e["no"], e["waiting"], e["meals"]);
        }
    }
    Ok(())
}

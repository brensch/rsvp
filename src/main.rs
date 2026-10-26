mod address;
mod assets;
mod backup;
mod cli;
mod config;
mod db;
mod email;
mod gmail;
mod journal;
mod notify;
mod prose;
mod rsvp;
mod site;
mod store;
mod tokens;
mod web;

use std::sync::Arc;

use anyhow::Result;
use clap::Parser;

use cli::{Cli, Cmd};

/// If RSVP_SECRET etc. aren't in the environment (e.g. running the CLI over SSH),
/// read them from the same env file systemd uses.
fn load_env_file() {
    if std::env::var_os("RSVP_SECRET").is_some() {
        return;
    }
    let path = std::env::var("RSVP_ENV_FILE").unwrap_or_else(|_| "/etc/rsvp/env".into());
    let Ok(raw) = std::fs::read_to_string(path) else { return };
    for line in raw.lines() {
        let line = line.trim();
        if line.starts_with('#') {
            continue;
        }
        if let Some((k, v)) = line.split_once('=') {
            let (k, v) = (k.trim(), v.trim().trim_matches('"'));
            if std::env::var_os(k).is_none() {
                // SAFETY: called at startup before any threads are spawned.
                unsafe { std::env::set_var(k, v) };
            }
        }
    }
}

fn main() -> Result<()> {
    load_env_file();
    let cli = Cli::parse();
    let json_logs = std::env::var("RSVP_LOG_JSON").is_ok();
    let filter = tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info,tower_http=warn".into());
    if json_logs {
        tracing_subscriber::fmt().with_env_filter(filter).json().with_writer(std::io::stderr).init();
    } else {
        tracing_subscriber::fmt().with_env_filter(filter).with_writer(std::io::stderr).init();
    }

    match cli.cmd {
        Cmd::Serve => tokio::runtime::Runtime::new()?.block_on(serve(cli)),
        Cmd::Backup => tokio::runtime::Runtime::new()?.block_on(async {
            let cfg = config::Config::load(&cli.config)?;
            let env = config::Env::from_env(cli.data.clone())?;
            let db = db::Db::open(&env.db_path())?;
            backup::run(&db, &cfg, &env).await?;
            println!("backup ok");
            Ok(())
        }),
        _ => cli::run_sync(cli),
    }
}

async fn serve(cli: Cli) -> Result<()> {
    let cfg = config::Config::load(&cli.config)?;
    let env = config::Env::from_env(cli.data.clone())?;
    let db = db::Db::open(&env.db_path())?;

    // Refuse to start if any accepted RSVP is missing from the database.
    let journal = journal::Journal::new(env.journal_path());
    let missing = db.with(|c| rsvp::journal_missing(c, &journal))?;
    if !missing.is_empty() {
        tracing::warn!(count = missing.len(), "journal has entries missing from the database; replaying");
        db.with(|c| {
            for e in &missing {
                rsvp::apply(c, &cfg, e, true)?;
            }
            Ok(())
        })?;
    }

    db.with(|c| site::seed(c, &cfg))?;
    let renamed = db.with(|c| store::refresh_all_names(c))?;
    if renamed > 0 {
        tracing::info!(renamed, "household names re-derived from their people");
    }
    let state = web::AppState::new(cfg, env, db)?;
    let http = reqwest::Client::builder().timeout(std::time::Duration::from_secs(30)).build()?;
    tokio::spawn(notify::run(notify::Worker {
        db: state.db.clone(),
        cfg: state.cfg.clone(),
        env: state.env.clone(),
        renderer: state.renderer.clone(),
        http,
        gmail: Arc::new(gmail::Sender::default()),
        last_email: Default::default(),
    }));
    tokio::spawn(notify::unanswered_invitations(state.db.clone(), state.cfg.clone()));
    tokio::spawn(backup::scheduler(state.db.clone(), state.cfg.clone(), Arc::clone(&state.env)));
    web::serve(state).await
}

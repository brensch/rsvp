//! Daily snapshots: `VACUUM INTO` a consistent copy, verify it, gzip it with
//! the RSVP journal, and upload both to a retention-locked GCS bucket.
//! (Continuous replication is Litestream's job; this is the belt to its braces.)

use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use serde_json::json;

use crate::config::{Config, Env};
use crate::db::Db;
use crate::store;

pub async fn run(db: &Db, cfg: &Config, env: &Env) -> Result<()> {
    let result = run_inner(db, cfg, env).await;
    let status = match &result {
        Ok(objects) => format!("ok · {}", objects.join(", ")),
        Err(e) => format!("FAILED: {e:#}"),
    };
    let failed = result.is_err();
    let base = cfg.site.base_url.clone();
    db.call(move |c| {
        store::set_setting(c, "system", "last_backup_status", &status)?;
        if failed {
            store::enqueue(c, "discord", &json!({"type": "backup", "text": format!("🚨 Daily backup failed: {status}\n{base}/admin/settings")}))?;
        } else {
            store::set_setting(c, "system", "last_backup_at", &crate::db::now())?;
        }
        Ok(())
    })
    .await?;
    result.map(|_| ())
}

async fn run_inner(db: &Db, cfg: &Config, env: &Env) -> Result<Vec<String>> {
    let dir = env.backup_dir();
    std::fs::create_dir_all(&dir)?;
    let stamp = time::OffsetDateTime::now_utc().format(time::macros::format_description!("[year][month][day]T[hour][minute][second]Z"))?;
    let raw = dir.join(format!("rsvp-{stamp}.db"));
    {
        let raw = raw.clone();
        db.call(move |c| {
            c.execute("VACUUM INTO ?1", [raw.to_string_lossy()])?;
            Ok(())
        })
        .await?;
    }
    // Verify the copy before trusting it.
    {
        let check = rusqlite::Connection::open_with_flags(&raw, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)?;
        let ok: String = check.query_row("PRAGMA integrity_check", [], |r| r.get(0))?;
        if ok != "ok" {
            bail!("snapshot integrity check failed: {ok}");
        }
    }
    let db_gz = gzip(&raw, &dir.join(format!("rsvp-{stamp}.db.gz")))?;
    std::fs::remove_file(&raw)?;
    let mut files = vec![db_gz];
    if env.journal_path().exists() {
        files.push(gzip(&env.journal_path(), &dir.join(format!("journal-{stamp}.jsonl.gz")))?);
    }
    let mut uploaded = vec![];
    if let Some(bucket) = cfg.backup.bucket.as_deref().filter(|b| !b.is_empty()) {
        let http = reqwest::Client::builder().timeout(Duration::from_secs(120)).build()?;
        let token = gce_token(&http).await.context("getting GCE service-account token")?;
        let day = &stamp[..8];
        for f in &files {
            let name = format!("snapshots/{}/{}", day, f.file_name().unwrap().to_string_lossy());
            upload(&http, &token, bucket, &name, f).await?;
            uploaded.push(format!("gs://{bucket}/{name}"));
        }
    } else {
        uploaded.extend(files.iter().map(|f| f.display().to_string()));
    }
    prune(&dir, cfg.backup.keep_local)?;
    tracing::info!(?uploaded, "backup complete");
    Ok(uploaded)
}

fn gzip(src: &Path, dst: &Path) -> Result<PathBuf> {
    let data = std::fs::read(src)?;
    let f = std::fs::File::create(dst)?;
    let mut enc = flate2::write::GzEncoder::new(f, flate2::Compression::best());
    enc.write_all(&data)?;
    enc.finish()?.sync_all()?;
    Ok(dst.to_path_buf())
}

fn prune(dir: &Path, keep: usize) -> Result<()> {
    for prefix in ["rsvp-", "journal-"] {
        let mut files: Vec<PathBuf> = std::fs::read_dir(dir)?
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.file_name().is_some_and(|n| n.to_string_lossy().starts_with(prefix) && n.to_string_lossy().ends_with(".gz")))
            .collect();
        files.sort();
        let excess = files.len().saturating_sub(keep);
        for f in files.into_iter().take(excess) {
            std::fs::remove_file(f)?;
        }
    }
    Ok(())
}

/// Access token for the VM's service account from the metadata server.
async fn gce_token(http: &reqwest::Client) -> Result<String> {
    let v: serde_json::Value = http
        .get("http://metadata.google.internal/computeMetadata/v1/instance/service-accounts/default/token")
        .header("Metadata-Flavor", "Google")
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    v["access_token"].as_str().map(String::from).context("no access_token")
}

async fn upload(http: &reqwest::Client, token: &str, bucket: &str, name: &str, file: &Path) -> Result<()> {
    let body = tokio::fs::read(file).await?;
    let resp = http
        .post(format!("https://storage.googleapis.com/upload/storage/v1/b/{bucket}/o"))
        // ifGenerationMatch=0: never overwrite an existing object.
        .query(&[("uploadType", "media"), ("name", name), ("ifGenerationMatch", "0")])
        .bearer_auth(token)
        .header("content-type", "application/gzip")
        .body(body)
        .send()
        .await?;
    if !resp.status().is_success() {
        bail!("upload {name}: {} {}", resp.status(), resp.text().await.unwrap_or_default().chars().take(300).collect::<String>());
    }
    Ok(())
}

/// Take one snapshot per UTC day, at or after the configured hour.
pub async fn scheduler(db: Db, cfg: std::sync::Arc<Config>, env: std::sync::Arc<Env>) {
    loop {
        let now = time::OffsetDateTime::now_utc();
        let today = now.date().to_string();
        let last = db.call(|c| store::setting(c, "last_backup_at")).await.ok().flatten().unwrap_or_default();
        if now.hour() >= cfg.backup.hour_utc && !last.starts_with(&today) {
            if let Err(e) = run(&db, &cfg, &env).await {
                tracing::error!(error = ?e, "scheduled backup failed");
                // Avoid hammering: wait an hour before retrying a failed day.
                tokio::time::sleep(Duration::from_secs(3600)).await;
                continue;
            }
        }
        tokio::time::sleep(Duration::from_secs(600)).await;
    }
}

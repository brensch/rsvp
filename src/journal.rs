//! Append-only, fsync'd JSON Lines journal of every RSVP submission.
//!
//! Written *before* the database transaction. If SQLite ever failed or was
//! corrupted, `rsvp journal replay` rebuilds every response from this file.

use std::fs::OpenOptions;
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Entry {
    pub id: String,
    /// "" (or "rsvp") for RSVP forms, "address" for mailing-address submissions.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub kind: String,
    pub household_id: i64,
    pub received_at: String,
    pub ip_hash: String,
    pub user_agent: String,
    pub payload: serde_json::Value,
}

#[derive(Clone)]
pub struct Journal {
    path: PathBuf,
    lock: Arc<Mutex<()>>,
}

impl Journal {
    pub fn new(path: PathBuf) -> Self {
        Self { path, lock: Arc::new(Mutex::new(())) }
    }

    pub fn append(&self, entry: &Entry) -> Result<()> {
        let mut line = serde_json::to_vec(entry)?;
        line.push(b'\n');
        let _g = self.lock.lock().unwrap_or_else(|p| p.into_inner());
        let mut f = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)
            .with_context(|| format!("opening journal {}", self.path.display()))?;
        f.write_all(&line)?;
        f.sync_all().context("fsync journal")?;
        Ok(())
    }

    pub fn read_all(&self) -> Result<Vec<Entry>> {
        if !self.path.exists() {
            return Ok(vec![]);
        }
        let f = std::fs::File::open(&self.path)?;
        let mut out = vec![];
        for (n, line) in BufReader::new(f).lines().enumerate() {
            let line = line?;
            if line.trim().is_empty() {
                continue;
            }
            match serde_json::from_str(&line) {
                Ok(e) => out.push(e),
                // A torn final line (crash mid-write) is skipped, never fatal.
                Err(e) => tracing::warn!(line = n + 1, error = %e, "unreadable journal line"),
            }
        }
        Ok(out)
    }
}

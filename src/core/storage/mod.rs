//! Pluggable persistence behind a single [`Storage`] trait.
//!
//! * [`csv::CsvStore`] — append-only CSV, human-inspectable, ideal for one-shot capture.
//! * [`sqlite::SqliteStore`] — `rusqlite` (bundled) in WAL mode, so the TUI and web server
//!   can read live while the logger writes. Default for long-term logging.

pub mod csv;
pub mod sqlite;

use crate::core::sample::Sample;
use anyhow::{Context, Result};
use std::path::PathBuf;

/// A persistence backend for battery samples.
pub trait Storage {
    /// Persist one sample.
    fn append(&mut self, s: &Sample) -> Result<()>;
    /// Read every sample, ordered by timestamp ascending.
    fn read_all(&self) -> Result<Vec<Sample>>;
    /// Read samples with `from <= ts <= to` (inclusive), ordered by timestamp.
    /// Public API used by tests and range-scoped consumers; SQLite overrides it
    /// with an indexed query.
    #[allow(dead_code)]
    fn read_range(&self, from: i64, to: i64) -> Result<Vec<Sample>> {
        Ok(self
            .read_all()?
            .into_iter()
            .filter(|s| s.ts >= from && s.ts <= to)
            .collect())
    }
}

/// Which backend to use.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Backend {
    Sqlite,
    Csv,
}

impl std::str::FromStr for Backend {
    type Err = anyhow::Error;
    fn from_str(s: &str) -> Result<Self> {
        match s.to_ascii_lowercase().as_str() {
            "sqlite" | "db" => Ok(Backend::Sqlite),
            "csv" => Ok(Backend::Csv),
            other => anyhow::bail!("unknown store backend {other:?} (use sqlite|csv)"),
        }
    }
}

/// `$XDG_DATA_HOME/battcurve` (or `~/.local/share/battcurve`), created if missing.
pub fn data_dir() -> Result<PathBuf> {
    let base = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .filter(|p| !p.as_os_str().is_empty())
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/share")))
        .context("neither XDG_DATA_HOME nor HOME is set")?;
    let dir = base.join("battcurve");
    std::fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
    Ok(dir)
}

pub fn default_csv_path() -> Result<PathBuf> {
    Ok(data_dir()?.join("samples.csv"))
}

pub fn default_db_path() -> Result<PathBuf> {
    Ok(data_dir()?.join("battcurve.db"))
}

/// Open the default store for a backend at its conventional path.
pub fn open(backend: Backend) -> Result<Box<dyn Storage>> {
    Ok(match backend {
        Backend::Csv => Box::new(csv::CsvStore::open(default_csv_path()?)?),
        Backend::Sqlite => Box::new(sqlite::SqliteStore::open(default_db_path()?)?),
    })
}

//! SQLite backend via `rusqlite` (bundled). Opened in WAL mode so a single
//! writer (the logger) and many readers (TUI, web server) coexist.

use super::Storage;
use crate::core::sample::{Sample, Status};
use anyhow::{Context, Result};
use rusqlite::Connection;
use std::path::Path;

pub struct SqliteStore {
    conn: Connection,
}

impl SqliteStore {
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).ok();
        }
        let conn = Connection::open(path)
            .with_context(|| format!("opening sqlite db {}", path.display()))?;
        // WAL: concurrent readers while the logger writes; NORMAL sync is plenty
        // durable for telemetry and far faster than FULL.
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS samples (
                ts                    INTEGER NOT NULL,
                status                TEXT    NOT NULL,
                capacity_pct          REAL    NOT NULL,
                voltage_v             REAL    NOT NULL,
                power_w               REAL    NOT NULL,
                energy_wh             REAL    NOT NULL,
                energy_full_wh        REAL    NOT NULL,
                energy_full_design_wh REAL    NOT NULL,
                cycle_count           INTEGER NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_samples_ts ON samples(ts);",
        )?;
        Ok(Self { conn })
    }

    fn row_to_sample(row: &rusqlite::Row) -> rusqlite::Result<Sample> {
        let status: String = row.get(1)?;
        Ok(Sample {
            ts: row.get(0)?,
            status: status.parse().unwrap_or(Status::Unknown),
            capacity_pct: row.get(2)?,
            voltage_v: row.get(3)?,
            power_w: row.get(4)?,
            energy_wh: row.get(5)?,
            energy_full_wh: row.get(6)?,
            energy_full_design_wh: row.get(7)?,
            cycle_count: row.get(8)?,
        })
    }
}

impl Storage for SqliteStore {
    fn append(&mut self, s: &Sample) -> Result<()> {
        self.conn.execute(
            "INSERT INTO samples
                (ts,status,capacity_pct,voltage_v,power_w,energy_wh,energy_full_wh,energy_full_design_wh,cycle_count)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9)",
            rusqlite::params![
                s.ts,
                s.status.as_str(),
                s.capacity_pct,
                s.voltage_v,
                s.power_w,
                s.energy_wh,
                s.energy_full_wh,
                s.energy_full_design_wh,
                s.cycle_count,
            ],
        )?;
        Ok(())
    }

    fn read_all(&self) -> Result<Vec<Sample>> {
        let mut stmt = self.conn.prepare("SELECT * FROM samples ORDER BY ts ASC")?;
        let rows = stmt.query_map([], Self::row_to_sample)?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    fn read_range(&self, from: i64, to: i64) -> Result<Vec<Sample>> {
        let mut stmt = self
            .conn
            .prepare("SELECT * FROM samples WHERE ts BETWEEN ?1 AND ?2 ORDER BY ts ASC")?;
        let rows = stmt.query_map([from, to], Self::row_to_sample)?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn sample(ts: i64) -> Sample {
        Sample {
            ts,
            status: Status::Discharging,
            capacity_pct: 70.0,
            voltage_v: 11.8,
            power_w: -6.8,
            energy_wh: 28.2,
            energy_full_wh: 40.3,
            energy_full_design_wh: 41.05,
            cycle_count: 22,
        }
    }

    #[test]
    fn round_trip_and_range() {
        let dir = tempdir().unwrap();
        let mut s = SqliteStore::open(dir.path().join("t.db")).unwrap();
        for ts in [3, 1, 2, 10] {
            s.append(&sample(ts)).unwrap();
        }
        let all = s.read_all().unwrap();
        assert_eq!(
            all.iter().map(|x| x.ts).collect::<Vec<_>>(),
            vec![1, 2, 3, 10]
        );
        assert_eq!(all[0], sample(1));
        let mid = s.read_range(2, 3).unwrap();
        assert_eq!(mid.iter().map(|x| x.ts).collect::<Vec<_>>(), vec![2, 3]);
    }

    #[test]
    fn opens_in_wal_mode() {
        let dir = tempdir().unwrap();
        let s = SqliteStore::open(dir.path().join("t.db")).unwrap();
        let mode: String = s
            .conn
            .query_row("PRAGMA journal_mode", [], |r| r.get(0))
            .unwrap();
        assert_eq!(mode.to_ascii_lowercase(), "wal");
    }

    #[test]
    fn concurrent_reader_sees_writes() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("t.db");
        let mut writer = SqliteStore::open(&path).unwrap();
        let reader = SqliteStore::open(&path).unwrap();
        writer.append(&sample(1)).unwrap();
        assert_eq!(
            reader.read_all().unwrap().len(),
            1,
            "reader must see writer's row"
        );
    }
}

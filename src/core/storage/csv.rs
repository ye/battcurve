//! Append-only CSV backend.

use super::Storage;
use crate::core::sample::Sample;
use anyhow::{Context, Result};
use std::fs::{File, OpenOptions};
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};

pub struct CsvStore {
    path: PathBuf,
}

impl CsvStore {
    /// Open (creating if needed). Writes a header row to brand-new files.
    pub fn open(path: impl Into<PathBuf>) -> Result<Self> {
        let path = path.into();
        let fresh = !path.exists() || std::fs::metadata(&path).map(|m| m.len() == 0).unwrap_or(true);
        if fresh {
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent).ok();
            }
            let mut w = BufWriter::new(File::create(&path).with_context(|| format!("creating {}", path.display()))?);
            writeln!(
                w,
                "ts,status,capacity_pct,voltage_v,power_w,energy_wh,energy_full_wh,energy_full_design_wh,cycle_count"
            )?;
        }
        Ok(Self { path })
    }
}

impl Storage for CsvStore {
    fn append(&mut self, s: &Sample) -> Result<()> {
        let mut w = BufWriter::new(
            OpenOptions::new()
                .append(true)
                .open(&self.path)
                .with_context(|| format!("opening {} for append", self.path.display()))?,
        );
        writeln!(
            w,
            "{},{},{},{},{},{},{},{},{}",
            s.ts,
            s.status.as_str(),
            s.capacity_pct,
            s.voltage_v,
            s.power_w,
            s.energy_wh,
            s.energy_full_wh,
            s.energy_full_design_wh,
            s.cycle_count
        )?;
        w.flush()?;
        Ok(())
    }

    fn read_all(&self) -> Result<Vec<Sample>> {
        read_csv(&self.path)
    }
}

fn read_csv(path: &Path) -> Result<Vec<Sample>> {
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(vec![]),
        Err(e) => return Err(e).with_context(|| format!("reading {}", path.display())),
    };
    let mut out = Vec::new();
    for line in text.lines().skip(1) {
        if line.trim().is_empty() {
            continue;
        }
        let f: Vec<&str> = line.split(',').collect();
        if f.len() < 9 {
            continue; // tolerate a partially written final line
        }
        out.push(Sample {
            ts: f[0].parse()?,
            status: f[1].parse().unwrap_or(crate::core::sample::Status::Unknown),
            capacity_pct: f[2].parse()?,
            voltage_v: f[3].parse()?,
            power_w: f[4].parse()?,
            energy_wh: f[5].parse()?,
            energy_full_wh: f[6].parse()?,
            energy_full_design_wh: f[7].parse()?,
            cycle_count: f[8].parse()?,
        });
    }
    out.sort_by_key(|s| s.ts);
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::sample::Status;
    use tempfile::tempdir;

    fn sample(ts: i64) -> Sample {
        Sample {
            ts,
            status: Status::Charging,
            capacity_pct: 55.0,
            voltage_v: 12.0,
            power_w: 10.0,
            energy_wh: 24.0,
            energy_full_wh: 40.0,
            energy_full_design_wh: 41.0,
            cycle_count: 22,
        }
    }

    #[test]
    fn round_trips_and_sorts() {
        let dir = tempdir().unwrap();
        let mut s = CsvStore::open(dir.path().join("s.csv")).unwrap();
        s.append(&sample(20)).unwrap();
        s.append(&sample(10)).unwrap();
        let got = s.read_all().unwrap();
        assert_eq!(got.len(), 2);
        assert_eq!(got[0].ts, 10, "read_all must sort by ts");
        assert_eq!(got[1], sample(20));
    }

    #[test]
    fn read_range_filters() {
        let dir = tempdir().unwrap();
        let mut s = CsvStore::open(dir.path().join("s.csv")).unwrap();
        for ts in [1, 5, 9, 15] {
            s.append(&sample(ts)).unwrap();
        }
        let got = s.read_range(5, 9).unwrap();
        assert_eq!(got.iter().map(|x| x.ts).collect::<Vec<_>>(), vec![5, 9]);
    }
}

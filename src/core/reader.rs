//! Read a normalized [`Sample`] from `/sys/class/power_supply`.
//!
//! Two reporting styles exist in the wild and we normalize both:
//! * **energy-based** (e.g. this HP laptop): `energy_now`/`energy_full` in µWh,
//!   `power_now` in µW, `voltage_now` in µV.
//! * **charge-based**: `charge_now`/`charge_full` in µAh, `current_now` in µA;
//!   energy and power are derived as `charge * voltage` and `current * voltage`.

use crate::core::sample::{Sample, Status};
use anyhow::{anyhow, Context, Result};
use chrono::Utc;
use std::fs;
use std::path::{Path, PathBuf};

const SYSFS: &str = "/sys/class/power_supply";

/// Locate a battery directory: the requested name, or the first `BAT*` found.
pub fn find_battery(requested: Option<&str>) -> Result<PathBuf> {
    if let Some(name) = requested {
        let p = Path::new(SYSFS).join(name);
        return if p.is_dir() {
            Ok(p)
        } else {
            Err(anyhow!("battery {name:?} not found under {SYSFS}"))
        };
    }
    let mut found: Vec<PathBuf> = fs::read_dir(SYSFS)
        .with_context(|| format!("reading {SYSFS}"))?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .map(|n| n.starts_with("BAT"))
                .unwrap_or(false)
        })
        .collect();
    found.sort();
    found
        .into_iter()
        .next()
        .ok_or_else(|| anyhow!("no BAT* battery found under {SYSFS}"))
}

fn read_str(base: &Path, key: &str) -> Option<String> {
    fs::read_to_string(base.join(key))
        .ok()
        .map(|s| s.trim().to_string())
}

fn read_f64(base: &Path, key: &str) -> Option<f64> {
    read_str(base, key).and_then(|s| s.parse::<f64>().ok())
}

/// Read and normalize one sample from the given battery directory.
///
/// Taking the directory as a parameter (rather than hard-coding the path) keeps
/// this testable against fixture directories — see the tests below.
pub fn read_sample(base: &Path) -> Result<Sample> {
    let status: Status = read_str(base, "status")
        .as_deref()
        .unwrap_or("Unknown")
        .parse()
        .unwrap_or(Status::Unknown);

    let voltage_v = read_f64(base, "voltage_now")
        .map(|uv| uv / 1e6)
        .context("missing voltage_now")?;

    // Energy fields: native (µWh) or derived from charge (µAh) * voltage.
    let (energy_wh, energy_full_wh, energy_full_design_wh) =
        if let Some(now) = read_f64(base, "energy_now") {
            (
                now / 1e6,
                read_f64(base, "energy_full").unwrap_or(0.0) / 1e6,
                read_f64(base, "energy_full_design").unwrap_or(0.0) / 1e6,
            )
        } else if let Some(charge_now) = read_f64(base, "charge_now") {
            // Wh = (µAh / 1e6) * (µV / 1e6) = µAh * V / 1e6
            let to_wh = |uah: f64| uah * voltage_v / 1e6;
            (
                to_wh(charge_now),
                to_wh(read_f64(base, "charge_full").unwrap_or(0.0)),
                to_wh(read_f64(base, "charge_full_design").unwrap_or(0.0)),
            )
        } else {
            return Err(anyhow!("battery exposes neither energy_now nor charge_now"));
        };

    // Power: native power_now (µW) or current_now (µA) * voltage.
    let power_mag_w = if let Some(uw) = read_f64(base, "power_now") {
        uw / 1e6
    } else if let Some(ua) = read_f64(base, "current_now") {
        (ua / 1e6) * voltage_v
    } else {
        0.0
    };
    // Sign by direction: charging adds energy (+), discharging removes it (-).
    let power_w = match status {
        Status::Discharging => -power_mag_w,
        _ => power_mag_w,
    };

    let capacity_pct = read_f64(base, "capacity").unwrap_or_else(|| {
        if energy_full_wh > 0.0 {
            energy_wh / energy_full_wh * 100.0
        } else {
            f64::NAN
        }
    });

    Ok(Sample {
        ts: Utc::now().timestamp(),
        status,
        capacity_pct,
        voltage_v,
        power_w,
        energy_wh,
        energy_full_wh,
        energy_full_design_wh,
        cycle_count: read_f64(base, "cycle_count").unwrap_or(0.0) as u32,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    fn write_battery(files: &[(&str, &str)]) -> tempfile::TempDir {
        let dir = tempdir().unwrap();
        for (k, v) in files {
            fs::write(dir.path().join(k), v).unwrap();
        }
        dir
    }

    #[test]
    fn reads_energy_based_battery() {
        // Mirrors this laptop's BAT0 while discharging.
        let dir = write_battery(&[
            ("status", "Discharging\n"),
            ("voltage_now", "11825000"),
            ("energy_now", "28202000"),
            ("energy_full", "40325000"),
            ("energy_full_design", "41050000"),
            ("power_now", "6834000"),
            ("capacity", "70"),
            ("cycle_count", "22"),
        ]);
        let s = read_sample(dir.path()).unwrap();
        assert_eq!(s.status, Status::Discharging);
        assert!((s.voltage_v - 11.825).abs() < 1e-6);
        assert!((s.energy_wh - 28.202).abs() < 1e-6);
        assert!(
            (s.power_w - (-6.834)).abs() < 1e-6,
            "power must be negative while discharging"
        );
        assert_eq!(s.capacity_pct, 70.0);
        assert_eq!(s.cycle_count, 22);
    }

    #[test]
    fn reads_charge_based_battery() {
        // µAh + µA reporting; energy/power derived via voltage.
        let dir = write_battery(&[
            ("status", "Charging\n"),
            ("voltage_now", "12000000"), // 12 V
            ("charge_now", "2000000"),   // 2.0 Ah -> 24 Wh
            ("charge_full", "4000000"),  // 4.0 Ah -> 48 Wh
            ("charge_full_design", "5000000"),
            ("current_now", "1000000"), // 1.0 A -> 12 W
            ("capacity", "50"),
        ]);
        let s = read_sample(dir.path()).unwrap();
        assert_eq!(s.status, Status::Charging);
        assert!((s.energy_wh - 24.0).abs() < 1e-6);
        assert!((s.energy_full_wh - 48.0).abs() < 1e-6);
        assert!(
            (s.power_w - 12.0).abs() < 1e-6,
            "power must be positive while charging"
        );
    }

    #[test]
    fn capacity_falls_back_to_energy_ratio() {
        let dir = write_battery(&[
            ("status", "Full"),
            ("voltage_now", "12000000"),
            ("energy_now", "5000000"),
            ("energy_full", "10000000"),
            ("energy_full_design", "10000000"),
        ]);
        let s = read_sample(dir.path()).unwrap();
        assert!((s.capacity_pct - 50.0).abs() < 1e-6);
    }
}

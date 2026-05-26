//! The normalized battery measurement that every backend reads and writes.

use serde::{Deserialize, Serialize};
use std::fmt;
use std::str::FromStr;

/// Battery charging state, normalized across sysfs / upower spellings.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Status {
    Charging,
    Discharging,
    Full,
    NotCharging,
    Unknown,
}

impl Status {
    pub fn as_str(self) -> &'static str {
        match self {
            Status::Charging => "Charging",
            Status::Discharging => "Discharging",
            Status::Full => "Full",
            Status::NotCharging => "Not charging",
            Status::Unknown => "Unknown",
        }
    }
}

impl fmt::Display for Status {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for Status {
    type Err = std::convert::Infallible;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        // Accepts sysfs ("Charging") and upower ("charging") spellings.
        Ok(match s.trim().to_ascii_lowercase().as_str() {
            "charging" => Status::Charging,
            "discharging" => Status::Discharging,
            "full" => Status::Full,
            "not charging" | "notcharging" => Status::NotCharging,
            _ => Status::Unknown,
        })
    }
}

/// One normalized sample in SI-ish units.
///
/// `power_w` is **signed**: positive while charging (energy flowing in),
/// negative while discharging. All energy fields are in watt-hours.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Sample {
    /// Unix epoch seconds.
    pub ts: i64,
    pub status: Status,
    /// State of charge, percent (0..=100).
    pub capacity_pct: f64,
    pub voltage_v: f64,
    /// Signed instantaneous power, watts (+charging / -discharging).
    pub power_w: f64,
    pub energy_wh: f64,
    pub energy_full_wh: f64,
    pub energy_full_design_wh: f64,
    pub cycle_count: u32,
}

impl Sample {
    /// Instantaneous current in amperes, derived as `power / voltage`.
    /// Signed the same way as `power_w`. Returns 0.0 if voltage is unusable.
    pub fn current_a(&self) -> f64 {
        if self.voltage_v > 0.1 {
            self.power_w / self.voltage_v
        } else {
            0.0
        }
    }

    /// Battery health as a percentage of design capacity.
    pub fn health_pct(&self) -> f64 {
        if self.energy_full_design_wh > 0.0 {
            self.energy_full_wh / self.energy_full_design_wh * 100.0
        } else {
            f64::NAN
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(voltage_v: f64, power_w: f64) -> Sample {
        Sample {
            ts: 0,
            status: Status::Discharging,
            capacity_pct: 70.0,
            voltage_v,
            power_w,
            energy_wh: 28.2,
            energy_full_wh: 40.3,
            energy_full_design_wh: 41.05,
            cycle_count: 22,
        }
    }

    #[test]
    fn current_is_power_over_voltage() {
        let sample = s(11.8, -6.83);
        assert!((sample.current_a() - (-6.83 / 11.8)).abs() < 1e-9);
    }

    #[test]
    fn current_guards_against_zero_voltage() {
        assert_eq!(s(0.0, -6.83).current_a(), 0.0);
    }

    #[test]
    fn health_matches_real_machine() {
        // 40.3 / 41.05 ~= 98.17%
        assert!((s(11.8, 0.0).health_pct() - 98.1729).abs() < 0.01);
    }

    #[test]
    fn status_parses_both_spellings() {
        assert_eq!(
            "Discharging".parse::<Status>().unwrap(),
            Status::Discharging
        );
        assert_eq!("charging".parse::<Status>().unwrap(), Status::Charging);
        assert_eq!(
            "not charging".parse::<Status>().unwrap(),
            Status::NotCharging
        );
        assert_eq!("weird".parse::<Status>().unwrap(), Status::Unknown);
    }
}

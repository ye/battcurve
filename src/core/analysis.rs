//! Pure analysis over a series of [`Sample`]s — the testable heart of the tool.
//!
//! Everything here is deterministic and hardware-free so it can be unit tested
//! with synthetic curves.

use crate::core::sample::{Sample, Status};
use serde::Serialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum SessionKind {
    Charge,
    Discharge,
}

/// A contiguous run of samples all charging or all discharging.
#[derive(Debug, Clone, Serialize)]
pub struct Session {
    pub id: usize,
    pub kind: SessionKind,
    pub start_ts: i64,
    pub end_ts: i64,
    pub start_soc: f64,
    pub end_soc: f64,
    pub samples: Vec<Sample>,
}

impl Session {
    pub fn duration_secs(&self) -> i64 {
        self.end_ts - self.start_ts
    }
}

/// Split a time-ordered series into charge/discharge sessions.
///
/// Samples whose status is neither charging nor discharging (Full, NotCharging,
/// Unknown) act as boundaries: they close any open session. Sessions shorter
/// than two samples are dropped as noise.
pub fn segment_sessions(samples: &[Sample]) -> Vec<Session> {
    let mut sessions = Vec::new();
    let mut cur: Vec<Sample> = Vec::new();
    let mut cur_kind: Option<SessionKind> = None;

    let flush = |sessions: &mut Vec<Session>, kind: Option<SessionKind>, buf: &mut Vec<Sample>| {
        if let Some(kind) = kind {
            if buf.len() >= 2 {
                let id = sessions.len();
                sessions.push(Session {
                    id,
                    kind,
                    start_ts: buf.first().unwrap().ts,
                    end_ts: buf.last().unwrap().ts,
                    start_soc: buf.first().unwrap().capacity_pct,
                    end_soc: buf.last().unwrap().capacity_pct,
                    samples: std::mem::take(buf),
                });
            }
        }
        buf.clear();
    };

    for s in samples {
        let kind = match s.status {
            Status::Charging => Some(SessionKind::Charge),
            Status::Discharging => Some(SessionKind::Discharge),
            _ => None,
        };
        match kind {
            None => flush(&mut sessions, cur_kind.take(), &mut cur),
            Some(k) => {
                if cur_kind != Some(k) {
                    flush(&mut sessions, cur_kind.take(), &mut cur);
                    cur_kind = Some(k);
                }
                cur.push(s.clone());
            }
        }
    }
    flush(&mut sessions, cur_kind.take(), &mut cur);
    sessions
}

/// Fill in `power_w` from energy deltas wherever a sample reports ~zero power.
///
/// Some batteries (e.g. this HP) expose `power_now` only intermittently — or
/// not at all — yet update `energy_now` reliably every sample. Since our power
/// convention is "+ when charging, − when discharging", the signed energy
/// difference over time *is* the power: `ΔEnergy / Δt`. Samples that already
/// carry a real power reading are left untouched. The first sample has no
/// predecessor and is left as-is.
pub fn fill_derived_power(samples: &mut [Sample]) {
    // Average the energy slope over a trailing window. Long enough that a coarse,
    // quantized counter accumulates several steps (so a 12 mWh quantum doesn't
    // spike one tick to tens of watts), short enough to stay responsive at the
    // realistic 10–30 s logging cadence.
    const WINDOW_SECS: i64 = 60;
    for i in 1..samples.len() {
        if samples[i].power_w.abs() >= 0.01 {
            continue;
        }
        let mut j = i;
        while j > 0 && samples[i].ts - samples[j - 1].ts <= WINDOW_SECS {
            j -= 1;
        }
        let dt_h = (samples[i].ts - samples[j].ts) as f64 / 3600.0;
        if dt_h > 0.0 {
            samples[i].power_w = (samples[i].energy_wh - samples[j].energy_wh) / dt_h;
        }
    }
}

/// Cumulative charge moved (amp-hours) at each sample, by trapezoidal
/// integration of |current| over time. Always non-decreasing.
pub fn cumulative_charge_ah(s: &Session) -> Vec<f64> {
    let mut q = Vec::with_capacity(s.samples.len());
    let mut acc = 0.0;
    for (i, sample) in s.samples.iter().enumerate() {
        if i > 0 {
            let prev = &s.samples[i - 1];
            let dt_h = (sample.ts - prev.ts).max(0) as f64 / 3600.0;
            let i_avg = (sample.current_a().abs() + prev.current_a().abs()) / 2.0;
            acc += i_avg * dt_h;
        }
        q.push(acc);
    }
    q
}

/// A point on a differential-capacity curve.
#[derive(Debug, Clone, Copy, Serialize)]
pub struct DqDvPoint {
    pub voltage_v: f64,
    pub dqdv: f64,
}

/// Differential capacity dQ/dV for a session.
///
/// Pairs cumulative charge with voltage, orders by voltage, takes finite
/// differences, then applies a moving-average smoother (dQ/dV is intrinsically
/// noisy). `smooth_window` is clamped to an odd value >= 1.
pub fn dq_dv(s: &Session, smooth_window: usize) -> Vec<DqDvPoint> {
    let q = cumulative_charge_ah(s);
    let mut vq: Vec<(f64, f64)> = s
        .samples
        .iter()
        .zip(q.iter())
        .map(|(s, q)| (s.voltage_v, *q))
        .collect();
    vq.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));

    let mut raw: Vec<DqDvPoint> = Vec::new();
    for w in vq.windows(2) {
        let dv = w[1].0 - w[0].0;
        if dv.abs() < 1e-4 {
            continue; // avoid blow-up on flat voltage
        }
        raw.push(DqDvPoint {
            voltage_v: (w[0].0 + w[1].0) / 2.0,
            dqdv: (w[1].1 - w[0].1) / dv,
        });
    }
    smooth(&raw, smooth_window)
}

fn smooth(points: &[DqDvPoint], window: usize) -> Vec<DqDvPoint> {
    let w = window.max(1) | 1; // force odd
    if w == 1 || points.len() < w {
        return points.to_vec();
    }
    let half = w / 2;
    (0..points.len())
        .map(|i| {
            let lo = i.saturating_sub(half);
            let hi = (i + half + 1).min(points.len());
            let avg = points[lo..hi].iter().map(|p| p.dqdv).sum::<f64>() / (hi - lo) as f64;
            DqDvPoint {
                voltage_v: points[i].voltage_v,
                dqdv: avg,
            }
        })
        .collect()
}

/// Constant-current / constant-voltage split of a charge session.
#[derive(Debug, Clone, Serialize)]
pub struct CcCv {
    /// Index into `session.samples` where CV begins, if a CV phase was found.
    pub cv_start_index: Option<usize>,
    pub cc_avg_current_a: f64,
    pub cv_avg_current_a: f64,
}

/// Detect the CC→CV transition of a charge session.
///
/// Heuristic: once voltage reaches within 1% of the session's max and stays
/// there, the charger is holding voltage constant (CV) while current tapers.
pub fn detect_cc_cv(s: &Session) -> CcCv {
    let vmax = s
        .samples
        .iter()
        .map(|x| x.voltage_v)
        .fold(f64::MIN, f64::max);
    let threshold = vmax * 0.99;
    let cv_start_index = s.samples.iter().position(|x| x.voltage_v >= threshold);

    let avg = |slice: &[Sample]| -> f64 {
        if slice.is_empty() {
            0.0
        } else {
            slice.iter().map(|x| x.current_a().abs()).sum::<f64>() / slice.len() as f64
        }
    };
    let (cc, cv) = match cv_start_index {
        Some(i) => (avg(&s.samples[..i]), avg(&s.samples[i..])),
        None => (avg(&s.samples), 0.0),
    };
    CcCv {
        cv_start_index,
        cc_avg_current_a: cc,
        cv_avg_current_a: cv,
    }
}

/// Battery wear / health snapshot.
#[derive(Debug, Clone, Copy, Serialize)]
pub struct HealthSummary {
    pub health_pct: f64,
    pub wear_pct: f64,
    pub energy_full_wh: f64,
    pub energy_full_design_wh: f64,
    pub cycle_count: u32,
}

/// Health from the most recent sample. `None` if the series is empty.
pub fn health_summary(samples: &[Sample]) -> Option<HealthSummary> {
    let last = samples.last()?;
    let health = last.health_pct();
    Some(HealthSummary {
        health_pct: health,
        wear_pct: if health.is_finite() {
            100.0 - health
        } else {
            f64::NAN
        },
        energy_full_wh: last.energy_full_wh,
        energy_full_design_wh: last.energy_full_design_wh,
        cycle_count: last.cycle_count,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(ts: i64, status: Status, v: f64, power_w: f64, soc: f64) -> Sample {
        Sample {
            ts,
            status,
            capacity_pct: soc,
            voltage_v: v,
            power_w,
            energy_wh: 30.0,
            energy_full_wh: 40.0,
            energy_full_design_wh: 41.0,
            cycle_count: 22,
        }
    }

    #[test]
    fn derives_power_from_energy_when_missing() {
        // Mirrors this HP: power_w reads 0 but energy drains ~12 Wh/h.
        let mut s = vec![
            Sample {
                energy_wh: 24.834,
                ..sample(0, Status::Discharging, 11.45, 0.0, 60.0)
            },
            Sample {
                energy_wh: 24.823,
                ..sample(2, Status::Discharging, 11.45, 0.0, 60.0)
            },
            Sample {
                energy_wh: 24.811,
                ..sample(4, Status::Discharging, 11.45, 0.0, 60.0)
            },
        ];
        fill_derived_power(&mut s);
        assert_eq!(s[0].power_w, 0.0, "first sample has no predecessor");
        // (24.823-24.834)/(2/3600) = -19.8 W, negative => discharging.
        assert!(s[1].power_w < 0.0, "discharge power must be negative");
        assert!((s[1].power_w - (-19.8)).abs() < 0.2, "got {}", s[1].power_w);
    }

    #[test]
    fn derives_power_across_flat_quantized_energy() {
        // Energy holds flat for several 1s samples, then steps down 12 mWh.
        // The step must be amortized over the whole flat span, not spike on one tick.
        let mut s = vec![
            Sample {
                energy_wh: 24.551,
                ..sample(0, Status::Discharging, 11.4, 0.0, 61.0)
            },
            Sample {
                energy_wh: 24.551,
                ..sample(1, Status::Discharging, 11.4, 0.0, 61.0)
            },
            Sample {
                energy_wh: 24.551,
                ..sample(2, Status::Discharging, 11.4, 0.0, 61.0)
            },
            Sample {
                energy_wh: 24.539,
                ..sample(3, Status::Discharging, 11.4, 0.0, 61.0)
            },
        ];
        fill_derived_power(&mut s);
        // Step is over 3 s: (24.539-24.551)/(3/3600) = -14.4 W, not -43 W.
        assert!((s[3].power_w - (-14.4)).abs() < 0.3, "got {}", s[3].power_w);
        assert!(s[3].power_w > -20.0, "step must be amortized, not spiked");
    }

    #[test]
    fn fill_preserves_real_power_readings() {
        let mut s = vec![
            sample(0, Status::Charging, 12.0, 20.0, 30.0),
            sample(60, Status::Charging, 12.1, 18.0, 40.0),
        ];
        fill_derived_power(&mut s);
        assert_eq!(s[1].power_w, 18.0, "non-zero power must be left untouched");
    }

    #[test]
    fn segments_on_direction_change() {
        let s = vec![
            sample(0, Status::Discharging, 12.0, -10.0, 90.0),
            sample(60, Status::Discharging, 11.8, -10.0, 80.0),
            sample(120, Status::Full, 12.6, 0.0, 100.0), // boundary
            sample(180, Status::Charging, 12.0, 20.0, 20.0),
            sample(240, Status::Charging, 12.4, 20.0, 40.0),
        ];
        let sessions = segment_sessions(&s);
        assert_eq!(sessions.len(), 2);
        assert_eq!(sessions[0].kind, SessionKind::Discharge);
        assert_eq!(sessions[1].kind, SessionKind::Charge);
        assert_eq!(sessions[1].start_soc, 20.0);
        assert_eq!(sessions[1].end_soc, 40.0);
    }

    #[test]
    fn cumulative_charge_is_monotonic() {
        // 1 hour discharge at ~constant 1 A (12 W / 12 V).
        let s = vec![
            sample(0, Status::Discharging, 12.0, -12.0, 100.0),
            sample(1800, Status::Discharging, 12.0, -12.0, 75.0),
            sample(3600, Status::Discharging, 12.0, -12.0, 50.0),
        ];
        let sess = segment_sessions(&s).pop().unwrap();
        let q = cumulative_charge_ah(&sess);
        assert!(
            q.windows(2).all(|w| w[1] >= w[0]),
            "Q must be non-decreasing"
        );
        // ~1 A over 1 h ~= 1 Ah total.
        assert!((q.last().unwrap() - 1.0).abs() < 0.05, "got {:?}", q.last());
    }

    #[test]
    fn detects_cc_cv_boundary() {
        // Voltage rises (CC) then holds near 12.6 V (CV) while current drops.
        let s = vec![
            sample(0, Status::Charging, 11.0, 24.0, 10.0),
            sample(600, Status::Charging, 11.8, 24.0, 40.0),
            sample(1200, Status::Charging, 12.6, 24.0, 80.0), // hits Vmax -> CV
            sample(1800, Status::Charging, 12.6, 6.0, 95.0),
            sample(2400, Status::Charging, 12.6, 2.0, 99.0),
        ];
        let sess = segment_sessions(&s).pop().unwrap();
        let cccv = detect_cc_cv(&sess);
        assert_eq!(cccv.cv_start_index, Some(2));
        assert!(
            cccv.cc_avg_current_a > cccv.cv_avg_current_a,
            "current tapers in CV"
        );
    }

    #[test]
    fn dq_dv_is_smoothed_and_nonempty() {
        let s: Vec<Sample> = (0..20)
            .map(|i| {
                sample(
                    i * 60,
                    Status::Charging,
                    11.0 + i as f64 * 0.08,
                    24.0,
                    i as f64 * 5.0,
                )
            })
            .collect();
        let sess = segment_sessions(&s).pop().unwrap();
        let curve = dq_dv(&sess, 5);
        assert!(!curve.is_empty());
        assert!(curve.iter().all(|p| p.dqdv.is_finite()));
    }

    #[test]
    fn health_uses_latest_sample() {
        let s = vec![sample(0, Status::Full, 12.6, 0.0, 100.0)];
        let h = health_summary(&s).unwrap();
        assert!((h.health_pct - (40.0 / 41.0 * 100.0)).abs() < 1e-6);
        assert!((h.wear_pct - (100.0 - 40.0 / 41.0 * 100.0)).abs() < 1e-6);
        assert_eq!(h.cycle_count, 22);
    }
}

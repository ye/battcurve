//! One-shot session capture: sample live until a stop condition, then summarize.

use crate::core::analysis::{self, SessionKind};
use crate::core::reader;
use crate::core::sample::{Sample, Status};
use crate::core::storage::{self, Backend};
use anyhow::Result;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Until {
    CtrlC,
    Full,
    Empty,
}

impl std::str::FromStr for Until {
    type Err = anyhow::Error;
    fn from_str(s: &str) -> Result<Self> {
        Ok(match s.to_ascii_lowercase().as_str() {
            "ctrl-c" | "ctrlc" | "manual" => Until::CtrlC,
            "full" => Until::Full,
            "empty" => Until::Empty,
            other => anyhow::bail!("unknown --until {other:?} (use ctrl-c|full|empty)"),
        })
    }
}

fn stop_reached(until: Until, s: &Sample) -> bool {
    match until {
        Until::CtrlC => false,
        Until::Full => matches!(s.status, Status::Full) || s.capacity_pct >= 99.5,
        Until::Empty => s.capacity_pct <= 3.0,
    }
}

pub fn run(
    interval: Duration,
    backend: Backend,
    battery: Option<String>,
    until: Until,
) -> Result<()> {
    let base = reader::find_battery(battery.as_deref())?;
    let mut store = storage::open(backend)?;
    let running = crate::cmd::running_flag();

    eprintln!(
        "battcurve: capturing {} every {:.0}s until {:?}. Ctrl-C to stop early.",
        base.display(),
        interval.as_secs_f64(),
        until
    );

    let mut captured: Vec<Sample> = Vec::new();
    while running.load(Ordering::SeqCst) {
        let tick = Instant::now();
        if let Ok(s) = reader::read_sample(&base) {
            let _ = store.append(&s);
            eprintln!(
                "  {} SoC {:5.1}%  {:6.2} V  {:+6.2} W",
                s.status, s.capacity_pct, s.voltage_v, s.power_w
            );
            let done = stop_reached(until, &s);
            captured.push(s);
            if done {
                break;
            }
        }
        while running.load(Ordering::SeqCst) && tick.elapsed() < interval {
            std::thread::sleep(Duration::from_millis(200).min(interval));
        }
    }

    analysis::fill_derived_power(&mut captured);
    print_summary(&captured);
    eprintln!("\nbattcurve: run `battcurve serve` to view the charts.");
    Ok(())
}

fn print_summary(samples: &[Sample]) {
    println!("\n=== Capture summary ===");
    if samples.len() < 2 {
        println!("Not enough samples captured.");
        return;
    }
    let dur = samples.last().unwrap().ts - samples.first().unwrap().ts;
    println!(
        "Samples: {}   Duration: {}m {}s",
        samples.len(),
        dur / 60,
        dur % 60
    );

    for sess in analysis::segment_sessions(samples) {
        let avg_w =
            sess.samples.iter().map(|s| s.power_w.abs()).sum::<f64>() / sess.samples.len() as f64;
        print!(
            "{:?}: {:.1}% -> {:.1}% over {}m {}s, avg {:.2} W",
            sess.kind,
            sess.start_soc,
            sess.end_soc,
            sess.duration_secs() / 60,
            sess.duration_secs() % 60,
            avg_w
        );
        if sess.kind == SessionKind::Charge {
            let cc = analysis::detect_cc_cv(&sess);
            match cc.cv_start_index {
                Some(_) => print!(
                    "  [CC {:.2} A -> CV {:.2} A]",
                    cc.cc_avg_current_a, cc.cv_avg_current_a
                ),
                None => print!("  [CC only, {:.2} A]", cc.cc_avg_current_a),
            }
        }
        println!();
    }

    if let Some(h) = analysis::health_summary(samples) {
        println!(
            "Health: {:.1}% of design ({:.1}/{:.1} Wh), wear {:.1}%, {} cycles",
            h.health_pct, h.energy_full_wh, h.energy_full_design_wh, h.wear_pct, h.cycle_count
        );
    }
}

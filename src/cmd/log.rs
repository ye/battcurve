//! Background logger: sample the battery at a fixed interval into a store.

use crate::core::reader;
use crate::core::storage::{self, Backend};
use anyhow::Result;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

pub fn run(interval: Duration, backend: Backend, battery: Option<String>) -> Result<()> {
    let base = reader::find_battery(battery.as_deref())?;
    let mut store = storage::open(backend)?;
    let running = crate::cmd::running_flag();

    eprintln!(
        "battcurve: logging {} every {:.0}s ({:?}). Ctrl-C to stop.",
        base.display(),
        interval.as_secs_f64(),
        backend
    );

    while running.load(Ordering::SeqCst) {
        let tick = Instant::now();
        match reader::read_sample(&base) {
            Ok(s) => {
                if let Err(e) = store.append(&s) {
                    eprintln!("battcurve: append failed: {e:#}");
                }
            }
            Err(e) => eprintln!("battcurve: read failed: {e:#}"),
        }
        // Sleep the remainder of the interval in short slices so Ctrl-C is responsive.
        while running.load(Ordering::SeqCst) && tick.elapsed() < interval {
            std::thread::sleep(Duration::from_millis(200).min(interval));
        }
    }
    eprintln!("battcurve: logger stopped.");
    Ok(())
}

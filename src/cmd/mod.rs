//! Subcommand implementations.

pub mod capture;
pub mod log;
pub mod serve;
pub mod tui;

use anyhow::{bail, Result};
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use std::time::Duration;

/// Parse a human duration like `10s`, `5m`, `1h`, or a bare number (seconds).
pub fn parse_duration(s: &str) -> Result<Duration> {
    let s = s.trim();
    let (num, mult) = if let Some(v) = s.strip_suffix("ms") {
        (v, 0.001)
    } else if let Some(v) = s.strip_suffix('s') {
        (v, 1.0)
    } else if let Some(v) = s.strip_suffix('m') {
        (v, 60.0)
    } else if let Some(v) = s.strip_suffix('h') {
        (v, 3600.0)
    } else {
        (s, 1.0)
    };
    let n: f64 = num.parse().map_err(|_| anyhow::anyhow!("bad duration {s:?}"))?;
    if n <= 0.0 {
        bail!("duration must be positive, got {s:?}");
    }
    Ok(Duration::from_secs_f64(n * mult))
}

/// Install a Ctrl-C handler and return a flag that flips to `false` on SIGINT.
pub fn running_flag() -> Arc<AtomicBool> {
    let flag = Arc::new(AtomicBool::new(true));
    let f = flag.clone();
    let _ = ctrlc::set_handler(move || {
        f.store(false, std::sync::atomic::Ordering::SeqCst);
    });
    flag
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_units() {
        assert_eq!(parse_duration("10s").unwrap(), Duration::from_secs(10));
        assert_eq!(parse_duration("5m").unwrap(), Duration::from_secs(300));
        assert_eq!(parse_duration("1h").unwrap(), Duration::from_secs(3600));
        assert_eq!(parse_duration("250ms").unwrap(), Duration::from_millis(250));
        assert_eq!(parse_duration("30").unwrap(), Duration::from_secs(30));
    }

    #[test]
    fn rejects_garbage() {
        assert!(parse_duration("abc").is_err());
        assert!(parse_duration("0s").is_err());
        assert!(parse_duration("-5s").is_err());
    }
}

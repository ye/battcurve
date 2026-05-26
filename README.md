# battcurve

Sample your laptop battery and visualize its **charge / discharge curves**, **dQ/dV
differential-capacity curve**, and **health / wear** — the curve families from battery
literature ([Battery University](https://www.batteryuniversity.com/article/bu-501-basics-about-discharging/),
[BioLogic](https://www.biologic.net/topics/how-to-read-cycling-curves/)) — built from your
own machine's data.

A single Rust binary with a shared core (sampler + storage + analysis) and three faces:
a background **logger**, an htop-style **TUI**, and a local **web UI** with interactive charts.

## Quick start

```sh
just build                 # cargo build
just run-log 30s           # background logger -> SQLite (Ctrl-C to stop)
just run-tui               # live htop-style monitor (press q / Esc to quit)
just serve 8787            # open http://127.0.0.1:8787 for the analysis charts
just capture               # one-shot: capture a session, then print a summary
just paths                 # show where data is stored
```

The logger, TUI, and web UI all share the same store, so you can log in one terminal
and watch live in another. Run the logger across **at least one full charge or discharge
cycle** before expecting clean V-vs-SoC and dQ/dV curves.

## The charts

- **Charge / discharge over time** — SoC %, voltage, and power on a shared time axis.
- **Voltage vs State of Charge** — the classic discharge plateau / CC–CV charge climb.
- **dQ/dV (differential capacity)** — peaks mark electrochemical phase transitions; used
  to track aging. Smoothed with a moving average since it is intrinsically noisy.
- **Health summary** — full-charge capacity vs design capacity, wear %, and cycle count.

## How it reads the battery

Reads `/sys/class/power_supply/BAT*` (override with `--battery BATx`), normalizing both
reporting styles found in the wild:

- **energy-based** (`energy_now` / `energy_full` in µWh, `voltage_now` in µV) — what this
  HP laptop uses.
- **charge-based** (`charge_now` / `charge_full` in µAh, `current_now` in µA) — energy and
  power are derived via voltage.

### Deriving power when the EC won't tell us

Some batteries (including the HP this was developed on) expose `power_now` only
intermittently — or report `No such device` — and have no `current_now` at all. But
`energy_now` updates reliably. Since power is just the signed energy slope, `battcurve`
derives it from the energy delta over a short trailing window
(`analysis::fill_derived_power`) wherever a real power reading is missing. Native
`power_now` readings, when present, are used as-is. The derived power also feeds the
current used for dQ/dV integration (`I = power / voltage`).

## Storage

Default backend is **SQLite** (`rusqlite`, bundled, WAL mode) at
`$XDG_DATA_HOME/battcurve/battcurve.db`, so the logger can write while the TUI and web
server read concurrently. Pass `--store csv` to any command to use a human-readable,
append-only CSV at `samples.csv` instead. Stored samples are the raw sysfs readings;
power derivation happens at read time, keeping the log a faithful record.

## Background service

```sh
just install-service       # installs target/release binary + enables a systemd --user unit
systemctl --user status battcurve-logger
```

The unit (`systemd/battcurve-logger.service`) runs `battcurve log --interval 30s` and
restarts on failure.

## Layout

```
justfile       command runner (build / test / lint / run-* / serve / install-service)
src/core/      sample, reader (sysfs), storage (csv|sqlite), analysis (sessions, dQ/dV, CC/CV, health)
src/cmd/       log, capture, tui, serve
src/web/       index.html (uPlot charts)
systemd/       user logger unit
```

## Tests

```sh
just test      # unit tests: analysis curves, power derivation, storage round-trips,
               # WAL + concurrent reader, reader normalization (energy- and charge-based)
just lint      # clippy with -D warnings
```

## Notes

- The web UI refreshes by **polling** the JSON API every 5 s (no WebSocket).
- uPlot is loaded from a CDN, so the first page load needs internet access. Vendor
  `uPlot.iife.min.js` / `uPlot.min.css` into `src/web/` and adjust the `<script>`/`<link>`
  tags if you need fully offline operation.
- A clean dQ/dV or full discharge curve only appears after a real charge/discharge cycle
  has been logged.

# Battery Charge / Discharge Curve Visualizer (`battcurve`)

## Context

We want a Linux tool that periodically samples laptop battery metrics and visualizes
**charge curves**, **discharge curves**, and battery-health trends — the same families of
curves described in battery literature (Battery University, BioLogic):

- **Charge curve** — how voltage/power/capacity change as energy is delivered; on Li-ion
  this splits into a **Constant-Current (CC)** phase (capacity climbs fast) and a
  **Constant-Voltage (CV)** phase (current/power taper as it tops off).
- **Discharge curve** — voltage vs time or vs State-of-Charge (SoC), showing the flat
  **voltage plateau** then a steep drop near empty.
- **dQ/dV differential-capacity curve** — peaks reveal electrochemical phase transitions
  and are used to track aging.
- **Health summary** — full-charge capacity vs design capacity + cycle count (wear %).

Existing Linux tooling (`upower`, GNOME stats) only shows the *current* state and a short,
coarse history. None plot real captured CC/CV or dQ/dV curves. This project fills that gap.

**Decisions locked in with the user:**
- Language: **Rust** (single binary, no runtime deps, robust as a long-running sampler).
- Operating modes: **both** a background logger (hours/days) and a one-shot session capture.
- Metrics: **all** — charge/discharge-over-time, V-vs-SoC, dQ/dV, health summary.
- UI: **TUI + web on a shared core** — a `ratatui` htop-style live monitor, plus a local
  `axum` web server rendering high-fidelity analysis charts (HTML5 canvas).

**Machine facts (this laptop, verified):** `BAT0` reports `energy_*` (µWh), `power_now`
(µW), `voltage_now` (µV), `cycle_count`, `energy_full`/`energy_full_design`. There is **no
`current_now`/`charge_now`**, so current is derived as `I = power_now / voltage_now`. The
core must also support charge-reporting batteries (`charge_now`/`current_now` in µAh/µA).

## Approach

A single Cargo binary `battcurve` with subcommands sharing one core library. Tasks are
driven by a **`justfile`** command runner. Storage is pluggable behind a `Storage` trait
with two backends: a simple **CSV** backend and an optional **SQLite** backend (default for
long-term logging) via `rusqlite` with the `bundled` feature — no system `sqlite3` needed.

### Project layout (all files new)

```
battcurve/
  justfile             # command runner: build/test/lint/run-* recipes
  Cargo.toml
  src/
    main.rs              # clap CLI dispatch
    core/
      mod.rs
      sample.rs          # Sample struct + units; status enum
      reader.rs          # sysfs reader (energy- AND charge-based), upower fallback
      storage/
        mod.rs           # `Storage` trait: append(sample), read_all(), sessions(), query
        csv.rs           # CSV backend (append-only, human-inspectable)
        sqlite.rs        # rusqlite backend (WAL mode) for robust concurrent R/W
      analysis.rs        # sessions, SoC, derived current, dQ/dV, CC/CV, health
    cmd/
      log.rs             # background logger (sample loop -> storage)
      capture.rs         # one-shot: sample until Ctrl-C/full/empty, then summarize
      tui.rs             # ratatui htop-style live monitor
      serve.rs           # axum server: REST JSON + WebSocket live feed
    web/
      index.html         # uPlot-based canvas charts (embedded via include_str!)
  systemd/
    battcurve-logger.service   # optional `systemctl --user` unit
  README.md
```

### Core library (`src/core`)

- **`sample.rs`** — `Sample { ts: i64, status: Status, capacity_pct: f64, voltage_v: f64,
  power_w: f64, energy_wh: f64, energy_full_wh: f64, energy_full_design_wh: f64,
  cycle_count: u32 }`. `Status` enum: `Charging | Discharging | Full | NotCharging | Unknown`.
- **`reader.rs`** — read `/sys/class/power_supply/BAT0/*`. Battery path is configurable
  (`--battery`, default first `BAT*`). Normalize **both** reporting styles:
  energy-based (this machine) and charge-based (`charge_now`×`voltage_now` → Wh,
  `current_now` → power). Fall back to parsing `upower -i` if sysfs is missing a field.
- **`storage/`** — a `Storage` trait (`append`, `read_all`, `sessions`, range queries) with
  two backends:
  - **`csv.rs`** — append-only CSV at `$XDG_DATA_HOME/battcurve/samples.csv`, header-stamped
    and human-inspectable. Good for one-shot `capture`.
  - **`sqlite.rs`** — `rusqlite` (`bundled` feature, no system dep) at
    `$XDG_DATA_HOME/battcurve/battcurve.db`, **WAL mode** so the TUI and web server can read
    live while the logger writes. `samples` table (indexed on `ts`, `session_id`); enables
    fast range/session queries for serving without loading the whole file. Default backend
    for `log`. Selectable via `--store sqlite|csv`.
- **`analysis.rs`** — pure functions, the testable heart:
  - `segment_sessions()` — split samples on `Status` transitions into charge/discharge runs.
  - `soc()` — use `capacity_pct` (fallback `energy/energy_full`).
  - `derive_current(sample)` — `power_w / voltage_v` (A) when no native current.
  - `dq_dv(session)` — accumulate `Q = Σ I·dt`, bin by voltage, finite-difference, then
    smooth (moving-average / Savitzky–Golay).
  - `detect_cc_cv(charge_session)` — CC = current near-flat & high while voltage rises;
    CV = voltage near-flat at max while current tapers.
  - `health(sample)` — `energy_full/energy_full_design × 100`, plus `cycle_count`.

### Subcommands

- **`battcurve log [--interval 10s]`** — background logger: sample → append CSV. Runs
  forever; meant to be wrapped by the systemd user unit. Idempotent appends.
- **`battcurve capture [--until full|empty|ctrl-c]`** — one-shot session: samples live,
  on exit prints a terminal summary (duration, ΔSoC, avg power, CC/CV split, health) and
  optionally points to `serve` for charts.
- **`battcurve tui`** — `ratatui` + `crossterm` htop-style live view: gauges (SoC, health),
  current status/power/voltage/time-to-full-or-empty, and sparkline/`Chart` widgets of the
  recent power & voltage trace. Reads existing CSV + samples live.
- **`battcurve serve [--port 8787]`** — `axum` + `tokio`: serves `index.html` and JSON
  endpoints (`/api/sessions`, `/api/session/{id}`, `/api/health`); a WebSocket pushes new
  samples for live chart updates. Frontend uses **uPlot** (tiny, fast canvas) to draw:
  charge/discharge over time (multi-axis: SoC, V, W), V-vs-SoC plateau, dQ/dV, health-over-time.

### Key dependencies (add to Cargo.toml)
`clap` (derive), `ratatui`, `crossterm`, `axum`, `tokio`, `serde`/`serde_json`, `csv`,
`rusqlite` (`bundled` feature), `time` or `chrono`, `anyhow`. uPlot shipped as a vendored
static asset (no npm build step). `just` is the command runner (install via `cargo install
just` if missing).

### Justfile recipes
`build`, `test`, `lint` (clippy), `fmt`, `run-log [interval]`, `run-tui`, `serve [port]`,
`capture`, `install-service` (systemd user unit), `db-path` / `csv-path` helpers.

## Verification

1. **Unit tests (`analysis.rs`)** — feed synthetic sessions (e.g. an ideal CC/CV charge and
   a plateau discharge) and assert: correct session segmentation, monotonic Q, dQ/dV peak at
   the expected voltage, correct CC/CV boundary, health math. Inject a fake reader (path or
   trait) so `reader.rs` is testable without real hardware.
2. **Storage tests** — round-trip `append`/`read_all`/`sessions` through **both** the CSV and
   SQLite backends and assert identical results; verify SQLite opens in WAL mode and a reader
   sees rows while a writer holds the DB.
3. **Reader smoke test** — run `just capture` against real `BAT0` for ~30s; confirm rows land
   in the store with sane values (voltage ~11–12V, power ~5–8W, SoC ~70%).
4. **End-to-end** — `just run-log 5s` for a few minutes while plugging/unplugging AC; then
   `just run-tui` shows live gauges and `just serve` renders the time, V-vs-SoC, dQ/dV, and
   health charts in the browser at `http://localhost:8787` (reading the SQLite store live).
5. **Cross-style guard** — unit-test `reader.rs` normalization against both a captured
   energy-based fixture (this laptop) and a synthetic charge-based fixture.
6. `just build`, `just test`, `just lint` (clippy) clean.

## Notes / open items
- A real, well-resolved dQ/dV or full discharge curve needs an actual full charge or full
  discharge cycle captured by the logger — the first useful charts appear after one cycle.
- systemd unit is optional convenience; the binary runs fine standalone.

//! htop-style live terminal monitor built on ratatui.

use crate::core::reader;
use crate::core::sample::{Sample, Status};
use crate::core::storage::{self, Backend};
use anyhow::Result;
use crossterm::{
    event::{self, Event, KeyCode, KeyEventKind},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{prelude::*, widgets::*};
use std::collections::VecDeque;
use std::io::{stdout, Stdout};
use std::time::{Duration, Instant};

type Tui = Terminal<CrosstermBackend<Stdout>>;

const HISTORY: usize = 240;

pub fn run(backend: Backend, battery: Option<String>) -> Result<()> {
    let base = reader::find_battery(battery.as_deref())?;

    // Seed the graphs with whatever the store already has.
    let mut history: VecDeque<Sample> = VecDeque::with_capacity(HISTORY);
    if let Ok(store) = storage::open(backend) {
        if let Ok(all) = store.read_all() {
            for s in all.into_iter().rev().take(HISTORY).rev() {
                history.push_back(s);
            }
        }
    }

    enable_raw_mode()?;
    let mut out = stdout();
    execute!(out, EnterAlternateScreen)?;
    let mut terminal = Terminal::new(CrosstermBackend::new(out))?;

    let res = event_loop(&mut terminal, &base, &mut history);

    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;
    res
}

fn event_loop(
    terminal: &mut Tui,
    base: &std::path::Path,
    history: &mut VecDeque<Sample>,
) -> Result<()> {
    let tick = Duration::from_secs(1);
    let mut last_sample = Instant::now() - tick; // sample immediately
    loop {
        if last_sample.elapsed() >= tick {
            if let Ok(s) = reader::read_sample(base) {
                if history.len() == HISTORY {
                    history.pop_front();
                }
                history.push_back(s);
            }
            last_sample = Instant::now();
        }

        terminal.draw(|f| draw(f, history))?;

        if event::poll(Duration::from_millis(200))? {
            if let Event::Key(k) = event::read()? {
                if k.kind == KeyEventKind::Press
                    && matches!(k.code, KeyCode::Char('q') | KeyCode::Esc)
                {
                    return Ok(());
                }
            }
        }
    }
}

fn draw(f: &mut Frame, history: &VecDeque<Sample>) {
    let area = f.area();
    let rows = Layout::vertical([
        Constraint::Length(3), // gauges
        Constraint::Length(8), // stats
        Constraint::Min(6),    // sparklines
    ])
    .split(area);

    // Derive power from energy deltas where the EC reports none (see analysis).
    let mut filled: Vec<Sample> = history.iter().cloned().collect();
    crate::core::analysis::fill_derived_power(&mut filled);
    let latest = filled.last();

    // --- Gauges: SoC + Health ---
    let g = Layout::horizontal([Constraint::Percentage(50), Constraint::Percentage(50)])
        .split(rows[0]);
    let soc = latest.map(|s| (s.capacity_pct / 100.0).clamp(0.0, 1.0)).unwrap_or(0.0);
    let health = latest.map(|s| (s.health_pct() / 100.0).clamp(0.0, 1.0)).unwrap_or(0.0);
    f.render_widget(
        Gauge::default()
            .block(Block::bordered().title(" State of Charge "))
            .gauge_style(Style::new().fg(Color::Green))
            .ratio(soc)
            .label(format!("{:.1}%", soc * 100.0)),
        g[0],
    );
    f.render_widget(
        Gauge::default()
            .block(Block::bordered().title(" Battery Health "))
            .gauge_style(Style::new().fg(Color::Cyan))
            .ratio(health)
            .label(format!("{:.1}%", health * 100.0)),
        g[1],
    );

    // --- Stats ---
    f.render_widget(stats_paragraph(latest, history.len()), rows[1]);

    // --- Sparklines: power + voltage ---
    let s = Layout::horizontal([Constraint::Percentage(50), Constraint::Percentage(50)])
        .split(rows[2]);
    let power: Vec<u64> = filled.iter().map(|x| (x.power_w.abs() * 100.0) as u64).collect();
    let vmin = filled.iter().map(|x| x.voltage_v).fold(f64::MAX, f64::min);
    let volt: Vec<u64> = filled
        .iter()
        .map(|x| ((x.voltage_v - vmin) * 1000.0).max(0.0) as u64)
        .collect();
    f.render_widget(
        Sparkline::default()
            .block(Block::bordered().title(" Power (W) "))
            .style(Style::new().fg(Color::Yellow))
            .data(&power),
        s[0],
    );
    f.render_widget(
        Sparkline::default()
            .block(Block::bordered().title(" Voltage (relative) "))
            .style(Style::new().fg(Color::Magenta))
            .data(&volt),
        s[1],
    );
}

fn stats_paragraph(latest: Option<&Sample>, n: usize) -> Paragraph<'static> {
    let lines = match latest {
        None => vec![Line::from("Waiting for first sample...")],
        Some(s) => {
            let eta = time_estimate(s);
            vec![
                Line::from(format!("Status:   {}", s.status)),
                Line::from(format!("Voltage:  {:.3} V", s.voltage_v)),
                Line::from(format!(
                    "Power:    {:+.2} W   ({:+.3} A)",
                    s.power_w,
                    s.current_a()
                )),
                Line::from(format!(
                    "Energy:   {:.1} / {:.1} Wh   (design {:.1})",
                    s.energy_wh, s.energy_full_wh, s.energy_full_design_wh
                )),
                Line::from(format!("Cycles:   {}", s.cycle_count)),
                Line::from(eta),
            ]
        }
    };
    Paragraph::new(lines).block(
        Block::bordered().title(format!(" battcurve — live  ({n} samples)   [q to quit] ")),
    )
}

fn time_estimate(s: &Sample) -> String {
    let p = s.power_w.abs();
    if p < 0.05 {
        return "Estimate: --".into();
    }
    let (label, wh) = match s.status {
        Status::Charging => ("Time to full", (s.energy_full_wh - s.energy_wh).max(0.0)),
        Status::Discharging => ("Time to empty", s.energy_wh.max(0.0)),
        _ => return "Estimate: --".into(),
    };
    let hours = wh / p;
    format!("{label}: {}h {:02}m", hours as u64, ((hours.fract()) * 60.0) as u64)
}

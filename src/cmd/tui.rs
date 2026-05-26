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
    let g =
        Layout::horizontal([Constraint::Percentage(50), Constraint::Percentage(50)]).split(rows[0]);
    let soc = latest
        .map(|s| (s.capacity_pct / 100.0).clamp(0.0, 1.0))
        .unwrap_or(0.0);
    let health = latest
        .map(|s| (s.health_pct() / 100.0).clamp(0.0, 1.0))
        .unwrap_or(0.0);
    render_gauge(f, g[0], " State of Charge ", soc, Color::Green);
    render_gauge(f, g[1], " Battery Health ", health, Color::Cyan);

    // --- Stats ---
    f.render_widget(stats_paragraph(latest, history.len()), rows[1]);

    // --- Line charts with labeled Y axes (units + scale) ---
    let cols =
        Layout::horizontal([Constraint::Percentage(50), Constraint::Percentage(50)]).split(rows[2]);
    let power_data: Vec<(f64, f64)> = filled
        .iter()
        .enumerate()
        .map(|(i, x)| (i as f64, x.power_w.abs()))
        .collect();
    let volt_data: Vec<(f64, f64)> = filled
        .iter()
        .enumerate()
        .map(|(i, x)| (i as f64, x.voltage_v))
        .collect();
    render_line_chart(f, cols[0], "Power", "W", Color::Yellow, &power_data);
    render_line_chart(f, cols[1], "Voltage", "V", Color::Magenta, &volt_data);
}

/// A bordered horizontal bar gauge whose percentage label is drawn in inverse
/// color per cell: dark-on-fill where the bar covers the text, color-on-dark
/// where it doesn't. `ratio` is 0.0..=1.0.
fn render_gauge(f: &mut Frame, area: Rect, title: &str, ratio: f64, color: Color) {
    let block = Block::bordered().title(title.to_string());
    let inner = block.inner(area);
    f.render_widget(block, area);
    if inner.width == 0 || inner.height == 0 {
        return;
    }

    let ratio = ratio.clamp(0.0, 1.0);
    let filled = (ratio * inner.width as f64).round() as u16;
    let bg = Color::Reset; // the terminal's own background (theme-agnostic)
    let buf = f.buffer_mut();

    // Paint the bar: colored background for the filled portion, plain otherwise.
    for y in inner.top()..inner.bottom() {
        for x in inner.left()..inner.right() {
            let on_bar = x - inner.left() < filled;
            buf[(x, y)].set_symbol(" ").set_bg(if on_bar { color } else { bg });
        }
    }

    // Overlay the centered "NN.N%" label, inverting fg/bg per cell.
    let label = format!("{:.1}%", ratio * 100.0);
    let start = inner.left() + inner.width.saturating_sub(label.len() as u16) / 2;
    let row = inner.top() + inner.height / 2;
    for (i, ch) in label.chars().enumerate() {
        let x = start + i as u16;
        if x >= inner.right() {
            break;
        }
        let on_bar = x - inner.left() < filled;
        let (fg, cell_bg) = if on_bar { (bg, color) } else { (color, bg) };
        buf[(x, row)]
            .set_char(ch)
            .set_style(Style::new().fg(fg).bg(cell_bg).add_modifier(Modifier::BOLD));
    }
}

/// A bordered line chart with a labeled Y axis (min / mid / max in `unit`).
fn render_line_chart(
    f: &mut Frame,
    area: Rect,
    title: &str,
    unit: &str,
    color: Color,
    data: &[(f64, f64)],
) {
    let block = Block::bordered().title(format!(" {title} ({unit}) "));
    if data.len() < 2 {
        f.render_widget(block, area);
        return;
    }
    let xmax = (data.len() - 1) as f64;
    let mut ymin = data.iter().map(|p| p.1).fold(f64::MAX, f64::min);
    let mut ymax = data.iter().map(|p| p.1).fold(f64::MIN, f64::max);
    if (ymax - ymin).abs() < 1e-6 {
        ymin -= 0.5;
        ymax += 0.5;
    }
    let pad = (ymax - ymin) * 0.08;
    ymin -= pad;
    ymax += pad;
    let ymid = (ymin + ymax) / 2.0;

    let datasets = vec![Dataset::default()
        .marker(symbols::Marker::Braille)
        .graph_type(GraphType::Line)
        .style(Style::new().fg(color))
        .data(data)];

    let chart = Chart::new(datasets)
        .block(block)
        .x_axis(Axis::default().bounds([0.0, xmax]))
        .y_axis(
            Axis::default()
                .style(Style::new().fg(Color::DarkGray))
                .bounds([ymin, ymax])
                .labels([
                    format!("{ymin:.2}"),
                    format!("{ymid:.2}"),
                    format!("{ymax:.2}"),
                ]),
        );
    f.render_widget(chart, area);
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
    Paragraph::new(lines)
        .block(Block::bordered().title(format!(" battcurve — live  ({n} samples)   [q to quit] ")))
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
    format!(
        "{label}: {}h {:02}m",
        hours as u64,
        ((hours.fract()) * 60.0) as u64
    )
}

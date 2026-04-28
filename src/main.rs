use std::collections::BTreeMap;
use std::io::stdout;
use std::process::Command;

use anyhow::{Context, Result};
use chrono::{Local, NaiveDate};
use crossterm::{
    event::{self, Event, KeyCode, KeyEventKind},
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
    ExecutableCommand,
};
use ratatui::{
    prelude::*,
    widgets::*,
};

const SSH_HOST: &str = "root@192.168.168.137";
const DAYS: i64 = 30;

/// One snapshot record
struct Snapshot {
    dataset: String,
    date: NaiveDate,
    used_bytes: u64,
}

/// Per-day aggregated info for a dataset
#[derive(Clone)]
struct DayInfo {
    has_snapshot: bool,
    total_used: u64,
}

fn parse_size(s: &str) -> u64 {
    let s = s.trim();
    if s == "-" || s == "0" || s == "0B" {
        return 0;
    }
    let (num_str, mult) = if let Some(n) = s.strip_suffix("T") {
        (n, 1_099_511_627_776u64)
    } else if let Some(n) = s.strip_suffix("G") {
        (n, 1_073_741_824)
    } else if let Some(n) = s.strip_suffix("M") {
        (n, 1_048_576)
    } else if let Some(n) = s.strip_suffix("K") {
        (n, 1_024)
    } else if let Some(n) = s.strip_suffix("B") {
        (n, 1)
    } else {
        (s, 1)
    };
    let val: f64 = num_str.parse().unwrap_or(0.0);
    (val * mult as f64) as u64
}

fn is_local() -> bool {
    Command::new("zfs").arg("version").output().is_ok_and(|o| o.status.success())
}

fn fetch_snapshots() -> Result<Vec<Snapshot>> {
    let zfs_cmd = "zfs list -t snapshot -o name,used,creation -Hp -s creation";
    let output = if is_local() {
        Command::new("sh")
            .args(["-c", zfs_cmd])
            .output()
            .context("Failed to run zfs")?
    } else {
        Command::new("ssh")
            .args([SSH_HOST, zfs_cmd])
            .output()
            .context("Failed to run ssh")?
    };

    if !output.status.success() {
        anyhow::bail!("command failed: {}", String::from_utf8_lossy(&output.stderr));
    }

    let cutoff = Local::now().date_naive() - chrono::Duration::days(DAYS);
    let mut snaps = Vec::new();

    for line in String::from_utf8_lossy(&output.stdout).lines() {
        let parts: Vec<&str> = line.split('\t').collect();
        if parts.len() < 3 {
            continue;
        }
        let name = parts[0];
        let used = parts[1].trim();
        let creation_ts: i64 = match parts[2].trim().parse() {
            Ok(v) => v,
            Err(_) => continue,
        };

        // Parse dataset name (before @)
        let dataset = match name.split_once('@') {
            Some((ds, _)) => ds,
            None => continue,
        };

        // Skip docker snapshots
        if dataset.contains("docker") {
            continue;
        }

        let date = chrono::DateTime::from_timestamp(creation_ts, 0)
            .map(|dt| dt.with_timezone(&Local).date_naive())
            .unwrap_or_default();

        if date < cutoff {
            continue;
        }

        snaps.push(Snapshot {
            dataset: dataset.to_string(),
            date,
            used_bytes: parse_size(used),
        });
    }

    Ok(snaps)
}

fn build_datasets(snaps: &[Snapshot]) -> (Vec<String>, BTreeMap<String, BTreeMap<NaiveDate, DayInfo>>) {
    let mut map: BTreeMap<String, BTreeMap<NaiveDate, DayInfo>> = BTreeMap::new();

    for snap in snaps {
        let entry = map.entry(snap.dataset.clone()).or_default();
        let day = entry.entry(snap.date).or_insert(DayInfo {
            has_snapshot: false,
            total_used: 0,
        });
        day.has_snapshot = true;
        day.total_used += snap.used_bytes;
    }

    let datasets: Vec<String> = map.keys().cloned().collect();
    (datasets, map)
}

fn format_bytes(b: u64) -> String {
    if b >= 1_073_741_824 {
        format!("{:.1}G", b as f64 / 1_073_741_824.0)
    } else if b >= 1_048_576 {
        format!("{:.1}M", b as f64 / 1_048_576.0)
    } else if b >= 1_024 {
        format!("{:.1}K", b as f64 / 1_024.0)
    } else {
        format!("{}B", b)
    }
}

fn main() -> Result<()> {
    // Fetch data before entering TUI
    eprintln!("Fetching ZFS snapshots via SSH...");
    let snaps = fetch_snapshots()?;
    let (datasets, data) = build_datasets(&snaps);

    if datasets.is_empty() {
        eprintln!("No snapshots found in the last {} days.", DAYS);
        return Ok(());
    }

    // Setup terminal
    enable_raw_mode()?;
    stdout().execute(EnterAlternateScreen)?;
    let mut terminal = Terminal::new(CrosstermBackend::new(stdout()))?;

    let mut selected: usize = 0;
    let today = Local::now().date_naive();
    let dates: Vec<NaiveDate> = (0..DAYS)
        .rev()
        .map(|i| today - chrono::Duration::days(i))
        .collect();

    loop {
        let ds = &datasets[selected];
        let ds_data = data.get(ds);

        let bar_data: Vec<(String, u64)> = dates
            .iter()
            .map(|d| {
                let info = ds_data.and_then(|m| m.get(d));
                let used = info.map(|i| i.total_used).unwrap_or(0);
                (d.format("%d").to_string(), used)
            })
            .collect();

        let max_val = bar_data.iter().map(|(_, v)| *v).max().unwrap_or(1).max(1);

        terminal.draw(|frame| {
            let area = frame.area();

            let chunks = Layout::default()
                .direction(Direction::Vertical)
                .constraints([
                    Constraint::Length(3),  // title
                    Constraint::Length(3),  // dataset selector
                    Constraint::Min(10),   // chart
                    Constraint::Length(1),  // sizes row
                    Constraint::Length(3),  // legend
                    Constraint::Length(2),  // help
                ])
                .split(area);

            // Title
            let title = Paragraph::new("ZFS Snapshot Stats (last 30 days)")
                .alignment(Alignment::Center)
                .block(Block::default().borders(Borders::BOTTOM));
            frame.render_widget(title, chunks[0]);

            // Dataset selector
            let ds_line = datasets
                .iter()
                .enumerate()
                .map(|(i, name)| {
                    if i == selected {
                        Span::styled(
                            format!(" [{}] ", name),
                            Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD),
                        )
                    } else {
                        Span::raw(format!("  {}  ", name))
                    }
                })
                .collect::<Vec<_>>();
            let selector = Paragraph::new(Line::from(ds_line))
                .block(Block::default().borders(Borders::ALL).title("Dataset"));
            frame.render_widget(selector, chunks[1]);

            // Bar chart — compute bar_width and gap to fill available width
            let chart_inner_width = chunks[2].width.saturating_sub(2) as usize; // minus borders
            let num_bars = dates.len();
            // Each bar takes bar_width + bar_gap, last bar has no trailing gap
            // total = num_bars * (bar_width + bar_gap) - bar_gap
            // Solve for bar_width with gap=1: bar_width = (total + 1) / num_bars - 1
            let bar_width = if num_bars > 0 {
                ((chart_inner_width + 1) / num_bars).saturating_sub(1).max(1) as u16
            } else {
                1
            };
            let bar_gap = if num_bars > 1 {
                let used_by_bars = num_bars as u16 * bar_width;
                let remaining = (chart_inner_width as u16).saturating_sub(used_by_bars);
                (remaining / (num_bars as u16 - 1)).max(0)
            } else {
                0
            };

            let bars: Vec<Bar> = dates
                .iter()
                .map(|d| {
                    let info = ds_data.and_then(|m| m.get(d));
                    let used = info.map(|i| i.total_used).unwrap_or(0);
                    let has_snap = info.map(|i| i.has_snapshot).unwrap_or(false);

                    let color = if !has_snap {
                        Color::Red
                    } else if used == 0 {
                        Color::DarkGray
                    } else {
                        Color::Green
                    };

                    let label = d.format("%m/%d").to_string();
                    Bar::default()
                        .value(used)
                        .label(Line::from(label))
                        .style(Style::default().fg(color))
                        .text_value(String::new())
                })
                .collect();

            let chart = BarChart::default()
                .block(
                    Block::default()
                        .borders(Borders::ALL)
                        .title(format!("Size change per day — {} (max: {})", ds, format_bytes(max_val))),
                )
                .data(BarGroup::default().bars(&bars))
                .bar_width(bar_width)
                .bar_gap(bar_gap)
                .max(max_val);
            frame.render_widget(chart, chunks[2]);

            // Sizes row — each value centered in its column
            let col_width = (bar_width + bar_gap) as usize;
            let sizes_str: String = dates
                .iter()
                .map(|d| {
                    let info = ds_data.and_then(|m| m.get(d));
                    let used = info.map(|i| i.total_used).unwrap_or(0);
                    let s = if used > 0 { format_bytes(used) } else { "0".into() };
                    format!("{:^width$}", s, width = col_width)
                })
                .collect();
            // offset by 1 for the left border of the chart block
            let sizes_line = format!(" {}", sizes_str);
            let sizes_widget = Paragraph::new(sizes_line)
                .style(Style::default().fg(Color::Cyan));
            frame.render_widget(sizes_widget, chunks[3]);

            // Legend
            let legend = Paragraph::new(Line::from(vec![
                Span::styled("██ ", Style::default().fg(Color::Green)),
                Span::raw("Backup done (data changed)  "),
                Span::styled("██ ", Style::default().fg(Color::DarkGray)),
                Span::raw("Backup done (0B change)  "),
                Span::styled("██ ", Style::default().fg(Color::Red)),
                Span::raw("No backup"),
            ]))
            .block(Block::default().borders(Borders::ALL).title("Legend"));
            frame.render_widget(legend, chunks[4]);

            // Help
            let help = Paragraph::new("← / → or h/l: switch dataset   q: quit")
                .alignment(Alignment::Center)
                .style(Style::default().fg(Color::DarkGray));
            frame.render_widget(help, chunks[5]);
        })?;

        // Handle input
        if event::poll(std::time::Duration::from_millis(100))? {
            if let Event::Key(key) = event::read()? {
                if key.kind == KeyEventKind::Press {
                    match key.code {
                        KeyCode::Char('q') | KeyCode::Esc => break,
                        KeyCode::Left | KeyCode::Char('h') => {
                            if selected > 0 {
                                selected -= 1;
                            }
                        }
                        KeyCode::Right | KeyCode::Char('l') => {
                            if selected < datasets.len() - 1 {
                                selected += 1;
                            }
                        }
                        _ => {}
                    }
                }
            }
        }
    }

    // Restore terminal
    disable_raw_mode()?;
    stdout().execute(LeaveAlternateScreen)?;
    Ok(())
}

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
const SOURCE_POOL: &str = "tank";
const BACKUP_POOL: &str = "backup";

/// Pool summary info
struct PoolInfo {
    size: u64,
    alloc: u64,
    free: u64,
}

/// Backup target: a top-level dataset under backup/
struct BackupTarget {
    name: String,
    used: u64,
}

#[derive(PartialEq)]
enum Page {
    Dashboard,
    Snapshots,
}

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

fn run_cmd(cmd: &str) -> Result<String> {
    let output = if is_local() {
        Command::new("sh")
            .args(["-c", cmd])
            .output()
            .context("Failed to run command")?
    } else {
        Command::new("ssh")
            .args([SSH_HOST, cmd])
            .output()
            .context("Failed to run ssh")?
    };
    if !output.status.success() {
        anyhow::bail!("command failed: {}", String::from_utf8_lossy(&output.stderr));
    }
    Ok(String::from_utf8_lossy(&output.stdout).to_string())
}

fn fetch_pool_info(pool: &str) -> Result<PoolInfo> {
    let out = run_cmd(&format!("zpool list -Hp -o size,alloc,free {}", pool))?;
    let parts: Vec<&str> = out.trim().split('\t').collect();
    if parts.len() < 3 {
        anyhow::bail!("unexpected zpool output");
    }
    Ok(PoolInfo {
        size: parts[0].trim().parse().unwrap_or(0),
        alloc: parts[1].trim().parse().unwrap_or(0),
        free: parts[2].trim().parse().unwrap_or(0),
    })
}

fn fetch_backup_targets() -> Result<Vec<BackupTarget>> {
    let out = run_cmd(&format!("zfs list -Hp -o name,used -d 1 -r {}", BACKUP_POOL))?;
    let mut targets = Vec::new();
    for line in out.lines() {
        let parts: Vec<&str> = line.split('\t').collect();
        if parts.len() < 2 { continue; }
        let name = parts[0].trim();
        // Skip the pool root itself
        if name == BACKUP_POOL { continue; }
        let used: u64 = parts[1].trim().parse().unwrap_or(0);
        targets.push(BackupTarget {
            name: name.to_string(),
            used,
        });
    }
    Ok(targets)
}

fn fetch_snapshots() -> Result<Vec<Snapshot>> {
    let zfs_cmd = "zfs list -t snapshot -o name,used,creation -Hp -s creation";
    let stdout = run_cmd(zfs_cmd)?;

    let cutoff = Local::now().date_naive() - chrono::Duration::days(DAYS);
    let mut snaps = Vec::new();

    for line in stdout.lines() {
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

        // Skip docker and backup pool snapshots
        if dataset.contains("docker") || dataset.starts_with("backup") {
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
    if b >= 1_099_511_627_776 {
        format!("{:.1}T", b as f64 / 1_099_511_627_776.0)
    } else if b >= 1_073_741_824 {
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
    eprintln!("Fetching ZFS data...");
    let snaps = fetch_snapshots()?;
    let (datasets, data) = build_datasets(&snaps);
    let backup_targets = fetch_backup_targets()?;
    let source_pool = fetch_pool_info(SOURCE_POOL)?;
    let backup_pool = fetch_pool_info(BACKUP_POOL)?;

    // Setup terminal
    enable_raw_mode()?;
    stdout().execute(EnterAlternateScreen)?;
    let mut terminal = Terminal::new(CrosstermBackend::new(stdout()))?;

    let mut selected: usize = 0;
    let mut page = Page::Dashboard;
    let today = Local::now().date_naive();
    let dates: Vec<NaiveDate> = (0..DAYS)
        .rev()
        .map(|i| today - chrono::Duration::days(i))
        .collect();

    loop {
        terminal.draw(|frame| {
            let area = frame.area();

            match page {
                Page::Dashboard => {
                    let chunks = Layout::default()
                        .direction(Direction::Vertical)
                        .constraints([
                            Constraint::Length(3),  // title
                            Constraint::Min(5),    // backup targets table
                            Constraint::Length(7),  // pool summary
                            Constraint::Length(2),  // help
                        ])
                        .split(area);

                    // Title
                    let title = Paragraph::new("ZFS Backup Dashboard")
                        .alignment(Alignment::Center)
                        .block(Block::default().borders(Borders::BOTTOM));
                    frame.render_widget(title, chunks[0]);

                    // Backup targets table
                    let total_used: u64 = backup_targets.iter().map(|t| t.used).sum();
                    let header = Row::new(vec!["Dataset", "Used"])
                        .style(Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD))
                        .bottom_margin(1);
                    let mut rows: Vec<Row> = backup_targets
                        .iter()
                        .map(|t| {
                            Row::new(vec![
                                t.name.clone(),
                                format_bytes(t.used),
                            ])
                        })
                        .collect();
                    // Separator + total row
                    rows.push(Row::new(vec![
                        String::from("─────────────────────"),
                        String::from("──────"),
                    ]).style(Style::default().fg(Color::DarkGray)));
                    rows.push(Row::new(vec![
                        String::from("Total"),
                        format_bytes(total_used),
                    ]).style(Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD)));

                    let table = Table::new(
                        rows,
                        [Constraint::Min(30), Constraint::Length(10)],
                    )
                    .header(header)
                    .block(Block::default().borders(Borders::ALL).title("Backup Targets"));
                    frame.render_widget(table, chunks[1]);

                    // Pool summary
                    let pool_header = Row::new(vec!["Pool", "Size", "Used", "Free", "Use%"])
                        .style(Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD))
                        .bottom_margin(1);
                    let pct = |alloc: u64, size: u64| -> String {
                        if size == 0 { "0%".into() } else { format!("{:.0}%", alloc as f64 / size as f64 * 100.0) }
                    };
                    let pool_rows = vec![
                        Row::new(vec![
                            SOURCE_POOL.to_string(),
                            format_bytes(source_pool.size),
                            format_bytes(source_pool.alloc),
                            format_bytes(source_pool.free),
                            pct(source_pool.alloc, source_pool.size),
                        ]),
                        Row::new(vec![
                            BACKUP_POOL.to_string(),
                            format_bytes(backup_pool.size),
                            format_bytes(backup_pool.alloc),
                            format_bytes(backup_pool.free),
                            pct(backup_pool.alloc, backup_pool.size),
                        ]),
                    ];
                    let pool_table = Table::new(
                        pool_rows,
                        [
                            Constraint::Min(10),
                            Constraint::Length(10),
                            Constraint::Length(10),
                            Constraint::Length(10),
                            Constraint::Length(6),
                        ],
                    )
                    .header(pool_header)
                    .block(Block::default().borders(Borders::ALL).title("Pool Summary"));
                    frame.render_widget(pool_table, chunks[2]);

                    // Help
                    let help_text = if datasets.is_empty() {
                        "q: quit"
                    } else {
                        "Enter/Tab: snapshot details   q: quit"
                    };
                    let help = Paragraph::new(help_text)
                        .alignment(Alignment::Center)
                        .style(Style::default().fg(Color::DarkGray));
                    frame.render_widget(help, chunks[3]);
                }

                Page::Snapshots => {
                    if datasets.is_empty() { return; }

                    let ds = &datasets[selected];
                    let ds_data = data.get(ds);
                    let max_val = dates
                        .iter()
                        .map(|d| ds_data.and_then(|m| m.get(d)).map(|i| i.total_used).unwrap_or(0))
                        .max()
                        .unwrap_or(1)
                        .max(1);

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

                    // Dataset selector with horizontal scrolling
                    let selector_inner_width = chunks[1].width.saturating_sub(2) as usize;
                    let ds_widths: Vec<usize> = datasets
                        .iter()
                        .map(|name| name.len() + 4)
                        .collect();
                    let mut start = selected;
                    let mut end = selected + 1;
                    let mut total_w = ds_widths[selected];
                    loop {
                        let mut expanded = false;
                        if start > 0 && total_w + ds_widths[start - 1] <= selector_inner_width {
                            start -= 1;
                            total_w += ds_widths[start];
                            expanded = true;
                        }
                        if end < datasets.len() && total_w + ds_widths[end] <= selector_inner_width {
                            total_w += ds_widths[end];
                            end += 1;
                            expanded = true;
                        }
                        if !expanded { break; }
                    }
                    let mut ds_line: Vec<Span> = Vec::new();
                    if start > 0 {
                        ds_line.push(Span::styled("◀ ", Style::default().fg(Color::DarkGray)));
                    }
                    for (i, name) in datasets.iter().enumerate() {
                        if i < start || i >= end { continue; }
                        if i == selected {
                            ds_line.push(Span::styled(
                                format!(" [{}] ", name),
                                Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD),
                            ));
                        } else {
                            ds_line.push(Span::raw(format!("  {}  ", name)));
                        }
                    }
                    if end < datasets.len() {
                        ds_line.push(Span::styled(" ▶", Style::default().fg(Color::DarkGray)));
                    }
                    let selector = Paragraph::new(Line::from(ds_line))
                        .block(Block::default().borders(Borders::ALL).title("Dataset"));
                    frame.render_widget(selector, chunks[1]);

                    // Bar chart
                    let chart_inner_width = chunks[2].width.saturating_sub(2) as usize;
                    let num_bars = dates.len();
                    let bar_width = if num_bars > 0 {
                        ((chart_inner_width + 1) / num_bars).saturating_sub(1).max(1) as u16
                    } else {
                        1
                    };
                    let bar_gap = if num_bars > 1 {
                        let used_by_bars = num_bars as u16 * bar_width;
                        let remaining = (chart_inner_width as u16).saturating_sub(used_by_bars);
                        remaining / (num_bars as u16 - 1)
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

                    // Sizes row
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
                    let help = Paragraph::new("← / → or h/l: switch dataset   Esc/Backspace: dashboard   q: quit")
                        .alignment(Alignment::Center)
                        .style(Style::default().fg(Color::DarkGray));
                    frame.render_widget(help, chunks[5]);
                }
            }
        })?;

        // Handle input
        if event::poll(std::time::Duration::from_millis(100))? {
            if let Event::Key(key) = event::read()? {
                if key.kind == KeyEventKind::Press {
                    match (&page, key.code) {
                        (_, KeyCode::Char('q')) => break,
                        // Dashboard navigation
                        (Page::Dashboard, KeyCode::Enter | KeyCode::Tab) => {
                            if !datasets.is_empty() {
                                page = Page::Snapshots;
                            }
                        }
                        // Snapshots navigation
                        (Page::Snapshots, KeyCode::Esc | KeyCode::Backspace) => {
                            page = Page::Dashboard;
                        }
                        (Page::Snapshots, KeyCode::Left | KeyCode::Char('h')) => {
                            if selected > 0 { selected -= 1; }
                        }
                        (Page::Snapshots, KeyCode::Right | KeyCode::Char('l')) => {
                            if selected < datasets.len() - 1 { selected += 1; }
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

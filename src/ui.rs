//! Render the App. Pure ratatui — no I/O.

use crate::app::{App, Mode, Tab};
use crate::jsonhl;
use crate::pullprog;
use crate::theme::AlertLevel;
use crate::trivy::Severity;
use humansize::{format_size, BINARY};
use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{
        Block, Borders, Cell, Clear, Gauge, Paragraph, Row, Sparkline, Table, TableState, Tabs,
        Wrap,
    },
    Frame,
};

pub fn draw(f: &mut Frame, app: &mut App) {
    let area = f.area();
    let outer = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3), // header (tabs)
            Constraint::Length(5), // sparklines
            Constraint::Min(5),    // body
            Constraint::Length(1), // filter bar (always — empty when not filtering)
            Constraint::Length(1), // status
        ])
        .split(area);

    // Cache regions for mouse hit testing. Body region excludes the block
    // border so click-row math lines up with the rendered rows below.
    app.layout.tabs = Some(outer[0]);
    app.layout.body = Some(inner_body(outer[2]));

    draw_tabs(f, app, outer[0]);
    draw_sparklines(f, app, outer[1]);
    draw_body(f, app, outer[2]);
    draw_filter_bar(f, app, outer[3]);
    draw_status(f, app, outer[4]);

    // Overlays.
    match app.mode {
        Mode::Detail => draw_detail_overlay(f, app, area),
        Mode::PromptPull => draw_prompt_overlay(f, app, area),
        Mode::PromptBuild => draw_build_prompt_overlay(f, app, area),
        Mode::PullProgress => draw_pull_overlay(f, app, area),
        Mode::Help => draw_help_overlay(f, app, area),
        Mode::ContextMenu => draw_context_menu(f, app, area),
        Mode::FilePicker => draw_file_picker(f, app, area),
        Mode::ProfilePicker => draw_profile_picker(f, app, area),
        Mode::PromptStackName => draw_stack_name_prompt(f, app, area),
        Mode::TrivyResult => draw_trivy_result(f, app, area),
        Mode::UpdatePrompt => draw_update_prompt(f, app, area),
        Mode::Browse | Mode::Filter | Mode::LogSearch => {}
    }
}

/// Body Rect minus the block border (1 row top/bottom, 1 col left/right) and
/// the table header row (1).
fn inner_body(r: Rect) -> Rect {
    Rect {
        x: r.x.saturating_add(1),
        y: r.y.saturating_add(1).saturating_add(1),
        width: r.width.saturating_sub(2),
        height: r.height.saturating_sub(3),
    }
}

fn draw_tabs(f: &mut Frame, app: &App, area: Rect) {
    let titles: Vec<Line> = Tab::ALL
        .iter()
        .map(|t| Line::from(Span::styled(t.label(), Style::default().fg(Color::White))))
        .collect();
    let idx = Tab::ALL.iter().position(|t| *t == app.tab).unwrap_or(0);
    let title = format!(" cgui · runtime: {} ", crate::runtime::name());
    let tabs = Tabs::new(titles)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(Span::styled(
                    title,
                    Style::default().fg(app.theme.accent).add_modifier(Modifier::BOLD),
                )),
        )
        .select(idx)
        .style(Style::default().fg(Color::DarkGray))
        .highlight_style(
            Style::default()
                .fg(Color::Black)
                .bg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        );
    f.render_widget(tabs, area);
}

fn draw_sparklines(f: &mut Frame, app: &App, area: Rect) {
    let cols = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
        .split(area);

    let cpu: Vec<u64> = app
        .cpu_history
        .iter()
        .map(|v| v.max(0.0).round() as u64)
        .collect();
    let mem: Vec<u64> = app
        .mem_history
        .iter()
        .map(|v| v.max(0.0).round() as u64)
        .collect();

    let cpu_now = app.cpu_history.back().copied().unwrap_or(0.0);
    let mem_now = app.mem_history.back().copied().unwrap_or(0.0);

    f.render_widget(
        Sparkline::default()
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title(format!(" CPU  {cpu_now:>5.1}% (Σ across containers) ")),
            )
            .data(&cpu)
            .style(Style::default().fg(Color::Green)),
        cols[0],
    );
    f.render_widget(
        Sparkline::default()
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title(format!(" MEM  {mem_now:>5.1}% of limit ")),
            )
            .data(&mem)
            .style(Style::default().fg(Color::Magenta)),
        cols[1],
    );
}

fn draw_body(f: &mut Frame, app: &mut App, area: Rect) {
    match app.tab {
        Tab::Containers => draw_containers(f, app, area),
        Tab::Images => draw_images(f, app, area),
        Tab::Volumes => draw_volumes(f, app, area),
        Tab::Networks => draw_networks(f, app, area),
        Tab::Stacks => draw_stacks(f, app, area),
        Tab::Logs => draw_logs(f, app, area),
    }
}

fn header_style() -> Style {
    Style::default()
        .fg(Color::Black)
        .bg(Color::Cyan)
        .add_modifier(Modifier::BOLD)
}

fn block_title(app: &App, label: &str, total: usize, shown: usize) -> String {
    let sort = app.sort_key.label(app.tab);
    if shown == total {
        format!(" {label} ({total}) · sort:{sort} ")
    } else {
        format!(" {label} ({shown}/{total}) · sort:{sort} · filter:{} ", app.filter)
    }
}

fn draw_containers(f: &mut Frame, app: &mut App, area: Rect) {
    let header = Row::new(vec!["", "ID", "IMAGE", "STATUS", "CPU%", "TREND", "MEM", "PORTS"])
        .style(header_style());
    let view = app.view_indices();
    let stats = app.stats_by_id();
    let rows: Vec<Row> = view
        .iter()
        .map(|&i| {
            let c = &app.containers[i];
            let status_style = match c.status.as_str() {
                "running" => Style::default().fg(Color::Green).add_modifier(Modifier::BOLD),
                "stopped" | "exited" => Style::default().fg(Color::Red),
                _ => Style::default().fg(Color::Yellow),
            };
            let mark = if app.marked.contains(&c.id) {
                Cell::from("●").style(Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD))
            } else {
                Cell::from(" ")
            };

            // Live stats overlay if available (running container with a stats sample),
            // else fall back to the configured CPU count and memory limit.
            let (cpu_cell, mem_cell) = match stats.get(&c.id) {
                Some(&(cpu, used, limit)) => {
                    let cpu_str = format!("{cpu:>5.1}%");
                    let cpu_style = cpu_color(cpu);
                    let mem_str = if limit > 0 {
                        format!(
                            "{} / {}",
                            format_size(used, BINARY),
                            format_size(limit, BINARY)
                        )
                    } else {
                        format_size(used, BINARY)
                    };
                    let mem_pct = if limit > 0 {
                        (used as f64 / limit as f64) * 100.0
                    } else {
                        0.0
                    };
                    (
                        Cell::from(cpu_str).style(cpu_style),
                        Cell::from(mem_str).style(mem_color(mem_pct)),
                    )
                }
                None => (
                    Cell::from(format!("    {}", c.cpus))
                        .style(Style::default().fg(Color::DarkGray)),
                    Cell::from(format_size(c.memory_bytes, BINARY))
                        .style(Style::default().fg(Color::DarkGray)),
                ),
            };

            let trend_history = app.cpu_history_per_id.get(&c.id);
            let trend = sparkline_str(trend_history.map(|h| h.iter().copied()).into_iter().flatten(), 16);
            let trend_cell = Cell::from(trend).style(Style::default().fg(app.theme.success));

            // Alert level: max of CPU and MEM alert levels for the live sample.
            let (cpu_lvl, mem_lvl) = match stats.get(&c.id) {
                Some(&(cpu, used, limit)) => {
                    let mem_pct = if limit > 0 { (used as f64 / limit as f64) * 100.0 } else { 0.0 };
                    (app.theme.alerts.cpu_level(cpu), app.theme.alerts.mem_level(mem_pct))
                }
                None => (AlertLevel::None, AlertLevel::None),
            };
            let row_lvl = match (cpu_lvl, mem_lvl) {
                (AlertLevel::Alert, _) | (_, AlertLevel::Alert) => AlertLevel::Alert,
                (AlertLevel::Warn, _) | (_, AlertLevel::Warn) => AlertLevel::Warn,
                _ => AlertLevel::None,
            };
            let row_style = alert_row_style(app, row_lvl);

            let mut row = Row::new(vec![
                mark,
                Cell::from(c.id.clone()),
                Cell::from(c.image.clone()).style(Style::default().fg(Color::Blue)),
                Cell::from(c.status.clone()).style(status_style),
                cpu_cell,
                trend_cell,
                mem_cell,
                Cell::from(c.ports.join(", ")),
            ]);
            if let Some(s) = row_style {
                row = row.style(s);
            }
            row
        })
        .collect();
    let widths = [
        Constraint::Length(2),
        Constraint::Percentage(20),
        Constraint::Percentage(28),
        Constraint::Length(10),
        Constraint::Length(7),
        Constraint::Length(16),
        Constraint::Length(20),
        Constraint::Min(10),
    ];
    let mark_count = app.marked.len();
    let mut title = block_title(app, "Containers", app.containers.len(), rows.len());
    if mark_count > 0 {
        title = format!("{title}· marked:{mark_count} ");
    }
    let table = Table::new(rows, widths)
        .header(header)
        .block(Block::default().borders(Borders::ALL).title(title))
        .row_highlight_style(
            Style::default()
                .bg(Color::DarkGray)
                .add_modifier(Modifier::BOLD),
        )
        .highlight_symbol("▶ ");
    let mut state = TableState::default();
    state.select(Some(app.selected));
    f.render_stateful_widget(table, area, &mut state);
}

/// Background style for an alerting row. Alert pulses (only on alert, not
/// warn) when `theme.alerts.pulse` is true; warn rows get a steady tint.
fn alert_row_style(app: &App, level: AlertLevel) -> Option<Style> {
    match level {
        AlertLevel::None => None,
        AlertLevel::Warn => Some(Style::default().bg(dim_bg(app.theme.warning))),
        AlertLevel::Alert => {
            let lit = !app.theme.alerts.pulse || app.pulse_phase();
            if lit {
                Some(
                    Style::default()
                        .bg(dim_bg(app.theme.danger))
                        .fg(Color::White)
                        .add_modifier(Modifier::BOLD),
                )
            } else {
                Some(Style::default().bg(dim_bg(app.theme.warning)))
            }
        }
    }
}

/// Convert a foreground accent into a row-background tint. Truecolor
/// variants are dimmed to ~25% to keep text readable; named colors fall
/// back to themselves (terminal-specific).
fn dim_bg(c: Color) -> Color {
    match c {
        Color::Rgb(r, g, b) => Color::Rgb(r / 4, g / 4, b / 4),
        other => other,
    }
}

/// Render a sequence of CPU% samples as a unicode bar chart, right-aligned to
/// `width` characters (newest sample at the right). Empty / single-sample
/// histories render as spaces.
fn sparkline_str<I: IntoIterator<Item = f64>>(samples: I, width: usize) -> String {
    const BARS: &[char] = &[' ', '▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];
    let mut v: Vec<f64> = samples.into_iter().collect();
    if v.is_empty() {
        return " ".repeat(width);
    }
    if v.len() > width {
        v.drain(0..v.len() - width);
    }
    let max = v.iter().copied().fold(0.0_f64, f64::max).max(1.0);
    let mut s = String::with_capacity(width);
    let pad = width - v.len();
    for _ in 0..pad {
        s.push(' ');
    }
    for x in v {
        let idx = ((x / max) * (BARS.len() - 1) as f64).round() as usize;
        s.push(BARS[idx.min(BARS.len() - 1)]);
    }
    s
}

fn cpu_color(pct: f64) -> Style {
    if pct >= 80.0 {
        Style::default().fg(Color::Red).add_modifier(Modifier::BOLD)
    } else if pct >= 40.0 {
        Style::default().fg(Color::Yellow)
    } else if pct > 0.0 {
        Style::default().fg(Color::Green)
    } else {
        Style::default().fg(Color::DarkGray)
    }
}
fn mem_color(pct: f64) -> Style {
    if pct >= 90.0 {
        Style::default().fg(Color::Red).add_modifier(Modifier::BOLD)
    } else if pct >= 70.0 {
        Style::default().fg(Color::Yellow)
    } else {
        Style::default().fg(Color::Magenta)
    }
}

fn draw_images(f: &mut Frame, app: &mut App, area: Rect) {
    let header = Row::new(vec!["REFERENCE", "SIZE", "DIGEST"]).style(header_style());
    let view = app.view_indices();
    let rows: Vec<Row> = view
        .iter()
        .map(|&i| {
            let im = &app.images[i];
            Row::new(vec![
                Cell::from(im.reference.clone()).style(Style::default().fg(Color::Blue)),
                Cell::from(im.size.clone()),
                Cell::from(short_digest(&im.digest)).style(Style::default().fg(Color::DarkGray)),
            ])
        })
        .collect();
    let widths = [
        Constraint::Percentage(50),
        Constraint::Length(12),
        Constraint::Min(10),
    ];
    let title = block_title(app, "Images", app.images.len(), rows.len());
    let mut state = TableState::default();
    state.select(Some(app.selected));
    let table = Table::new(rows, widths)
        .header(header)
        .block(Block::default().borders(Borders::ALL).title(title))
        .row_highlight_style(Style::default().bg(Color::DarkGray))
        .highlight_symbol("▶ ");
    f.render_stateful_widget(table, area, &mut state);
}

fn draw_volumes(f: &mut Frame, app: &mut App, area: Rect) {
    let header = Row::new(vec!["NAME", "DRIVER", "SOURCE"]).style(header_style());
    let view = app.view_indices();
    let rows: Vec<Row> = view
        .iter()
        .map(|&i| {
            let v = &app.volumes[i];
            Row::new(vec![
                Cell::from(v.name.clone()),
                Cell::from(v.driver.clone()),
                Cell::from(v.source.clone()).style(Style::default().fg(Color::DarkGray)),
            ])
        })
        .collect();
    let widths = [
        Constraint::Percentage(30),
        Constraint::Length(10),
        Constraint::Min(20),
    ];
    let title = block_title(app, "Volumes", app.volumes.len(), rows.len());
    let mut state = TableState::default();
    state.select(Some(app.selected));
    let table = Table::new(rows, widths)
        .header(header)
        .block(Block::default().borders(Borders::ALL).title(title))
        .row_highlight_style(Style::default().bg(Color::DarkGray))
        .highlight_symbol("▶ ");
    f.render_stateful_widget(table, area, &mut state);
}

fn draw_networks(f: &mut Frame, app: &mut App, area: Rect) {
    let header = Row::new(vec!["ID", "MODE", "STATE", "SUBNET"]).style(header_style());
    let view = app.view_indices();
    let rows: Vec<Row> = view
        .iter()
        .map(|&i| {
            let n = &app.networks[i];
            let state_style = if n.state == "running" {
                Style::default().fg(Color::Green)
            } else {
                Style::default().fg(Color::Red)
            };
            Row::new(vec![
                Cell::from(n.id.clone()),
                Cell::from(n.mode.clone()),
                Cell::from(n.state.clone()).style(state_style),
                Cell::from(n.subnet.clone()).style(Style::default().fg(Color::DarkGray)),
            ])
        })
        .collect();
    let widths = [
        Constraint::Length(20),
        Constraint::Length(10),
        Constraint::Length(10),
        Constraint::Min(20),
    ];
    let title = block_title(app, "Networks", app.networks.len(), rows.len());
    let mut state = TableState::default();
    state.select(Some(app.selected));
    let table = Table::new(rows, widths)
        .header(header)
        .block(Block::default().borders(Borders::ALL).title(title))
        .row_highlight_style(Style::default().bg(Color::DarkGray))
        .highlight_symbol("▶ ");
    f.render_stateful_widget(table, area, &mut state);
}

fn draw_stacks(f: &mut Frame, app: &mut App, area: Rect) {
    let header = Row::new(vec!["NAME", "SERVICES", "RUNNING", "HEALTH", "RESTART", "SOURCE"])
        .style(header_style());
    let view = app.view_indices();
    let running_names: std::collections::HashSet<String> = app
        .containers
        .iter()
        .filter(|c| c.status == "running")
        .map(|c| c.id.clone())
        .collect();
    let rows: Vec<Row> = view
        .iter()
        .map(|&i| {
            let s = &app.stacks[i];
            let total = s.services.len();
            let mut up = 0usize;
            for svc in &s.services {
                if running_names.contains(&crate::stacks::container_name(&s.name, &svc.name)) {
                    up += 1;
                }
            }
            let running_style = if up == total && total > 0 {
                Style::default().fg(app.theme.success).add_modifier(Modifier::BOLD)
            } else if up == 0 {
                Style::default().fg(app.theme.muted)
            } else {
                Style::default().fg(app.theme.warning)
            };
            // Aggregate health across services that have a healthcheck
            // configured: ✓ if all known checks are ok, ✗ if any failed,
            // — if none have run yet, blank if no service has a healthcheck.
            let configured: Vec<&str> = s
                .services
                .iter()
                .filter(|svc| svc.healthcheck.is_some())
                .map(|svc| svc.name.as_str())
                .collect();
            let (health_label, health_style) = if configured.is_empty() {
                ("".to_string(), Style::default().fg(app.theme.muted))
            } else {
                let mut all_ok = true;
                let mut any_seen = false;
                let mut any_fail = false;
                for svc_name in &configured {
                    if let Some(h) = app.health.get(&(s.name.clone(), svc_name.to_string())) {
                        if let Some(ok) = h.ok {
                            any_seen = true;
                            if !ok {
                                all_ok = false;
                                any_fail = true;
                            }
                        }
                    } else {
                        all_ok = false;
                    }
                }
                if !any_seen {
                    ("waiting".into(), Style::default().fg(app.theme.muted))
                } else if any_fail {
                    (
                        "✗ unhealthy".into(),
                        Style::default().fg(app.theme.danger).add_modifier(Modifier::BOLD),
                    )
                } else if all_ok {
                    (
                        format!("✓ healthy ({})", configured.len()),
                        Style::default().fg(app.theme.success).add_modifier(Modifier::BOLD),
                    )
                } else {
                    ("partial".into(), Style::default().fg(app.theme.warning))
                }
            };

            // Restart-policy summary: "always·on-fail·no" counts.
            let mut n_always = 0;
            let mut n_onfail = 0;
            for svc in &s.services {
                match svc.restart_policy() {
                    crate::stacks::RestartPolicy::Always => n_always += 1,
                    crate::stacks::RestartPolicy::OnFailure => n_onfail += 1,
                    _ => {}
                }
            }
            let restart_label = if n_always == 0 && n_onfail == 0 {
                "—".to_string()
            } else {
                let mut parts = Vec::new();
                if n_always > 0 {
                    parts.push(format!("always:{n_always}"));
                }
                if n_onfail > 0 {
                    parts.push(format!("on-fail:{n_onfail}"));
                }
                parts.join(" ")
            };

            let src = s
                .source
                .as_ref()
                .map(|p| p.display().to_string())
                .unwrap_or_default();
            Row::new(vec![
                Cell::from(s.name.clone()).style(Style::default().fg(app.theme.info).add_modifier(Modifier::BOLD)),
                Cell::from(total.to_string()),
                Cell::from(format!("{up}/{total}")).style(running_style),
                Cell::from(health_label).style(health_style),
                Cell::from(restart_label).style(Style::default().fg(app.theme.muted)),
                Cell::from(src).style(Style::default().fg(app.theme.muted)),
            ])
        })
        .collect();
    let widths = [
        Constraint::Percentage(18),
        Constraint::Length(10),
        Constraint::Length(10),
        Constraint::Length(20),
        Constraint::Length(20),
        Constraint::Min(20),
    ];
    let title = block_title(app, "Stacks", app.stacks.len(), rows.len());
    let mut state = TableState::default();
    state.select(Some(app.selected));
    let table = Table::new(rows, widths)
        .header(header)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(format!("{title}· u up · D down · Enter detail · l logs (1st svc) ")),
        )
        .row_highlight_style(Style::default().bg(Color::DarkGray))
        .highlight_symbol("▶ ");
    f.render_stateful_widget(table, area, &mut state);
}

fn draw_logs(f: &mut Frame, app: &App, area: Rect) {
    let mode_tag = if app.log_search_regex { "regex" } else { "text" };
    let buf_text: String = match app.logs_buf.lock() {
        Ok(v) => v.join("\n"),
        Err(_) => String::new(),
    };
    let compiled_re = if app.log_search_regex && !app.log_search.is_empty() {
        regex::RegexBuilder::new(&app.log_search)
            .case_insensitive(true)
            .build()
            .ok()
    } else {
        None
    };
    let match_count = if let Some(re) = &compiled_re {
        count_regex_matches(&buf_text, re)
    } else if !app.log_search.is_empty() {
        count_matches(&buf_text, &app.log_search)
    } else {
        0
    };
    let stream_tag = if app.log_following { "● follow" } else { "static" };
    let title = match (&app.log_target, app.log_search.is_empty()) {
        (Some(id), true) => format!(
            " Logs · {id} · {stream_tag} (/ search · ^R regex · l reload · F follow · wheel scrolls) "
        ),
        (Some(id), false) => {
            let bad = compiled_re.is_none() && app.log_search_regex;
            if bad {
                format!(" Logs · {id} · {stream_tag} · {mode_tag}:{}  (regex error) ", app.log_search)
            } else {
                format!(
                    " Logs · {id} · {stream_tag} · {mode_tag}:{}  ({} matches) ",
                    app.log_search, match_count
                )
            }
        }
        (None, _) => " Logs (select a container, press l for one-shot or F to follow) ".to_string(),
    };

    // Auto-tail when following and the user hasn't scrolled.
    let inner_h = area.height.saturating_sub(2) as usize;
    let visible_text = if app.log_following && app.log_scroll == 0 && inner_h > 0 {
        let lines: Vec<&str> = buf_text.lines().collect();
        let start = lines.len().saturating_sub(inner_h);
        lines[start..].join("\n")
    } else {
        buf_text
    };

    let lines: Vec<Line> = if visible_text.is_empty() {
        vec![Line::from("No logs loaded.")]
    } else if app.log_search.is_empty() {
        visible_text.lines().map(|l| Line::from(l.to_string())).collect()
    } else if let Some(re) = &compiled_re {
        visible_text.lines().map(|l| highlight_regex(l, re)).collect()
    } else {
        visible_text
            .lines()
            .map(|l| highlight_search(l, &app.log_search))
            .collect()
    };

    let title_color = if app.log_following { app.theme.success } else { app.theme.muted };
    let p = Paragraph::new(lines)
        .wrap(Wrap { trim: false })
        .scroll((app.log_scroll, 0))
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(title_color))
                .title(title),
        );
    f.render_widget(p, area);
}

fn draw_file_picker(f: &mut Frame, app: &App, area: Rect) {
    let r = centered(area, 80, 70);
    f.render_widget(Clear, r);
    let title = format!(
        " Pick build context · {} · Enter descend · . pick · Esc cancel ",
        app.picker.path.display()
    );
    let lines: Vec<Line> = app
        .picker
        .entries
        .iter()
        .enumerate()
        .map(|(i, e)| {
            let is_sel = i == app.picker.selected;
            let icon = if e.is_dir { "📁" } else { "  " };
            let text = format!(" {icon} {} ", e.name);
            let style = if is_sel {
                Style::default()
                    .fg(Color::Black)
                    .bg(app.theme.accent)
                    .add_modifier(Modifier::BOLD)
            } else if e.is_dir {
                Style::default().fg(app.theme.info).add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(app.theme.muted)
            };
            Line::from(Span::styled(text, style))
        })
        .collect();
    let p = Paragraph::new(lines).block(
        Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(app.theme.accent))
            .title(Span::styled(
                title,
                Style::default().fg(app.theme.accent).add_modifier(Modifier::BOLD),
            )),
    );
    f.render_widget(p, r);
}

fn draw_profile_picker(f: &mut Frame, app: &App, area: Rect) {
    let r = centered(area, 60, 40);
    f.render_widget(Clear, r);
    let active = crate::runtime::name();
    let lines: Vec<Line> = app
        .profiles
        .iter()
        .enumerate()
        .map(|(i, p)| {
            let is_sel = i == app.profile_picker_selected;
            let active_marker = if p.name == active { "● " } else { "  " };
            let text = format!(" {active_marker}{:<16}  {}", p.name, p.binary);
            let style = if is_sel {
                Style::default()
                    .fg(Color::Black)
                    .bg(app.theme.accent)
                    .add_modifier(Modifier::BOLD)
            } else if p.name == active {
                Style::default().fg(app.theme.success).add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(app.theme.primary)
            };
            Line::from(Span::styled(text, style))
        })
        .collect();
    let lines = if lines.is_empty() {
        vec![Line::from(Span::styled(
            "  no profiles loaded — see ~/.config/cgui/profiles.toml",
            Style::default().fg(app.theme.muted),
        ))]
    } else {
        lines
    };
    let p = Paragraph::new(lines).block(
        Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(app.theme.accent))
            .title(Span::styled(
                " Runtime profile · Enter activate · Esc cancel ",
                Style::default().fg(app.theme.accent).add_modifier(Modifier::BOLD),
            )),
    );
    f.render_widget(p, r);
}

fn count_matches(text: &str, needle: &str) -> usize {
    if needle.is_empty() {
        return 0;
    }
    let needle = needle.to_lowercase();
    text.lines()
        .map(|l| {
            let lower = l.to_lowercase();
            let mut start = 0usize;
            let mut n = 0;
            while let Some(off) = lower[start..].find(&needle) {
                n += 1;
                start += off + needle.len().max(1);
            }
            n
        })
        .sum()
}

fn count_regex_matches(text: &str, re: &regex::Regex) -> usize {
    text.lines().map(|l| re.find_iter(l).count()).sum()
}

/// Highlight regex matches in a line. Any compile error is handled by the
/// caller (we simply fall back to plain rendering there).
fn highlight_regex(line: &str, re: &regex::Regex) -> Line<'static> {
    let mut spans: Vec<Span<'static>> = Vec::new();
    let mut cursor = 0usize;
    for m in re.find_iter(line) {
        if m.start() > cursor {
            spans.push(Span::raw(line[cursor..m.start()].to_string()));
        }
        spans.push(Span::styled(
            m.as_str().to_string(),
            Style::default()
                .fg(Color::Black)
                .bg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        ));
        cursor = m.end();
        // Avoid infinite loop on zero-width matches.
        if m.end() == m.start() {
            cursor += 1;
        }
    }
    if cursor < line.len() {
        spans.push(Span::raw(line[cursor..].to_string()));
    }
    Line::from(spans)
}

/// Render a single log line as a sequence of Spans, with case-insensitive
/// highlighting of any occurrence of `needle`. Preserves original casing.
fn highlight_search(line: &str, needle: &str) -> Line<'static> {
    if needle.is_empty() {
        return Line::from(line.to_string());
    }
    let lower = line.to_lowercase();
    let needle_lc = needle.to_lowercase();
    let mut spans: Vec<Span<'static>> = Vec::new();
    let mut cursor = 0usize;
    while let Some(off) = lower[cursor..].find(&needle_lc) {
        let abs = cursor + off;
        if abs > cursor {
            spans.push(Span::raw(line[cursor..abs].to_string()));
        }
        let end = abs + needle.len();
        spans.push(Span::styled(
            line[abs..end].to_string(),
            Style::default()
                .fg(Color::Black)
                .bg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        ));
        cursor = end;
        if needle.is_empty() {
            break;
        }
    }
    if cursor < line.len() {
        spans.push(Span::raw(line[cursor..].to_string()));
    }
    Line::from(spans)
}

fn draw_filter_bar(f: &mut Frame, app: &App, area: Rect) {
    if app.mode == Mode::Filter {
        let p = Paragraph::new(Line::from(vec![
            Span::styled(" /", Style::default().fg(Color::Yellow)),
            Span::raw(&app.filter),
            Span::styled("█", Style::default().fg(Color::Yellow)),
            Span::styled(
                "   (Enter apply · Esc cancel · Backspace)",
                Style::default().fg(Color::DarkGray),
            ),
        ]));
        f.render_widget(p, area);
    } else if app.mode == Mode::LogSearch {
        let p = Paragraph::new(Line::from(vec![
            Span::styled(" /", Style::default().fg(Color::Yellow)),
            Span::raw(&app.log_search),
            Span::styled("█", Style::default().fg(Color::Yellow)),
            Span::styled(
                "   (search-as-you-type · Enter keep · Esc clear)",
                Style::default().fg(Color::DarkGray),
            ),
        ]));
        f.render_widget(p, area);
    } else if !app.filter.is_empty() {
        let p = Paragraph::new(Line::from(vec![
            Span::styled(" filter: ", Style::default().fg(Color::DarkGray)),
            Span::styled(&app.filter, Style::default().fg(Color::Yellow)),
            Span::styled("   (/ edit · Esc clear)", Style::default().fg(Color::DarkGray)),
        ]));
        f.render_widget(p, area);
    } else if app.tab == Tab::Logs && !app.log_search.is_empty() {
        let p = Paragraph::new(Line::from(vec![
            Span::styled(" search: ", Style::default().fg(Color::DarkGray)),
            Span::styled(&app.log_search, Style::default().fg(Color::Yellow)),
            Span::styled("   (/ edit · Esc clear)", Style::default().fg(Color::DarkGray)),
        ]));
        f.render_widget(p, area);
    }
}

fn draw_status(f: &mut Frame, app: &App, area: Rect) {
    let mut spans = vec![Span::styled(
        format!(" {} ", app.status),
        Style::default().fg(Color::Black).bg(Color::White),
    )];
    // Update-available chip(s). One per component that's behind and not
    // dismissed for this session, on theme.info so it doesn't collide with
    // pull/build (warning) or success chips.
    for u in app.visible_updates() {
        let chip = format!(
            " ⬆ {} {} → {} · U to view ",
            u.component.label(),
            u.installed,
            u.latest
        );
        spans.push(Span::styled(
            chip,
            Style::default()
                .fg(Color::Black)
                .bg(app.theme.info)
                .add_modifier(Modifier::BOLD),
        ));
    }
    // Background-pull indicator: visible whenever a pull is running OR finished
    // but not currently focused, prompting the user to re-attach with `P`.
    if app.pull_attachable() && app.mode != Mode::PullProgress {
        let pct = app
            .pull_log
            .lock()
            .ok()
            .and_then(|v| pullprog::parse_progress(&v))
            .map(|p| format!("{:.0}%", p * 100.0))
            .unwrap_or_else(|| "…".into());
        let label = match (&app.pull_reference, app.pull_running) {
            (Some(r), true) => format!(" ⟳ pulling {r} {pct}  P to view "),
            (Some(r), false) => format!(" ✓ pulled {r}  P to view "),
            (None, true) => " ⟳ pull running · P to view ".into(),
            (None, false) => " ✓ pull done · P to view ".into(),
        };
        let style = if app.pull_running {
            Style::default()
                .fg(Color::Black)
                .bg(Color::Yellow)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default()
                .fg(Color::Black)
                .bg(Color::Green)
                .add_modifier(Modifier::BOLD)
        };
        spans.push(Span::styled(label, style));
    }
    let p = Paragraph::new(Line::from(spans));
    f.render_widget(p, area);
}

fn centered(area: Rect, w_pct: u16, h_pct: u16) -> Rect {
    let v = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage((100 - h_pct) / 2),
            Constraint::Percentage(h_pct),
            Constraint::Percentage((100 - h_pct) / 2),
        ])
        .split(area);
    Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage((100 - w_pct) / 2),
            Constraint::Percentage(w_pct),
            Constraint::Percentage((100 - w_pct) / 2),
        ])
        .split(v[1])[1]
}

fn draw_detail_overlay(f: &mut Frame, app: &App, area: Rect) {
    let r = centered(area, 80, 80);
    f.render_widget(Clear, r);
    let title = " Inspect (↑↓/PgUp/PgDn scroll · Esc close) ";
    let lines = jsonhl::highlight(&app.detail);
    let p = Paragraph::new(lines)
        .scroll((app.detail_scroll, 0))
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::Cyan))
                .title(Span::styled(title, Style::default().fg(Color::Cyan))),
        );
    f.render_widget(p, r);
}

fn draw_prompt_overlay(f: &mut Frame, app: &App, area: Rect) {
    let r = centered(area, 60, 25);
    f.render_widget(Clear, r);
    let recents_n = app.prefs.recent_pulls.len();
    let recents_hint = if recents_n > 0 {
        let pos = match app.recent_idx {
            Some(i) => format!("{}/{recents_n}", i + 1),
            None => "—".into(),
        };
        format!("↑↓ recent ({pos})")
    } else {
        "(no recent pulls)".into()
    };
    let body = Paragraph::new(vec![
        Line::from(Span::styled(
            "Image reference to pull:",
            Style::default().fg(app.theme.primary),
        )),
        Line::from(""),
        Line::from(vec![
            Span::raw("  "),
            Span::styled(&app.prompt_buf, Style::default().fg(app.theme.warning)),
            Span::styled("█", Style::default().fg(app.theme.warning)),
        ]),
        Line::from(""),
        Line::from(Span::styled(
            format!("Enter pull · Esc cancel · {recents_hint}"),
            Style::default().fg(app.theme.muted),
        )),
    ])
    .block(
        Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(app.theme.accent))
            .title(" Pull image "),
    );
    f.render_widget(body, r);
}

fn draw_help_overlay(f: &mut Frame, app: &App, area: Rect) {
    let r = centered(area, 70, 80);
    f.render_widget(Clear, r);

    let mut lines: Vec<Line> = Vec::new();
    let h = |k: &str, d: &str| -> Line<'static> {
        Line::from(vec![
            Span::styled(
                format!("  {k:<14}"),
                Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD),
            ),
            Span::raw(d.to_string()),
        ])
    };
    let section = |title: &str| -> Line<'static> {
        Line::from(Span::styled(
            format!(" {title} "),
            Style::default()
                .fg(Color::Black)
                .bg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ))
    };

    lines.push(section("Global"));
    lines.push(h("q / Esc", "Quit (or clear filter/search if set)"));
    lines.push(h("Tab / →", "Next tab"));
    lines.push(h("Shift-Tab / ←", "Prev tab"));
    lines.push(h("↑ ↓ / j", "Move selection"));
    lines.push(h("Enter", "Inspect (open detail pane)"));
    lines.push(h("/", "Filter (Logs: search-as-you-type)"));
    lines.push(h("o", "Cycle sort key for current tab"));
    lines.push(h("r", "Refresh"));
    lines.push(h("a", "Toggle show-all vs running-only"));
    lines.push(h("?", "Toggle this help"));
    lines.push(h("X", "Switch runtime profile (container/docker/…)"));
    lines.push(h("U", "View available updates (when chip is shown)"));
    lines.push(h("Mouse L/R", "Click row · right-click for menu · wheel scrolls"));
    lines.push(Line::from(""));

    match app.tab {
        Tab::Containers => {
            lines.push(section("Containers"));
            lines.push(h("Space", "Mark / unmark for batch ops"));
            lines.push(h("s / x / K / d", "Start / stop / kill / delete"));
            lines.push(h("l", "Load logs into Logs tab"));
            lines.push(h("e", "Exec /bin/sh in selected container"));
            lines.push(h("F", "Follow logs (live stream into Logs tab)"));
        }
        Tab::Images => {
            lines.push(section("Images"));
            lines.push(h("p", "Pull image (prompt + progress modal)"));
            lines.push(h("b", "Build image (Ctrl-O opens file picker)"));
            lines.push(h("T", "Scan with trivy (HIGH+CRITICAL only)"));
            lines.push(h("P", "Re-attach to backgrounded pull/build/scan"));
        }
        Tab::Volumes => {
            lines.push(section("Volumes"));
            lines.push(h("Enter", "Detail: capacity, on-disk, fill bar, JSON"));
        }
        Tab::Networks => {
            lines.push(section("Networks"));
            lines.push(h("Enter", "Detail: subnet/gateway/nameservers + JSON"));
        }
        Tab::Stacks => {
            lines.push(section("Stacks"));
            lines.push(h("u", "Up — `container run -d` per service in topo order"));
            lines.push(h("D", "Down — stop+delete every service container"));
            lines.push(h("Enter", "Detail: source + services + restart + health"));
            lines.push(h("l", "Logs of the stack's first service"));
            lines.push(h("L", "Multi-follow logs from EVERY service (prefixed)"));
            lines.push(h("n", "New stack (template + open in $EDITOR)"));
            lines.push(h("E", "Edit selected stack in $EDITOR"));
            lines.push(h("auto", "Stack files reload on disk change (FSEvents)"));
            lines.push(h("auto", "restart=always|on-failure re-runs stopped svcs"));
            lines.push(h("auto", "[service.healthcheck] kind=tcp|cmd · interval_s"));
        }
        Tab::Logs => {
            lines.push(section("Logs"));
            lines.push(h("/", "Search-as-you-type (highlight matches)"));
            lines.push(h("Ctrl-R", "Toggle regex search mode"));
            lines.push(h("F", "Toggle follow stream on/off"));
            lines.push(h("Esc", "Clear search"));
        }
    }
    lines.push(Line::from(""));
    lines.push(section("Pull modal"));
    lines.push(h("Esc", "Background the modal (status bar chip + P to re-attach)"));
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        "  Press ? or Esc to close",
        Style::default().fg(Color::DarkGray),
    )));

    let p = Paragraph::new(lines).wrap(Wrap { trim: false }).block(
        Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::Cyan))
            .title(Span::styled(
                format!(" cgui · help · {} ", app.tab.label()),
                Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD),
            )),
    );
    f.render_widget(p, r);
}

fn draw_pull_overlay(f: &mut Frame, app: &App, area: Rect) {
    let r = centered(area, 80, 60);
    f.render_widget(Clear, r);

    let lines = match app.pull_log.lock() {
        Ok(v) => v.clone(),
        Err(_) => vec!["<lock poisoned>".to_string()],
    };
    let progress = pullprog::parse_progress(&lines);
    let status_line = pullprog::status_label(&lines);

    let participle = app.op_kind.participle();
    let done = app.op_kind.done();
    let title = match (&app.pull_reference, app.pull_running) {
        (Some(r), true) => format!(" {participle} {r} · Esc backgrounds (P re-attach) "),
        (Some(r), false) => format!(" {done} {r} · Esc closes "),
        (None, true) => format!(" {participle}… · Esc backgrounds (P re-attach) "),
        (None, false) => format!(" {done} · Esc closes "),
    };
    let border_color = if app.pull_running { app.theme.warning } else { app.theme.success };

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(border_color))
        .title(Span::styled(title, Style::default().fg(border_color).add_modifier(Modifier::BOLD)));
    let inner = block.inner(r);
    f.render_widget(block, r);

    // Reserve top 3 rows for the gauge, the rest for the stream.
    let split = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(3), Constraint::Min(1)])
        .split(inner);

    // --- Gauge ---
    let pct = progress.unwrap_or(0.0);
    let gauge_label = match (progress, app.pull_running) {
        (Some(p), _) => format!("{:>5.1}% — {}", p * 100.0, truncate(&status_line, inner.width.saturating_sub(20) as usize)),
        (None, true) => format!("…  {}", truncate(&status_line, inner.width.saturating_sub(8) as usize)),
        (None, false) => "done".to_string(),
    };
    let gauge_color = if !app.pull_running || pct >= 0.66 {
        app.theme.success
    } else if pct >= 0.33 {
        app.theme.warning
    } else {
        app.theme.accent
    };
    let g = Gauge::default()
        .block(Block::default().borders(Borders::BOTTOM).border_style(Style::default().fg(Color::DarkGray)))
        .gauge_style(Style::default().fg(gauge_color).bg(Color::Black))
        .ratio(if app.pull_running { pct } else { 1.0 })
        .label(Span::styled(gauge_label, Style::default().fg(Color::White).add_modifier(Modifier::BOLD)));
    f.render_widget(g, split[0]);

    // --- Stream body ---
    // When the user has scrolled (op_scroll > 0), show from the top with that
    // offset. Otherwise auto-tail to the last screenful so a long pull keeps
    // the latest line visible without extra interaction.
    let h = split[1].height as usize;
    let body = if app.op_scroll == 0 {
        let start = lines.len().saturating_sub(h);
        lines[start..].join("\n")
    } else {
        lines.join("\n")
    };
    let p = Paragraph::new(body)
        .wrap(Wrap { trim: false })
        .scroll((app.op_scroll, 0));
    f.render_widget(p, split[1]);
}

fn draw_build_prompt_overlay(f: &mut Frame, app: &App, area: Rect) {
    let r = centered(area, 70, 30);
    f.render_widget(Clear, r);
    let label = |text: &str, active: bool| -> Span<'static> {
        Span::styled(
            text.to_string(),
            if active {
                Style::default().fg(app.theme.warning).add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(app.theme.muted)
            },
        )
    };
    let cursor_for = |i: u8| if app.build_field == i { "█" } else { " " };
    let body = Paragraph::new(vec![
        Line::from(label("Build context (path or URL):", app.build_field == 0)),
        Line::from(vec![
            Span::raw("  "),
            Span::styled(&app.build_path, Style::default().fg(app.theme.warning)),
            Span::styled(cursor_for(0), Style::default().fg(app.theme.warning)),
        ]),
        Line::from(""),
        Line::from(label("Tag (optional, e.g. myapp:latest):", app.build_field == 1)),
        Line::from(vec![
            Span::raw("  "),
            Span::styled(&app.build_tag, Style::default().fg(app.theme.warning)),
            Span::styled(cursor_for(1), Style::default().fg(app.theme.warning)),
        ]),
        Line::from(""),
        Line::from(Span::styled(
            {
                let n = app.prefs.recent_builds.len();
                let recents_hint = if n > 0 {
                    let pos = match app.recent_idx {
                        Some(i) => format!("{}/{n}", i + 1),
                        None => "—".into(),
                    };
                    format!("↑↓ recent ({pos})")
                } else {
                    "(no recent builds)".into()
                };
                format!("Tab switches fields · ^O file picker · Enter starts · Esc cancels · {recents_hint}")
            },
            Style::default().fg(app.theme.muted),
        )),
    ])
    .block(
        Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(app.theme.accent))
            .title(Span::styled(
                " Build image ",
                Style::default().fg(app.theme.accent).add_modifier(Modifier::BOLD),
            )),
    );
    f.render_widget(body, r);
}

fn draw_context_menu(f: &mut Frame, app: &App, area: Rect) {
    let Some(menu) = &app.context_menu else { return };
    let width: u16 = (menu
        .items
        .iter()
        .map(|(l, _)| l.chars().count())
        .max()
        .unwrap_or(10) as u16)
        .saturating_add(4);
    let height: u16 = (menu.items.len() as u16).saturating_add(2);
    // Anchor near the click but keep on-screen.
    let x = menu.x.min(area.width.saturating_sub(width));
    let y = menu.y.min(area.height.saturating_sub(height));
    let r = Rect { x, y, width, height };
    f.render_widget(Clear, r);

    let lines: Vec<Line> = menu
        .items
        .iter()
        .enumerate()
        .map(|(i, (label, _))| {
            let is_sel = i == menu.selected;
            let style = if is_sel {
                Style::default()
                    .fg(Color::Black)
                    .bg(app.theme.accent)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(app.theme.primary)
            };
            Line::from(Span::styled(format!(" {label} "), style))
        })
        .collect();
    let p = Paragraph::new(lines).block(
        Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(app.theme.accent)),
    );
    f.render_widget(p, r);
}

fn truncate(s: &str, max: usize) -> String {
    if max == 0 || s.chars().count() <= max {
        return s.to_string();
    }
    let mut out: String = s.chars().take(max.saturating_sub(1)).collect();
    out.push('…');
    out
}

fn draw_stack_name_prompt(f: &mut Frame, app: &App, area: Rect) {
    let r = centered(area, 60, 20);
    f.render_widget(Clear, r);
    let body = Paragraph::new(vec![
        Line::from(Span::styled(
            "New stack name (TOML filename, no extension):",
            Style::default().fg(app.theme.primary),
        )),
        Line::from(""),
        Line::from(vec![
            Span::raw("  "),
            Span::styled(&app.prompt_buf, Style::default().fg(app.theme.warning)),
            Span::styled("█", Style::default().fg(app.theme.warning)),
        ]),
        Line::from(""),
        Line::from(Span::styled(
            "Enter create + open in $EDITOR · Esc cancel",
            Style::default().fg(app.theme.muted),
        )),
    ])
    .block(
        Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(app.theme.accent))
            .title(" New stack "),
    );
    f.render_widget(body, r);
}

fn draw_trivy_result(f: &mut Frame, app: &App, area: Rect) {
    let r = centered(area, 90, 80);
    f.render_widget(Clear, r);
    let Some(report) = &app.trivy_report else {
        let p = Paragraph::new("No trivy report parsed.").block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(app.theme.muted))
                .title(" Trivy "),
        );
        f.render_widget(p, r);
        return;
    };

    let filter_label = match app.trivy_filter {
        None => "all".to_string(),
        Some(s) => s.label().to_lowercase(),
    };
    let search_label = if app.trivy_search.is_empty() {
        "—".to_string()
    } else {
        app.trivy_search.clone()
    };
    let title = format!(
        " Trivy · {} · sev:{filter_label} · search:{search_label} · 1-4 0 / Esc ",
        report.artifact
    );
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(app.theme.accent))
        .title(Span::styled(
            title,
            Style::default().fg(app.theme.accent).add_modifier(Modifier::BOLD),
        ));
    let inner = block.inner(r);
    f.render_widget(block, r);

    // Top stripe (counts), optional search input row, then the table.
    let constraints: Vec<Constraint> = if app.trivy_search_active {
        vec![Constraint::Length(3), Constraint::Length(1), Constraint::Min(1)]
    } else {
        vec![Constraint::Length(3), Constraint::Min(1)]
    };
    let split = Layout::default()
        .direction(Direction::Vertical)
        .constraints(constraints)
        .split(inner);

    // Severity counts row. Active filter chip gets a bright underline.
    let counts = report.counts();
    let mut count_spans: Vec<Span> = Vec::with_capacity(counts.len() * 2);
    for (sev, n) in counts {
        if n == 0 && sev != Severity::Critical && sev != Severity::High {
            continue;
        }
        let (fg, bg) = severity_colors(app, sev);
        let chip = format!(" {} {n} ", sev.label());
        let mut style = Style::default().fg(fg).bg(bg).add_modifier(Modifier::BOLD);
        if app.trivy_filter == Some(sev) {
            style = style.add_modifier(Modifier::UNDERLINED);
        }
        count_spans.push(Span::styled(chip, style));
        count_spans.push(Span::raw(" "));
    }
    let header = Paragraph::new(Line::from(count_spans)).block(
        Block::default()
            .borders(Borders::BOTTOM)
            .border_style(Style::default().fg(app.theme.muted)),
    );
    f.render_widget(header, split[0]);

    // Optional search input row.
    let table_idx = if app.trivy_search_active {
        let bar = Paragraph::new(Line::from(vec![
            Span::styled(" /", Style::default().fg(app.theme.warning)),
            Span::raw(&app.trivy_search),
            Span::styled("█", Style::default().fg(app.theme.warning)),
            Span::styled(
                "   (Enter keep · Esc clear · Backspace)",
                Style::default().fg(app.theme.muted),
            ),
        ]));
        f.render_widget(bar, split[1]);
        2
    } else {
        1
    };

    // Findings table.
    if report.findings.is_empty() {
        let body = Paragraph::new(Line::from(Span::styled(
            "no HIGH or CRITICAL findings ✓",
            Style::default().fg(app.theme.success).add_modifier(Modifier::BOLD),
        )));
        f.render_widget(body, split[table_idx]);
        return;
    }
    let header_row = Row::new(vec!["SEV", "CVE", "PKG", "INSTALLED", "FIXED", "TITLE"])
        .style(header_style());
    let needle = app.trivy_search.to_lowercase();
    let rows: Vec<Row> = report
        .findings
        .iter()
        .filter(|f| match app.trivy_filter {
            Some(s) => f.severity == s,
            None => true,
        })
        .filter(|f| {
            if needle.is_empty() {
                return true;
            }
            f.id.to_lowercase().contains(&needle)
                || f.pkg.to_lowercase().contains(&needle)
                || f.title.to_lowercase().contains(&needle)
        })
        .map(|fnd| {
            let (fg, _) = severity_colors(app, fnd.severity);
            let sev_cell = Cell::from(fnd.severity.label())
                .style(Style::default().fg(fg).add_modifier(Modifier::BOLD));
            let fixed = if fnd.fixed.is_empty() { "—".into() } else { fnd.fixed.clone() };
            Row::new(vec![
                sev_cell,
                Cell::from(fnd.id.clone()),
                Cell::from(fnd.pkg.clone()).style(Style::default().fg(app.theme.info)),
                Cell::from(fnd.installed.clone()).style(Style::default().fg(app.theme.muted)),
                Cell::from(fixed).style(Style::default().fg(app.theme.success)),
                Cell::from(fnd.title.clone()),
            ])
        })
        .skip(app.trivy_scroll as usize)
        .collect();
    let widths = [
        Constraint::Length(9),
        Constraint::Length(18),
        Constraint::Length(18),
        Constraint::Length(14),
        Constraint::Length(14),
        Constraint::Min(10),
    ];
    let table = Table::new(rows, widths).header(header_row);
    f.render_widget(table, split[table_idx]);
}

fn severity_colors(app: &App, s: Severity) -> (Color, Color) {
    match s {
        Severity::Critical => (Color::White, app.theme.danger),
        Severity::High => (Color::Black, app.theme.warning),
        Severity::Medium => (Color::Black, app.theme.info),
        Severity::Low => (Color::Black, app.theme.muted),
        Severity::Unknown => (Color::White, Color::DarkGray),
    }
}

fn draw_update_prompt(f: &mut Frame, app: &App, area: Rect) {
    let r = centered(area, 80, 75);
    f.render_widget(Clear, r);

    let visible = app.visible_updates();
    let total = visible.len();
    let Some(u) = app.current_update() else {
        let p = Paragraph::new("No updates to view.").block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(app.theme.muted))
                .title(" Update "),
        );
        f.render_widget(p, r);
        return;
    };

    let asset_hint = match &u.asset {
        Some(a) => format!("D download ({} MiB) · ", a.size / 1024 / 1024),
        None => String::new(),
    };
    let title = format!(
        " Update · {} · {}/{} · {asset_hint}O open · L later · ←→ · Esc ",
        u.component.label(),
        app.update_modal_idx.min(total.saturating_sub(1)) + 1,
        total
    );
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(app.theme.info))
        .title(Span::styled(
            title,
            Style::default().fg(app.theme.info).add_modifier(Modifier::BOLD),
        ));
    let inner = block.inner(r);
    f.render_widget(block, r);

    // Header (versions + url + published) on top, notes pane below.
    let split = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(6), Constraint::Min(1)])
        .split(inner);

    let header = Paragraph::new(vec![
        Line::from(vec![
            Span::styled("Component:  ", Style::default().fg(app.theme.muted)),
            Span::styled(
                u.component.label(),
                Style::default().fg(app.theme.info).add_modifier(Modifier::BOLD),
            ),
        ]),
        Line::from(vec![
            Span::styled("Installed:  ", Style::default().fg(app.theme.muted)),
            Span::styled(u.installed.clone(), Style::default().fg(app.theme.warning)),
            Span::raw("    "),
            Span::styled("Latest:  ", Style::default().fg(app.theme.muted)),
            Span::styled(
                u.latest.clone(),
                Style::default().fg(app.theme.success).add_modifier(Modifier::BOLD),
            ),
            Span::raw("    "),
            Span::styled("Published:  ", Style::default().fg(app.theme.muted)),
            Span::styled(
                short_date(&u.published_at),
                Style::default().fg(app.theme.primary),
            ),
        ]),
        Line::from(vec![
            Span::styled("URL:        ", Style::default().fg(app.theme.muted)),
            Span::styled(
                u.release_url.clone(),
                Style::default().fg(app.theme.accent).add_modifier(Modifier::UNDERLINED),
            ),
        ]),
        Line::from(""),
        Line::from(Span::styled(
            "release notes:",
            Style::default().fg(app.theme.muted),
        )),
    ])
    .block(
        Block::default()
            .borders(Borders::BOTTOM)
            .border_style(Style::default().fg(app.theme.muted)),
    );
    f.render_widget(header, split[0]);

    let notes = if u.notes.is_empty() {
        "(no release notes)".to_string()
    } else {
        u.notes.clone()
    };
    let body = Paragraph::new(notes)
        .wrap(Wrap { trim: false })
        .scroll((app.update_notes_scroll, 0));
    f.render_widget(body, split[1]);
}

fn short_date(s: &str) -> String {
    // "2026-04-30T17:55:36Z" → "2026-04-30"
    s.split('T').next().unwrap_or(s).to_string()
}

fn short_digest(d: &str) -> String {
    d.split(':').nth(1).map(|s| s[..s.len().min(12)].to_string()).unwrap_or_default()
}

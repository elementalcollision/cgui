//! Render the App. Pure ratatui — no I/O.

use crate::app::{App, Mode, Tab};
use humansize::{format_size, BINARY};
use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{
        Block, Borders, Cell, Clear, Paragraph, Row, Sparkline, Table, TableState, Tabs, Wrap,
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

    draw_tabs(f, app, outer[0]);
    draw_sparklines(f, app, outer[1]);
    draw_body(f, app, outer[2]);
    draw_filter_bar(f, app, outer[3]);
    draw_status(f, app, outer[4]);

    // Overlays.
    match app.mode {
        Mode::Detail => draw_detail_overlay(f, app, area),
        Mode::PromptPull => draw_prompt_overlay(f, app, area),
        Mode::PullProgress => draw_pull_overlay(f, app, area),
        Mode::Browse | Mode::Filter => {}
    }
}

fn draw_tabs(f: &mut Frame, app: &App, area: Rect) {
    let titles: Vec<Line> = Tab::ALL
        .iter()
        .map(|t| Line::from(Span::styled(t.label(), Style::default().fg(Color::White))))
        .collect();
    let idx = Tab::ALL.iter().position(|t| *t == app.tab).unwrap_or(0);
    let tabs = Tabs::new(titles)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(Span::styled(
                    " cgui · Apple container front end ",
                    Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD),
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
    let header = Row::new(vec!["ID", "IMAGE", "STATUS", "CPUS", "MEM", "PORTS"])
        .style(header_style());
    let view = app.view_indices();
    let rows: Vec<Row> = view
        .iter()
        .map(|&i| {
            let c = &app.containers[i];
            let status_style = match c.status.as_str() {
                "running" => Style::default().fg(Color::Green).add_modifier(Modifier::BOLD),
                "stopped" | "exited" => Style::default().fg(Color::Red),
                _ => Style::default().fg(Color::Yellow),
            };
            Row::new(vec![
                Cell::from(c.id.clone()),
                Cell::from(c.image.clone()).style(Style::default().fg(Color::Blue)),
                Cell::from(c.status.clone()).style(status_style),
                Cell::from(c.cpus.to_string()),
                Cell::from(format_size(c.memory_bytes, BINARY)),
                Cell::from(c.ports.join(", ")),
            ])
        })
        .collect();
    let widths = [
        Constraint::Percentage(22),
        Constraint::Percentage(36),
        Constraint::Length(10),
        Constraint::Length(5),
        Constraint::Length(12),
        Constraint::Min(10),
    ];
    let title = block_title(app, "Containers", app.containers.len(), rows.len());
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

fn draw_logs(f: &mut Frame, app: &App, area: Rect) {
    let title = match &app.log_target {
        Some(id) => format!(" Logs · {id} (l on Containers tab to load) "),
        None => " Logs (select a container, press l) ".to_string(),
    };
    let body = if app.logs.is_empty() {
        "No logs loaded.".to_string()
    } else {
        app.logs.clone()
    };
    let p = Paragraph::new(body)
        .wrap(Wrap { trim: false })
        .block(Block::default().borders(Borders::ALL).title(title));
    f.render_widget(p, area);
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
    } else if !app.filter.is_empty() {
        let p = Paragraph::new(Line::from(vec![
            Span::styled(" filter: ", Style::default().fg(Color::DarkGray)),
            Span::styled(&app.filter, Style::default().fg(Color::Yellow)),
            Span::styled("   (/ edit · Esc clear)", Style::default().fg(Color::DarkGray)),
        ]));
        f.render_widget(p, area);
    }
}

fn draw_status(f: &mut Frame, app: &App, area: Rect) {
    let p = Paragraph::new(Line::from(vec![Span::styled(
        format!(" {} ", app.status),
        Style::default().fg(Color::Black).bg(Color::White),
    )]));
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
    let p = Paragraph::new(app.detail.clone())
        .wrap(Wrap { trim: false })
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
    let r = centered(area, 60, 20);
    f.render_widget(Clear, r);
    let body = Paragraph::new(vec![
        Line::from(Span::styled(
            "Image reference to pull:",
            Style::default().fg(Color::White),
        )),
        Line::from(""),
        Line::from(vec![
            Span::raw("  "),
            Span::styled(&app.prompt_buf, Style::default().fg(Color::Yellow)),
            Span::styled("█", Style::default().fg(Color::Yellow)),
        ]),
        Line::from(""),
        Line::from(Span::styled(
            "Enter pull · Esc cancel",
            Style::default().fg(Color::DarkGray),
        )),
    ])
    .block(
        Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::Cyan))
            .title(" Pull image "),
    );
    f.render_widget(body, r);
}

fn draw_pull_overlay(f: &mut Frame, app: &App, area: Rect) {
    let r = centered(area, 80, 60);
    f.render_widget(Clear, r);
    let lines = match app.pull_log.lock() {
        Ok(v) => v.clone(),
        Err(_) => vec!["<lock poisoned>".to_string()],
    };
    let total = lines.len();
    // Show last N lines that fit.
    let h = r.height.saturating_sub(2) as usize;
    let start = total.saturating_sub(h);
    let body = lines[start..].join("\n");
    let title = if app.pull_running {
        format!(" Pulling… · {total} lines · Esc hide ")
    } else {
        format!(" Pull complete · {total} lines · Esc close ")
    };
    let p = Paragraph::new(body)
        .wrap(Wrap { trim: false })
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(if app.pull_running {
                    Color::Yellow
                } else {
                    Color::Green
                }))
                .title(title),
        );
    f.render_widget(p, r);
}

fn short_digest(d: &str) -> String {
    d.split(':').nth(1).map(|s| s[..s.len().min(12)].to_string()).unwrap_or_default()
}

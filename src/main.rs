mod app;
mod cli;
mod compose;
mod container;
mod doctor;
mod jsonhl;
mod prefs;
mod pullprog;
mod runtime;
mod stacks;
mod theme;
mod trivy;
mod ui;
mod update;
mod watcher;

use anyhow::Result;
use clap::Parser;
use crossterm::{
    event::{
        DisableMouseCapture, EnableMouseCapture, Event, EventStream, KeyCode, KeyModifiers,
        MouseButton, MouseEvent, MouseEventKind,
    },
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use futures::StreamExt;
use ratatui::{backend::CrosstermBackend, Terminal};
use std::{io::stdout, time::Duration};
use tokio::time::{interval, MissedTickBehavior};

use crate::app::{App, ContextAction, ContextMenu, Mode, OperationKind, Tab};

#[tokio::main]
async fn main() -> Result<()> {
    let cli = cli::Cli::parse();
    if let Some(code) = cli::dispatch_cli(&cli).await? {
        std::process::exit(code);
    }
    run_tui().await
}

async fn run_tui() -> Result<()> {
    enter_terminal()?;
    let backend = CrosstermBackend::new(stdout());
    let mut term = Terminal::new(backend)?;

    let result = event_loop(&mut term).await;

    leave_terminal()?;
    term.show_cursor()?;
    result
}

fn enter_terminal() -> Result<()> {
    enable_raw_mode()?;
    execute!(stdout(), EnterAlternateScreen, EnableMouseCapture)?;
    Ok(())
}

fn leave_terminal() -> Result<()> {
    disable_raw_mode()?;
    execute!(stdout(), LeaveAlternateScreen, DisableMouseCapture)?;
    Ok(())
}

async fn event_loop<B: ratatui::backend::Backend>(term: &mut Terminal<B>) -> Result<()> {
    let mut app = App::new();
    // First refresh runs inline so the UI has data before the first draw.
    let initial = app::fetch_all(app.show_all).await;
    app.apply_refresh(initial);

    let mut events = EventStream::new();
    let mut tick = interval(Duration::from_millis(2000));
    tick.set_missed_tick_behavior(MissedTickBehavior::Skip);
    let mut redraw = interval(Duration::from_millis(150));
    redraw.set_missed_tick_behavior(MissedTickBehavior::Skip);

    let mut pull_handle: Option<tokio::task::JoinHandle<Result<()>>> = None;
    let mut log_handle: Option<tokio::task::JoinHandle<Result<()>>> = None;
    let mut refresh_handle: Option<tokio::task::JoinHandle<app::RefreshResult>> = None;

    // Background watchers: filesystem events + restart/healthcheck loop.
    let (watch_tx, mut watch_rx) =
        tokio::sync::mpsc::unbounded_channel::<watcher::Event>();
    let _fs_watcher = watcher::spawn_fs_watcher(watch_tx.clone());
    let _bg_handle = watcher::spawn_restart_health(watch_tx.clone());
    let _update_handle = watcher::spawn_update_check(watch_tx.clone());

    while app.running {
        // Reap a finished refresh task — apply its result to App.
        if let Some(h) = refresh_handle.as_ref() {
            if h.is_finished() {
                let h = refresh_handle.take().unwrap();
                if let Ok(r) = h.await {
                    app.apply_refresh(r);
                }
            }
        }
        // Reap a finished follow task and clear the running flag.
        if let Some(h) = log_handle.as_ref() {
            if h.is_finished() {
                let h = log_handle.take().unwrap();
                let _ = h.await;
                app.log_following = false;
            }
        }
        // Reap finished pull task.
        if let Some(h) = pull_handle.as_ref() {
            if h.is_finished() {
                let h = pull_handle.take().unwrap();
                let res = h.await.unwrap_or_else(|e| Err(anyhow::anyhow!("join: {e}")));
                app.pull_running = false;
                let verb = app.op_kind.verb();
                match res {
                    Ok(()) => app.set_status(format!("{verb} complete.")),
                    Err(e) => app.set_status(format!("{verb} failed: {e}")),
                }
                // Trivy: parse the captured JSON body and switch to the
                // result modal automatically. Falls back to leaving the raw
                // op log visible if parsing fails.
                if app.op_kind == OperationKind::Trivy {
                    let json = app.trivy_json.lock().ok().map(|g| g.clone()).unwrap_or_default();
                    if !json.trim().is_empty() {
                        if let Some(report) = trivy::Report::parse(&json) {
                            app.trivy_report = Some(report);
                            app.trivy_scroll = 0;
                            app.mode = Mode::TrivyResult;
                        }
                    }
                }
                if matches!(app.op_kind, OperationKind::StackUp | OperationKind::StackDown) {
                    app.reload_stacks();
                }
                // Queued install: if `[I]` triggered the download and it
                // succeeded with a cached path, run the appropriate install
                // route now (sudo installer for the runtime; atomic-replace
                // for cgui itself). Failure clears the queue silently.
                if app.op_kind == OperationKind::UpdateDownload && app.install_after_download {
                    let path = app.download_result.lock().ok().and_then(|g| g.clone());
                    match (path, app.install_component) {
                        (Some(p), Some(update::Component::AppleContainer)) => {
                            install_pkg(term, &mut app, p).await?;
                        }
                        (Some(p), Some(update::Component::CguiSelf)) => {
                            install_self(&mut app, p).await;
                        }
                        _ => {
                            app.set_status("install cancelled (download failed)");
                            app.install_after_download = false;
                            app.install_component = None;
                            app.install_expected = None;
                        }
                    }
                }
                refresh_now(&mut app).await;
            }
        }

        term.draw(|f| ui::draw(f, &mut app))?;

        tokio::select! {
            _ = tick.tick() => {
                // Spawn refresh as a background task — never block the
                // event loop on a slow `container stats` (the runtime can
                // take ~2s per call when a container is running).
                if refresh_handle.is_none()
                    && matches!(app.mode, Mode::Browse | Mode::Filter | Mode::PullProgress | Mode::Detail)
                {
                    refresh_handle = Some(tokio::spawn(app::fetch_all(app.show_all)));
                }
            }
            _ = redraw.tick() => { /* re-render only */ }
            ev = watch_rx.recv() => {
                if let Some(e) = ev {
                    handle_watcher_event(&mut app, e);
                }
            }
            ev = events.next() => {
                match ev {
                    Some(Ok(Event::Key(k))) => {
                        if k.kind != crossterm::event::KeyEventKind::Press { continue; }
                        handle_key(term, &mut app, &mut pull_handle, &mut log_handle, k.code, k.modifiers).await?;
                    }
                    Some(Ok(Event::Mouse(m))) => handle_mouse(&mut app, m).await,
                    _ => {}
                }
            }
        }
    }
    if let Some(h) = log_handle.take() {
        h.abort();
    }
    if let Some(h) = refresh_handle.take() {
        h.abort();
    }
    app.save_prefs();
    Ok(())
}

/// Run a refresh inline (still bounded by container::run's 8s timeout) and
/// apply the result. Used by key handlers that want fresh data immediately
/// after an action — start/stop/etc. — without waiting for the next tick.
async fn refresh_now(app: &mut App) {
    let r = app::fetch_all(app.show_all).await;
    app.apply_refresh(r);
}

async fn handle_mouse(app: &mut App, m: MouseEvent) {
    // Wheel scroll first — works in any mode that has a scrollable view.
    match m.kind {
        MouseEventKind::ScrollDown => return wheel(app, 3),
        MouseEventKind::ScrollUp => return wheel(app, -3),
        _ => {}
    }

    // Right-click → context menu (browse mode only).
    if let MouseEventKind::Down(MouseButton::Right) = m.kind {
        if app.mode == Mode::Browse {
            open_context_menu(app, m.column, m.row);
        }
        return;
    }

    // From here on, only handle left-clicks.
    if !matches!(m.kind, MouseEventKind::Down(MouseButton::Left)) {
        return;
    }

    // Help overlay dismisses on any click.
    if app.mode == Mode::Help {
        app.mode = Mode::Browse;
        return;
    }
    // Context menu: click an item to activate, click elsewhere to dismiss.
    if app.mode == Mode::ContextMenu {
        let menu_rect = context_menu_rect(app);
        if let (Some(menu), Some(r)) = (app.context_menu.clone(), menu_rect) {
            if hit(r, m.column, m.row) {
                let idx = (m.row.saturating_sub(r.y).saturating_sub(1)) as usize;
                if idx < menu.items.len() {
                    let action = menu.items[idx].1;
                    app.mode = Mode::Browse;
                    app.context_menu = None;
                    invoke_context_action(app, action).await;
                    return;
                }
            }
        }
        app.mode = Mode::Browse;
        app.context_menu = None;
        return;
    }
    // Other overlays swallow left-clicks rather than mis-firing on chrome.
    if matches!(
        app.mode,
        Mode::Detail | Mode::PromptPull | Mode::PromptBuild | Mode::PullProgress
    ) {
        return;
    }

    if let Some(tabs) = app.layout.tabs {
        if hit(tabs, m.column, m.row) {
            if let Some(t) = tab_from_x(tabs, m.column) {
                app.set_tab(t);
            }
            return;
        }
    }
    if let Some(body) = app.layout.body {
        if hit(body, m.column, m.row) {
            let row = (m.row.saturating_sub(body.y)) as usize;
            let n = app.row_count();
            if n > 0 && row < n {
                app.selected = row;
            }
        }
    }
}

fn wheel(app: &mut App, delta: i32) {
    let bump = |s: &mut u16| {
        if delta > 0 {
            *s = s.saturating_add(delta as u16);
        } else {
            *s = s.saturating_sub((-delta) as u16);
        }
    };
    match app.mode {
        Mode::Detail => bump(&mut app.detail_scroll),
        Mode::PullProgress => bump(&mut app.op_scroll),
        Mode::Help | Mode::PromptPull | Mode::PromptBuild | Mode::ContextMenu => {}
        _ => {
            if app.tab == Tab::Logs {
                bump(&mut app.log_scroll);
            }
        }
    }
}

fn open_context_menu(app: &mut App, x: u16, y: u16) {
    let items: Vec<(String, ContextAction)> = match app.tab {
        Tab::Containers => vec![
            ("Inspect".into(), ContextAction::Inspect),
            ("Logs".into(), ContextAction::Logs),
            ("Start".into(), ContextAction::Start),
            ("Stop".into(), ContextAction::Stop),
            ("Kill".into(), ContextAction::Kill),
            ("Delete".into(), ContextAction::Delete),
            ("Exec /bin/sh".into(), ContextAction::Exec),
            ("Refresh".into(), ContextAction::Refresh),
            ("Toggle show-all".into(), ContextAction::ToggleAll),
            ("Help".into(), ContextAction::Help),
        ],
        Tab::Images => vec![
            ("Inspect".into(), ContextAction::Inspect),
            ("Pull image…".into(), ContextAction::Pull),
            ("Trivy scan".into(), ContextAction::TrivyScan),
            ("Delete".into(), ContextAction::Delete),
            ("Refresh".into(), ContextAction::Refresh),
            ("Help".into(), ContextAction::Help),
        ],
        Tab::Volumes | Tab::Networks => vec![
            ("Inspect".into(), ContextAction::Inspect),
            ("Refresh".into(), ContextAction::Refresh),
            ("Help".into(), ContextAction::Help),
        ],
        Tab::Stacks => vec![
            ("Inspect".into(), ContextAction::Inspect),
            ("Up".into(), ContextAction::StackUp),
            ("Down".into(), ContextAction::StackDown),
            ("Refresh".into(), ContextAction::Refresh),
            ("Help".into(), ContextAction::Help),
        ],
        Tab::Logs => vec![
            ("Refresh".into(), ContextAction::Refresh),
            ("Help".into(), ContextAction::Help),
        ],
    };
    // Snap selection to the row under the cursor where useful.
    if let Some(body) = app.layout.body {
        if hit(body, x, y) {
            let row = (y.saturating_sub(body.y)) as usize;
            let n = app.row_count();
            if n > 0 && row < n {
                app.selected = row;
            }
        }
    }
    app.context_menu = Some(ContextMenu {
        x,
        y,
        items,
        selected: 0,
    });
    app.mode = Mode::ContextMenu;
}

fn context_menu_rect(app: &App) -> Option<ratatui::layout::Rect> {
    let area = app.layout.body?; // approximation of total drawable area
    let menu = app.context_menu.as_ref()?;
    let width: u16 = (menu
        .items
        .iter()
        .map(|(l, _)| l.chars().count())
        .max()
        .unwrap_or(10) as u16)
        .saturating_add(4);
    let height: u16 = (menu.items.len() as u16).saturating_add(2);
    let max_x = area.x + area.width;
    let max_y = area.y + area.height;
    let x = menu.x.min(max_x.saturating_sub(width));
    let y = menu.y.min(max_y.saturating_sub(height));
    Some(ratatui::layout::Rect { x, y, width, height })
}

async fn invoke_context_action(app: &mut App, action: ContextAction) {
    match action {
        ContextAction::Inspect => open_detail(app).await,
        ContextAction::Logs => {
            // Context-menu Logs is one-shot; for follow, the user can press F.
            // We don't have a log_handle here, so do an unconditional fetch
            // that the next event-loop iteration will see when it pumps the
            // logs_buf — safe because no follow can be running at this point
            // unless they started one then opened the menu (rare).
            let mut none: Option<tokio::task::JoinHandle<Result<()>>> = None;
            load_logs(app, &mut none).await;
        }
        ContextAction::Start => batch_action(app, "start").await,
        ContextAction::Stop => batch_action(app, "stop").await,
        ContextAction::Kill => batch_action(app, "kill").await,
        ContextAction::Delete => batch_action(app, "delete").await,
        ContextAction::Exec => app.set_status("exec from menu: press 'e' on the row"),
        ContextAction::Pull => {
            app.prompt_buf.clear();
            app.mode = Mode::PromptPull;
            app.set_status("Type image reference, Enter to pull");
        }
        ContextAction::TrivyScan => app.set_status("trivy scan: bind via T key on the row"),
        ContextAction::StackUp => app.set_status("stack up: press u on the row"),
        ContextAction::StackDown => app.set_status("stack down: press D on the row"),
        ContextAction::Refresh => {
            app.set_status("Refreshing…");
            refresh_now(app).await;
            app.set_status("Refreshed.");
        }
        ContextAction::ToggleAll => {
            app.show_all = !app.show_all;
            app.save_prefs();
            refresh_now(app).await;
        }
        ContextAction::Help => app.mode = Mode::Help,
    }
}

fn hit(r: ratatui::layout::Rect, x: u16, y: u16) -> bool {
    x >= r.x && x < r.x + r.width && y >= r.y && y < r.y + r.height
}

/// Map an x coordinate inside the tab bar to a Tab. ratatui's `Tabs` widget
/// renders each title as " label " (1-space padding on each side) and uses
/// a single-character divider "│" between them. The visible gap of " │ "
/// is just two padding spaces flanking the divider — not three separator
/// columns — so each tab advances `label_len + 2 + 1`.
fn tab_from_x(tabs_rect: ratatui::layout::Rect, x: u16) -> Option<app::Tab> {
    let inside = x.checked_sub(tabs_rect.x.saturating_add(1))?; // skip border
    let mut cursor: u16 = 0;
    for (i, t) in app::Tab::ALL.iter().enumerate() {
        let label_len = t.label().chars().count() as u16 + 2; // " label "
        if inside >= cursor && inside < cursor + label_len {
            return Some(app::Tab::ALL[i]);
        }
        cursor += label_len + 1; // single-char divider
    }
    None
}

async fn handle_key<B: ratatui::backend::Backend>(
    term: &mut Terminal<B>,
    app: &mut App,
    pull_handle: &mut Option<tokio::task::JoinHandle<Result<()>>>,
    log_handle: &mut Option<tokio::task::JoinHandle<Result<()>>>,
    code: KeyCode,
    mods: KeyModifiers,
) -> Result<()> {
    // Mode-specific input first.
    match app.mode.clone() {
        Mode::Filter => {
            match code {
                KeyCode::Esc => {
                    app.filter.clear();
                    app.mode = Mode::Browse;
                    app.selected = 0;
                    app.reset_status();
                }
                KeyCode::Enter => {
                    app.mode = Mode::Browse;
                    app.set_status(format!("filter applied: {}", app.filter));
                }
                KeyCode::Backspace => {
                    app.filter.pop();
                    app.selected = 0;
                }
                KeyCode::Char(c) => {
                    app.filter.push(c);
                    app.selected = 0;
                }
                _ => {}
            }
            return Ok(());
        }
        Mode::PromptPull => {
            match code {
                KeyCode::Esc => {
                    app.prompt_buf.clear();
                    app.recent_idx = None;
                    app.mode = Mode::Browse;
                    app.reset_status();
                }
                KeyCode::Up => {
                    app.cycle_recent_pull(1);
                    return Ok(());
                }
                KeyCode::Down => {
                    app.cycle_recent_pull(-1);
                    return Ok(());
                }
                KeyCode::Enter => {
                    let reference = std::mem::take(&mut app.prompt_buf);
                    app.recent_idx = None;
                    if reference.trim().is_empty() {
                        app.mode = Mode::Browse;
                        app.set_status("pull cancelled (empty reference)");
                        return Ok(());
                    }
                    if let Ok(mut v) = app.pull_log.lock() {
                        v.clear();
                    }
                    app.pull_running = true;
                    app.op_kind = OperationKind::Pull;
                    app.pull_reference = Some(reference.clone());
                    app.op_scroll = 0;
                    app.prefs.push_recent_pull(reference.trim());
                    app.save_prefs();
                    *pull_handle = Some(container::spawn_pull(reference.clone(), app.pull_log.clone()));
                    app.mode = Mode::PullProgress;
                    app.set_status(format!("pulling {reference}…"));
                }
                KeyCode::Backspace => {
                    app.prompt_buf.pop();
                    app.recent_idx = None;
                }
                KeyCode::Char(c) => {
                    app.prompt_buf.push(c);
                    app.recent_idx = None;
                }
                _ => {}
            }
            return Ok(());
        }
        Mode::Detail => {
            match code {
                KeyCode::Esc | KeyCode::Char('q') | KeyCode::Enter => {
                    app.mode = Mode::Browse;
                    app.detail_scroll = 0;
                }
                KeyCode::Down | KeyCode::Char('j') => {
                    app.detail_scroll = app.detail_scroll.saturating_add(1);
                }
                KeyCode::Up | KeyCode::Char('k') => {
                    app.detail_scroll = app.detail_scroll.saturating_sub(1);
                }
                KeyCode::PageDown => {
                    app.detail_scroll = app.detail_scroll.saturating_add(20);
                }
                KeyCode::PageUp => {
                    app.detail_scroll = app.detail_scroll.saturating_sub(20);
                }
                _ => {}
            }
            return Ok(());
        }
        Mode::PullProgress => {
            if matches!(code, KeyCode::Esc | KeyCode::Char('q') | KeyCode::Enter) {
                app.mode = Mode::Browse;
                if app.pull_running {
                    app.set_status("pull running in background · P to re-attach");
                }
            }
            return Ok(());
        }
        Mode::Help => {
            if matches!(code, KeyCode::Esc | KeyCode::Char('q') | KeyCode::Char('?') | KeyCode::Enter) {
                app.mode = Mode::Browse;
            }
            return Ok(());
        }
        Mode::StackDiff => {
            match code {
                KeyCode::Esc | KeyCode::Char('q') | KeyCode::Enter => {
                    app.mode = Mode::Browse;
                    app.stack_diff_scroll = 0;
                }
                KeyCode::Down | KeyCode::Char('j') => {
                    app.stack_diff_scroll = app.stack_diff_scroll.saturating_add(1);
                }
                KeyCode::Up | KeyCode::Char('k') => {
                    app.stack_diff_scroll = app.stack_diff_scroll.saturating_sub(1);
                }
                KeyCode::PageDown => {
                    app.stack_diff_scroll = app.stack_diff_scroll.saturating_add(10);
                }
                KeyCode::PageUp => {
                    app.stack_diff_scroll = app.stack_diff_scroll.saturating_sub(10);
                }
                _ => {}
            }
            return Ok(());
        }
        Mode::UpdatePrompt => {
            let visible_n = app.visible_updates().len();
            match code {
                KeyCode::Esc | KeyCode::Char('q') | KeyCode::Enter => {
                    app.mode = Mode::Browse;
                    app.update_notes_scroll = 0;
                }
                KeyCode::Right | KeyCode::Tab | KeyCode::Char('n') => {
                    if visible_n > 0 {
                        app.update_modal_idx = (app.update_modal_idx + 1) % visible_n;
                        app.update_notes_scroll = 0;
                    }
                }
                KeyCode::Left | KeyCode::BackTab | KeyCode::Char('p') => {
                    if visible_n > 0 {
                        app.update_modal_idx = (app.update_modal_idx + visible_n - 1) % visible_n;
                        app.update_notes_scroll = 0;
                    }
                }
                KeyCode::Down | KeyCode::Char('j') => {
                    app.update_notes_scroll = app.update_notes_scroll.saturating_add(1);
                }
                KeyCode::Up | KeyCode::Char('k') => {
                    app.update_notes_scroll = app.update_notes_scroll.saturating_sub(1);
                }
                KeyCode::PageDown => {
                    app.update_notes_scroll = app.update_notes_scroll.saturating_add(10);
                }
                KeyCode::PageUp => {
                    app.update_notes_scroll = app.update_notes_scroll.saturating_sub(10);
                }
                KeyCode::Char('L') | KeyCode::Char('l') => {
                    if let Some(u) = app.current_update() {
                        let label = u.component.label().to_string();
                        app.dismissed_updates.insert(label.clone());
                        app.set_status(format!("dismissed {label} for this session"));
                    }
                    let new_n = app.visible_updates().len();
                    if new_n == 0 {
                        app.mode = Mode::Browse;
                    } else if app.update_modal_idx >= new_n {
                        app.update_modal_idx = new_n - 1;
                    }
                    app.update_notes_scroll = 0;
                }
                KeyCode::Char('O') | KeyCode::Char('o') => {
                    if let Some(u) = app.current_update() {
                        match std::process::Command::new("open").arg(&u.release_url).status() {
                            Ok(s) if s.success() => app.set_status(format!("opened {}", u.release_url)),
                            Ok(s) => app.set_status(format!("open exited {s}")),
                            Err(e) => app.set_status(format!("open error: {e}")),
                        }
                    }
                }
                KeyCode::Char('D') | KeyCode::Char('d') => {
                    if let Some(u) = app.current_update() {
                        start_update_download(app, pull_handle, &u, false);
                    }
                }
                KeyCode::Char('I') | KeyCode::Char('i') => {
                    if let Some(u) = app.current_update() {
                        match u.component {
                            update::Component::AppleContainer => {
                                // Brew path skips the download: brew owns the asset itself.
                                if update::install_kind() == update::InstallKind::Brew {
                                    app.install_component = Some(u.component);
                                    app.install_expected = Some(u.latest.clone());
                                    install_brew(term, app, u.component).await?;
                                    app.mode = Mode::Browse;
                                    return Ok(());
                                }
                                if u.asset.is_none() {
                                    app.set_status(format!(
                                        "no signed installer asset for {} {}",
                                        u.component.label(),
                                        u.latest
                                    ));
                                    return Ok(());
                                }
                                app.install_component = Some(u.component);
                                app.install_expected = Some(u.latest.clone());
                                app.install_after_download = true;
                                start_update_download(app, pull_handle, &u, true);
                            }
                            update::Component::CguiSelf => {
                                match update::cgui_install_method() {
                                    update::CguiInstallMethod::Brew => {
                                        app.install_component = Some(u.component);
                                        app.install_expected = Some(u.latest.clone());
                                        install_brew(term, app, u.component).await?;
                                        app.mode = Mode::Browse;
                                        return Ok(());
                                    }
                                    update::CguiInstallMethod::Cargo => {
                                        app.set_status(
                                            "cargo-installed cgui — upgrade with `cargo install cgui --force`",
                                        );
                                        return Ok(());
                                    }
                                    update::CguiInstallMethod::Binary => {
                                        if u.asset.is_none() {
                                            app.set_status(format!(
                                                "no binary asset published for cgui {} — `cargo install` from source",
                                                u.latest
                                            ));
                                            return Ok(());
                                        }
                                        app.install_component = Some(u.component);
                                        app.install_expected = Some(u.latest.clone());
                                        app.install_after_download = true;
                                        start_update_download(app, pull_handle, &u, true);
                                    }
                                }
                            }
                        }
                    }
                }
                _ => {}
            }
            return Ok(());
        }
        Mode::PromptStackName => {
            match code {
                KeyCode::Esc => {
                    app.prompt_buf.clear();
                    app.mode = Mode::Browse;
                    app.reset_status();
                }
                KeyCode::Enter => {
                    let name = app.prompt_buf.trim().to_string();
                    if name.is_empty() {
                        app.set_status("create cancelled (empty name)");
                        app.mode = Mode::Browse;
                        return Ok(());
                    }
                    if !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_') {
                        app.set_status("name must be ASCII alphanumeric, '-', or '_'");
                        return Ok(());
                    }
                    match stacks::create_template(&name) {
                        Ok(p) => {
                            app.mode = Mode::Browse;
                            app.set_status(format!("created {} — opening $EDITOR…", p.display()));
                            edit_path(term, app, p).await?;
                            app.reload_stacks();
                        }
                        Err(e) => {
                            app.set_status(format!("create failed: {e}"));
                        }
                    }
                }
                KeyCode::Backspace => {
                    app.prompt_buf.pop();
                }
                KeyCode::Char(c) => {
                    app.prompt_buf.push(c);
                }
                _ => {}
            }
            return Ok(());
        }
        Mode::TrivyResult => {
            use crate::trivy::Severity;
            // Search input mode: characters go into the buffer; nav keys exit.
            if app.trivy_search_active {
                match code {
                    KeyCode::Esc => {
                        app.trivy_search.clear();
                        app.trivy_search_active = false;
                        app.trivy_scroll = 0;
                    }
                    KeyCode::Enter => {
                        app.trivy_search_active = false;
                    }
                    KeyCode::Backspace => {
                        app.trivy_search.pop();
                        app.trivy_scroll = 0;
                    }
                    KeyCode::Char(c) => {
                        app.trivy_search.push(c);
                        app.trivy_scroll = 0;
                    }
                    _ => {}
                }
                return Ok(());
            }
            match code {
                KeyCode::Esc | KeyCode::Char('q') | KeyCode::Enter => {
                    if !app.trivy_search.is_empty() && code == KeyCode::Esc {
                        app.trivy_search.clear();
                        app.trivy_scroll = 0;
                    } else {
                        app.mode = Mode::Browse;
                    }
                }
                KeyCode::Char('/') => {
                    app.trivy_search_active = true;
                    app.trivy_scroll = 0;
                }
                KeyCode::Down | KeyCode::Char('j') => {
                    app.trivy_scroll = app.trivy_scroll.saturating_add(1);
                }
                KeyCode::Up | KeyCode::Char('k') => {
                    app.trivy_scroll = app.trivy_scroll.saturating_sub(1);
                }
                KeyCode::PageDown => {
                    app.trivy_scroll = app.trivy_scroll.saturating_add(10);
                }
                KeyCode::PageUp => {
                    app.trivy_scroll = app.trivy_scroll.saturating_sub(10);
                }
                KeyCode::Char('1') | KeyCode::Char('c') => {
                    app.trivy_filter = Some(Severity::Critical);
                    app.trivy_scroll = 0;
                }
                KeyCode::Char('2') | KeyCode::Char('h') => {
                    app.trivy_filter = Some(Severity::High);
                    app.trivy_scroll = 0;
                }
                KeyCode::Char('3') | KeyCode::Char('m') => {
                    app.trivy_filter = Some(Severity::Medium);
                    app.trivy_scroll = 0;
                }
                KeyCode::Char('4') | KeyCode::Char('l') => {
                    app.trivy_filter = Some(Severity::Low);
                    app.trivy_scroll = 0;
                }
                KeyCode::Char('0') => {
                    app.trivy_filter = None;
                    app.trivy_scroll = 0;
                }
                _ => {}
            }
            return Ok(());
        }
        Mode::ContextMenu => {
            let len = app.context_menu.as_ref().map(|m| m.items.len()).unwrap_or(0);
            match code {
                KeyCode::Esc | KeyCode::Char('q') => {
                    app.mode = Mode::Browse;
                    app.context_menu = None;
                }
                KeyCode::Down | KeyCode::Char('j') => {
                    if let Some(m) = app.context_menu.as_mut() {
                        if !m.items.is_empty() {
                            m.selected = (m.selected + 1).min(m.items.len() - 1);
                        }
                    }
                }
                KeyCode::Up | KeyCode::Char('k') => {
                    if let Some(m) = app.context_menu.as_mut() {
                        if m.selected > 0 {
                            m.selected -= 1;
                        }
                    }
                }
                KeyCode::Enter => {
                    if let Some(m) = app.context_menu.as_ref() {
                        if !m.items.is_empty() && m.selected < len {
                            let action = m.items[m.selected].1;
                            app.mode = Mode::Browse;
                            app.context_menu = None;
                            invoke_context_action(app, action).await;
                        }
                    }
                }
                _ => {}
            }
            return Ok(());
        }
        Mode::PromptBuild => {
            match code {
                KeyCode::Esc => {
                    app.build_path.clear();
                    app.build_tag.clear();
                    app.build_field = 0;
                    app.mode = Mode::Browse;
                    app.reset_status();
                }
                KeyCode::Char('o') if mods.contains(KeyModifiers::CONTROL) => {
                    let start = if app.build_path.is_empty() {
                        std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("/"))
                    } else {
                        std::path::PathBuf::from(&app.build_path)
                    };
                    let start = if start.is_dir() {
                        start
                    } else {
                        std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("/"))
                    };
                    app.picker_load(start);
                    app.mode = Mode::FilePicker;
                    app.set_status("file picker · ↑↓ · Enter descend · . pick · Esc cancel");
                }
                KeyCode::Tab => app.build_field = if app.build_field == 0 { 1 } else { 0 },
                KeyCode::Up => {
                    app.cycle_recent_build(1);
                    return Ok(());
                }
                KeyCode::Down => {
                    app.cycle_recent_build(-1);
                    return Ok(());
                }
                KeyCode::Enter => {
                    let path = app.build_path.trim().to_string();
                    app.recent_idx = None;
                    if path.is_empty() {
                        app.set_status("build cancelled (empty context path)");
                        app.mode = Mode::Browse;
                        return Ok(());
                    }
                    let tag = if app.build_tag.trim().is_empty() {
                        None
                    } else {
                        Some(app.build_tag.trim().to_string())
                    };
                    if let Ok(mut v) = app.pull_log.lock() {
                        v.clear();
                    }
                    app.pull_running = true;
                    app.op_kind = OperationKind::Build;
                    app.pull_reference = Some(tag.clone().unwrap_or_else(|| path.clone()));
                    app.op_scroll = 0;
                    app.prefs.push_recent_build(&path, tag.as_deref());
                    app.save_prefs();
                    *pull_handle = Some(container::spawn_build(path.clone(), tag, app.pull_log.clone()));
                    app.mode = Mode::PullProgress;
                    app.set_status(format!("building {path}…"));
                }
                KeyCode::Backspace => {
                    if app.build_field == 0 {
                        app.build_path.pop();
                    } else {
                        app.build_tag.pop();
                    }
                    app.recent_idx = None;
                }
                KeyCode::Char(c) => {
                    if app.build_field == 0 {
                        app.build_path.push(c);
                    } else {
                        app.build_tag.push(c);
                    }
                    app.recent_idx = None;
                }
                _ => {}
            }
            return Ok(());
        }
        Mode::LogSearch => {
            match code {
                KeyCode::Esc => {
                    app.log_search.clear();
                    app.mode = Mode::Browse;
                    app.reset_status();
                }
                KeyCode::Enter => {
                    app.mode = Mode::Browse;
                    if app.log_search.is_empty() {
                        app.reset_status();
                    } else {
                        let mode = if app.log_search_regex { "regex" } else { "text" };
                        app.set_status(format!("{mode} search: {}", app.log_search));
                    }
                }
                KeyCode::Char('r') if mods.contains(KeyModifiers::CONTROL) => {
                    app.log_search_regex = !app.log_search_regex;
                    app.set_status(if app.log_search_regex {
                        "regex search ON (^R toggles)"
                    } else {
                        "text search (^R for regex)"
                    });
                }
                KeyCode::Backspace => {
                    app.log_search.pop();
                }
                KeyCode::Char(c) => {
                    app.log_search.push(c);
                }
                _ => {}
            }
            return Ok(());
        }
        Mode::FilePicker => {
            match code {
                KeyCode::Esc | KeyCode::Char('q') => {
                    app.mode = Mode::PromptBuild;
                    app.reset_status();
                }
                KeyCode::Down | KeyCode::Char('j') => {
                    let n = app.picker.entries.len();
                    if n > 0 {
                        app.picker.selected = (app.picker.selected + 1).min(n - 1);
                    }
                }
                KeyCode::Up | KeyCode::Char('k') => {
                    if app.picker.selected > 0 {
                        app.picker.selected -= 1;
                    }
                }
                KeyCode::Enter => {
                    if let Some(e) = app.picker.entries.get(app.picker.selected).cloned() {
                        if e.is_dir {
                            let next = if e.name == ".." {
                                app.picker
                                    .path
                                    .parent()
                                    .map(|p| p.to_path_buf())
                                    .unwrap_or(app.picker.path.clone())
                            } else {
                                app.picker.path.join(e.name)
                            };
                            app.picker_load(next);
                        }
                    }
                }
                KeyCode::Char('.') => {
                    app.build_path = app.picker.path.to_string_lossy().into_owned();
                    app.mode = Mode::PromptBuild;
                    app.set_status(format!("context: {}", app.build_path));
                }
                _ => {}
            }
            return Ok(());
        }
        Mode::ProfilePicker => {
            let n = app.profiles.len();
            match code {
                KeyCode::Esc | KeyCode::Char('q') => {
                    app.mode = Mode::Browse;
                    app.reset_status();
                }
                KeyCode::Down | KeyCode::Char('j') => {
                    if n > 0 {
                        app.profile_picker_selected = (app.profile_picker_selected + 1).min(n - 1);
                    }
                }
                KeyCode::Up | KeyCode::Char('k') => {
                    if app.profile_picker_selected > 0 {
                        app.profile_picker_selected -= 1;
                    }
                }
                KeyCode::Enter => {
                    let idx = app.profile_picker_selected;
                    app.select_profile(idx);
                    app.mode = Mode::Browse;
                    refresh_now(app).await;
                }
                _ => {}
            }
            return Ok(());
        }
        Mode::Browse => {}
    }

    // Browse mode.
    match code {
        KeyCode::Char('q') | KeyCode::Esc => {
            if !app.filter.is_empty() {
                app.filter.clear();
                app.selected = 0;
                app.reset_status();
            } else if app.tab == Tab::Logs && !app.log_search.is_empty() {
                app.log_search.clear();
                app.reset_status();
            } else {
                app.running = false;
            }
        }
        KeyCode::Char('c') if mods.contains(KeyModifiers::CONTROL) => app.running = false,
        KeyCode::Tab | KeyCode::Right => app.next_tab(),
        KeyCode::BackTab | KeyCode::Left => app.prev_tab(),
        KeyCode::Down | KeyCode::Char('j') => app.move_down(),
        KeyCode::Up => app.move_up(),
        KeyCode::Char('r') => {
            app.set_status("Refreshing…");
            if app.tab == Tab::Stacks {
                app.reload_stacks();
            }
            refresh_now(app).await;
            app.set_status("Refreshed.");
        }
        KeyCode::Char('a') => {
            app.show_all = !app.show_all;
            app.set_status(if app.show_all {
                "Showing all"
            } else {
                "Showing running only"
            });
            app.save_prefs();
            refresh_now(app).await;
        }
        KeyCode::Char('?') => {
            app.mode = Mode::Help;
        }
        KeyCode::Char('U') => {
            if app.visible_updates().is_empty() {
                app.set_status("no updates available · cgui update for fresh check");
            } else {
                app.update_modal_idx = 0;
                app.update_notes_scroll = 0;
                app.mode = Mode::UpdatePrompt;
            }
        }
        KeyCode::Char('X') => {
            app.profile_picker_selected = app
                .profiles
                .iter()
                .position(|p| p.name == runtime::name())
                .unwrap_or(0);
            app.mode = Mode::ProfilePicker;
            app.set_status("Pick a runtime profile · Enter activate · Esc cancel");
        }
        KeyCode::Char('/') => {
            if app.tab == Tab::Logs {
                app.mode = Mode::LogSearch;
                app.set_status("Search logs…");
            } else {
                app.mode = Mode::Filter;
                app.set_status("Filter…");
            }
        }
        KeyCode::Char('P') => {
            if app.pull_attachable() {
                app.mode = Mode::PullProgress;
                app.set_status("re-attached to pull");
            } else {
                app.set_status("no pull to re-attach");
            }
        }
        KeyCode::Char('o') => {
            app.sort_key = app.sort_key.cycle(app.tab);
            app.selected = 0;
            app.sort_keys
                .insert(app.tab.key().to_string(), app.sort_key.0);
            app.save_prefs();
            app.set_status(format!("sort: {}", app.sort_key.label(app.tab)));
        }
        KeyCode::Enter => open_detail(app).await,
        KeyCode::Char('p') if app.tab == Tab::Images => {
            app.prompt_buf.clear();
            app.mode = Mode::PromptPull;
            app.set_status("Type image reference, Enter to pull");
        }
        KeyCode::Char('b') if app.tab == Tab::Images => {
            app.build_path.clear();
            app.build_tag.clear();
            app.build_field = 0;
            app.mode = Mode::PromptBuild;
            app.set_status("Build context path, then Tab → tag, Enter to start");
        }
        KeyCode::Char('T') if app.tab == Tab::Images => start_trivy(app, pull_handle).await,
        KeyCode::Char('u') if app.tab == Tab::Stacks => start_stack(app, pull_handle, true).await,
        KeyCode::Char('D') if app.tab == Tab::Stacks => start_stack(app, pull_handle, false).await,
        KeyCode::Char('l') if app.tab == Tab::Stacks => stack_logs(app, log_handle).await,
        KeyCode::Char('L') if app.tab == Tab::Stacks => stack_logs_multi(app, log_handle).await,
        KeyCode::Char('n') if app.tab == Tab::Stacks => {
            app.prompt_buf.clear();
            app.mode = Mode::PromptStackName;
            app.set_status("Type a stack name (no extension), Enter to create");
        }
        KeyCode::Char('E') if app.tab == Tab::Stacks => edit_stack(term, app).await?,
        KeyCode::Char('=') if app.tab == Tab::Stacks => stack_diff(app).await,
        KeyCode::Char(' ') if app.tab == Tab::Containers => {
            app.toggle_mark_current_container();
            app.move_down();
        }
        KeyCode::Char('s') if app.tab == Tab::Containers => batch_action(app, "start").await,
        KeyCode::Char('x') if app.tab == Tab::Containers => batch_action(app, "stop").await,
        KeyCode::Char('K') if app.tab == Tab::Containers => batch_action(app, "kill").await,
        KeyCode::Char('d') if app.tab == Tab::Containers => batch_action(app, "delete").await,
        KeyCode::Char('l') if app.tab == Tab::Containers => load_logs(app, log_handle).await,
        KeyCode::Char('F') if app.tab == Tab::Containers => follow_logs(app, log_handle).await,
        KeyCode::Char('F') if app.tab == Tab::Logs => {
            // Toggle: if already following, stop; else start on log_target.
            if app.log_following {
                if let Some(h) = log_handle.take() { h.abort(); }
                app.log_following = false;
                app.set_status("follow stopped");
            } else if let Some(id) = app.log_target.clone() {
                start_follow(app, log_handle, id);
            } else {
                app.set_status("no container selected for follow");
            }
        }
        KeyCode::Char('e') if app.tab == Tab::Containers => exec_shell(term, app).await?,
        _ => {}
    }
    Ok(())
}

/// Run a lifecycle verb against either the marked set (if any) or the
/// currently highlighted row. Aggregates ok/err per id into a one-line status.
async fn batch_action(app: &mut App, verb: &str) {
    let ids = app.target_container_ids();
    if ids.is_empty() {
        app.set_status("No selection.");
        return;
    }
    let n = ids.len();
    if n == 1 {
        app.set_status(format!("{verb} {}…", ids[0]));
    } else {
        app.set_status(format!("{verb} ×{n}…"));
    }

    let mut ok = 0usize;
    let mut errs: Vec<String> = Vec::new();
    for id in &ids {
        let r = match verb {
            "start" => container::start(id).await,
            "stop" => container::stop(id).await,
            "kill" => container::kill(id).await,
            "delete" => container::delete(id).await,
            _ => Ok(()),
        };
        match r {
            Ok(()) => ok += 1,
            Err(e) => errs.push(format!("{id}: {e}")),
        }
    }

    if errs.is_empty() {
        app.set_status(format!("{verb} ok ({ok}/{n})"));
    } else {
        let first = errs.into_iter().next().unwrap_or_default();
        app.set_status(format!("{verb} {ok}/{n} ok · err: {first}"));
    }

    // Drop marks for any ids that no longer exist (deleted) — safest to just
    // clear them all on a successful batch verb so subsequent actions don't
    // accidentally re-target.
    if ok == n && matches!(verb, "delete") {
        app.marked.clear();
    }
    refresh_now(app).await;
}

async fn load_logs(
    app: &mut App,
    log_handle: &mut Option<tokio::task::JoinHandle<Result<()>>>,
) {
    let Some(id) = app.current_container_id() else {
        app.set_status("No selection.");
        return;
    };
    // One-shot fetch supersedes any in-flight follow.
    if let Some(h) = log_handle.take() {
        h.abort();
    }
    app.log_following = false;
    app.set_status(format!("loading logs for {id}…"));
    match container::logs(&id, 500).await {
        Ok(s) => {
            // Push the fetched lines into logs_buf for the unified renderer.
            if let Ok(mut v) = app.logs_buf.lock() {
                v.clear();
                for line in s.lines() {
                    v.push(line.to_string());
                }
            }
            app.logs = s;
            app.log_target = Some(id);
            app.log_scroll = 0;
            app.set_tab(Tab::Logs);
            app.set_status("Logs loaded.");
        }
        Err(e) => app.set_status(format!("logs error: {e}")),
    }
}

async fn follow_logs(
    app: &mut App,
    log_handle: &mut Option<tokio::task::JoinHandle<Result<()>>>,
) {
    let Some(id) = app.current_container_id() else {
        app.set_status("No selection.");
        return;
    };
    start_follow(app, log_handle, id);
    app.set_tab(Tab::Logs);
}

fn start_follow(
    app: &mut App,
    log_handle: &mut Option<tokio::task::JoinHandle<Result<()>>>,
    id: String,
) {
    if let Some(h) = log_handle.take() {
        h.abort();
    }
    if let Ok(mut v) = app.logs_buf.lock() {
        v.clear();
    }
    app.log_target = Some(id.clone());
    app.log_scroll = 0;
    app.log_following = true;
    *log_handle = Some(container::spawn_log_follow(id.clone(), app.logs_buf.clone()));
    app.set_status(format!("following {id} (F to stop)"));
}

fn start_follow_multi(
    app: &mut App,
    log_handle: &mut Option<tokio::task::JoinHandle<Result<()>>>,
    label: String,
    targets: Vec<(String, String)>,
) {
    if let Some(h) = log_handle.take() {
        h.abort();
    }
    if let Ok(mut v) = app.logs_buf.lock() {
        v.clear();
    }
    app.log_target = Some(label.clone());
    app.log_scroll = 0;
    app.log_following = true;
    *log_handle = Some(container::spawn_logs_multi(targets, app.logs_buf.clone()));
    app.set_status(format!("multi-following {label} (F on Logs to stop)"));
}

async fn stack_diff(app: &mut App) {
    let Some(stack) = app.current_stack() else {
        app.set_status("No stack selected.");
        return;
    };
    app.set_status(format!("diffing {}…", stack.name));
    let rows = stacks::diff_against_runtime(&stack).await;
    app.stack_diff_rows = rows;
    app.stack_diff_target = Some(stack.name.clone());
    app.stack_diff_scroll = 0;
    app.mode = Mode::StackDiff;
    app.set_status(format!("diff: {}", stack.name));
}

async fn stack_logs_multi(
    app: &mut App,
    log_handle: &mut Option<tokio::task::JoinHandle<Result<()>>>,
) {
    let Some(stack) = app.current_stack() else {
        app.set_status("No stack selected.");
        return;
    };
    if stack.services.is_empty() {
        app.set_status("Stack has no services.");
        return;
    }
    let targets: Vec<(String, String)> = stack
        .services
        .iter()
        .map(|svc| (svc.name.clone(), stacks::container_name(&stack.name, &svc.name)))
        .collect();
    start_follow_multi(app, log_handle, format!("stack:{}", stack.name), targets);
    app.set_tab(Tab::Logs);
}

async fn open_detail(app: &mut App) {
    let target = match app.tab {
        Tab::Containers => app.current_container_id(),
        Tab::Images => app.current_image_ref(),
        Tab::Networks => app
            .selected_row()
            .and_then(|i| app.networks.get(i).map(|n| n.id.clone())),
        Tab::Volumes => app
            .selected_row()
            .and_then(|i| app.volumes.get(i).map(|v| v.name.clone())),
        Tab::Stacks => app.current_stack().map(|s| s.name.clone()),
        Tab::Logs => None,
    };
    let Some(id) = target else {
        app.set_status("No selection to inspect.");
        return;
    };
    app.set_status(format!("inspecting {id}…"));
    let result = match app.tab {
        Tab::Volumes => container::volume_detail(&id).await,
        Tab::Networks => container::network_detail(&id).await,
        Tab::Stacks => Ok(stack_detail_text(app)),
        _ => container::inspect(&id).await,
    };
    match result {
        Ok(s) => {
            app.detail = s;
            app.detail_scroll = 0;
            app.mode = Mode::Detail;
            app.set_status(format!("inspect {id}"));
        }
        Err(e) => app.set_status(format!("inspect error: {e}")),
    }
}

fn handle_watcher_event(app: &mut App, ev: watcher::Event) {
    match ev {
        watcher::Event::StacksChanged => {
            app.reload_stacks();
        }
        watcher::Event::Health {
            stack,
            service,
            ok,
            message,
        } => {
            app.health.insert(
                (stack, service),
                app::HealthEntry {
                    ok: Some(ok),
                    last_check: Some(std::time::SystemTime::now()),
                    message,
                },
            );
        }
        watcher::Event::Status(s) => {
            app.set_status(s);
        }
        watcher::Event::Updates(v) => {
            if !v.is_empty() {
                let summary = v
                    .iter()
                    .map(|u| format!("{} {}→{}", u.component.label(), u.installed, u.latest))
                    .collect::<Vec<_>>()
                    .join(", ");
                app.set_status(format!("update available: {summary} · cgui doctor"));
            }
            app.updates = v;
        }
    }
}

async fn edit_stack<B: ratatui::backend::Backend>(
    term: &mut Terminal<B>,
    app: &mut App,
) -> Result<()> {
    let Some(stack) = app.current_stack() else {
        app.set_status("No stack selected.");
        return Ok(());
    };
    let Some(path) = stack.source.clone() else {
        app.set_status("Stack has no source path on disk.");
        return Ok(());
    };
    edit_path(term, app, path).await?;
    app.reload_stacks();
    Ok(())
}

async fn edit_path<B: ratatui::backend::Backend>(
    term: &mut Terminal<B>,
    app: &mut App,
    path: std::path::PathBuf,
) -> Result<()> {
    let editor = std::env::var("EDITOR").unwrap_or_else(|_| "vi".into());
    leave_terminal()?;
    println!("\n--- cgui edit → {editor} {} ---\n", path.display());
    // Pass the editor command verbatim through `sh -c` so values like
    // "code -w" or "nvim +12" work.
    let status = std::process::Command::new("sh")
        .arg("-c")
        .arg(format!("{editor} \"{}\"", path.display()))
        .status();
    enter_terminal()?;
    term.clear()?;
    match status {
        Ok(s) if s.success() => app.set_status(format!("edited {}", path.display())),
        Ok(s) => app.set_status(format!("editor exited {s}")),
        Err(e) => app.set_status(format!("editor spawn error: {e}")),
    }
    Ok(())
}

fn stack_detail_text(app: &App) -> String {
    let Some(s) = app.current_stack() else { return "(no stack)".into() };
    let mut out = String::new();
    use std::fmt::Write as _;
    let _ = writeln!(out, "== Stack: {} ==", s.name);
    if let Some(p) = &s.source {
        let _ = writeln!(out, "Source:    {}", p.display());
    }
    let _ = writeln!(out, "Services:  {}", s.services.len());
    let _ = writeln!(out, "\n== Services ==");
    for svc in &s.services {
        let _ = writeln!(out, "\n  {}", svc.name);
        let _ = writeln!(out, "    image:    {}", svc.image);
        if !svc.depends_on.is_empty() {
            let _ = writeln!(out, "    depends:  {}", svc.depends_on.join(", "));
        }
        if let Some(n) = &svc.network {
            let _ = writeln!(out, "    network:  {n}");
        }
        if !svc.ports.is_empty() {
            let _ = writeln!(out, "    ports:    {}", svc.ports.join(", "));
        }
        if !svc.volumes.is_empty() {
            let _ = writeln!(out, "    volumes:  {}", svc.volumes.join(", "));
        }
        if !svc.env.is_empty() {
            let env: Vec<String> = svc.env.iter().map(|(k, v)| format!("{k}={v}")).collect();
            let _ = writeln!(out, "    env:      {}", env.join(", "));
        }
        match svc.restart_policy() {
            stacks::RestartPolicy::No => {}
            p => {
                let _ = writeln!(out, "    restart:  {:?}", p);
            }
        }
        if let Some(hc) = &svc.healthcheck {
            let _ = writeln!(
                out,
                "    health:   {} target={:?} command={:?} interval={}s",
                hc.kind, hc.target, hc.command, hc.interval_s
            );
            if let Some(h) = app.health.get(&(s.name.clone(), svc.name.clone())) {
                let mark = match h.ok {
                    Some(true) => "✓",
                    Some(false) => "✗",
                    None => "·",
                };
                let _ = writeln!(out, "              last: {mark} {}", h.message);
            }
        }
    }
    let _ = writeln!(out, "\n== Run plan (topo) ==");
    for svc in stacks::topo_order(&s) {
        let _ = writeln!(
            out,
            "  → container {}",
            stacks::run_args(&s.name, svc).join(" ")
        );
    }
    out
}

async fn start_trivy(
    app: &mut App,
    pull_handle: &mut Option<tokio::task::JoinHandle<Result<()>>>,
) {
    let Some(image) = app.current_image_ref() else {
        app.set_status("No image selected.");
        return;
    };
    if let Ok(mut v) = app.pull_log.lock() { v.clear(); }
    if let Ok(mut s) = app.trivy_json.lock() { s.clear(); }
    app.trivy_report = None;
    app.trivy_filter = None;
    app.trivy_search.clear();
    app.trivy_search_active = false;
    app.pull_running = true;
    app.op_kind = OperationKind::Trivy;
    app.pull_reference = Some(image.clone());
    app.op_scroll = 0;
    *pull_handle = Some(container::spawn_trivy(
        image.clone(),
        app.pull_log.clone(),
        app.trivy_json.clone(),
    ));
    app.mode = Mode::PullProgress;
    app.set_status(format!("scanning {image}…"));
}

async fn start_stack(
    app: &mut App,
    pull_handle: &mut Option<tokio::task::JoinHandle<Result<()>>>,
    up: bool,
) {
    let Some(stack) = app.current_stack() else {
        app.set_status("No stack selected.");
        return;
    };
    if let Ok(mut v) = app.pull_log.lock() { v.clear(); }
    app.pull_running = true;
    app.op_kind = if up { OperationKind::StackUp } else { OperationKind::StackDown };
    app.pull_reference = Some(stack.name.clone());
    app.op_scroll = 0;
    let handle = if up {
        stacks::spawn_up(stack.clone(), app.pull_log.clone())
    } else {
        stacks::spawn_down(stack.clone(), app.pull_log.clone())
    };
    *pull_handle = Some(handle);
    app.mode = Mode::PullProgress;
    app.set_status(format!(
        "{} {}…",
        if up { "starting" } else { "stopping" },
        stack.name
    ));
}

async fn stack_logs(
    app: &mut App,
    log_handle: &mut Option<tokio::task::JoinHandle<Result<()>>>,
) {
    let Some(stack) = app.current_stack() else {
        app.set_status("No stack selected.");
        return;
    };
    let Some(svc) = stack.services.first() else {
        app.set_status("Stack has no services.");
        return;
    };
    let id = stacks::container_name(&stack.name, &svc.name);
    start_follow(app, log_handle, id);
    app.set_tab(Tab::Logs);
}

/// Kick off an UpdateDownload spawn. `for_install` only changes the status
/// line so the user sees that the download is the first half of an install.
fn start_update_download(
    app: &mut App,
    pull_handle: &mut Option<tokio::task::JoinHandle<Result<()>>>,
    u: &update::UpdateInfo,
    for_install: bool,
) {
    let Some(asset) = u.asset.clone() else {
        app.set_status("no signed installer asset");
        return;
    };
    if let Ok(mut v) = app.pull_log.lock() { v.clear(); }
    if let Ok(mut g) = app.download_result.lock() { *g = None; }
    app.pull_running = true;
    app.op_kind = OperationKind::UpdateDownload;
    app.pull_reference = Some(format!("{} {}", u.component.label(), u.latest));
    app.op_scroll = 0;
    *pull_handle = Some(update::spawn_download(
        asset,
        app.pull_log.clone(),
        app.download_result.clone(),
    ));
    app.mode = Mode::PullProgress;
    app.set_status(if for_install {
        format!("downloading {} {} (will install on completion)…", u.component.label(), u.latest)
    } else {
        format!("downloading {} {}…", u.component.label(), u.latest)
    });
}

/// Run `sudo installer -pkg <pkg> -target /` with the TUI fully suspended,
/// so the sudo password prompt and installer's stderr land on the user's
/// real terminal. Restores the TUI on return and verifies the new version.
async fn install_pkg<B: ratatui::backend::Backend>(
    term: &mut Terminal<B>,
    app: &mut App,
    pkg: std::path::PathBuf,
) -> Result<()> {
    let component = app.install_component.unwrap_or(update::Component::AppleContainer);
    let expected = app.install_expected.clone().unwrap_or_default();
    let argv = update::installer_argv(&pkg);
    let pretty = argv.join(" ");

    leave_terminal()?;
    println!("\n--- cgui install → {pretty} ---");
    println!("--- sudo will prompt for your password ---\n");
    let status = std::process::Command::new(&argv[0])
        .args(&argv[1..])
        .status();
    enter_terminal()?;
    term.clear()?;

    match status {
        Ok(s) if s.success() => verify_post_install(app, component, &expected),
        Ok(s) => app.set_status(format!("installer exited {s} — version unchanged?")),
        Err(e) => app.set_status(format!("installer spawn error: {e}")),
    }
    Ok(())
}

/// Self-update: atomic-replace the running cgui binary with the downloaded
/// asset. The TUI stays up — no terminal teardown is needed because there's
/// no interactive prompt and POSIX rename is instant. After replacement we
/// can't call ourselves to verify (we'd be the old code), so we just tell
/// the user to restart.
async fn install_self(app: &mut App, downloaded: std::path::PathBuf) {
    let expected = app.install_expected.clone().unwrap_or_default();
    match update::install_self_binary(downloaded, app.pull_log.clone()).await {
        Ok(()) => {
            app.set_status(format!(
                "✓ replaced cgui binary — restart to use {expected}"
            ));
            app.updates
                .retain(|u| u.component != update::Component::CguiSelf);
            app.prefs.update_cache.retain(|c| c.component != "cgui");
            app.prefs.save();
        }
        Err(e) => app.set_status(format!("self-update failed: {e}")),
    }
    app.install_after_download = false;
    app.install_component = None;
    app.install_expected = None;
}

/// Brew path: no sudo, no download. Suspends the TUI just so brew's progress
/// (which can be chatty) is visible on the real terminal.
async fn install_brew<B: ratatui::backend::Backend>(
    term: &mut Terminal<B>,
    app: &mut App,
    component: update::Component,
) -> Result<()> {
    let expected = app.install_expected.clone().unwrap_or_default();
    let argv = update::brew_upgrade_argv(component);
    let pretty = argv.join(" ");

    leave_terminal()?;
    println!("\n--- cgui upgrade → {pretty} ---\n");
    let status = std::process::Command::new(&argv[0])
        .args(&argv[1..])
        .status();
    enter_terminal()?;
    term.clear()?;

    match status {
        Ok(s) if s.success() => verify_post_install(app, component, &expected),
        Ok(s) => app.set_status(format!("brew upgrade exited {s}")),
        Err(e) => app.set_status(format!("brew spawn error: {e}")),
    }
    Ok(())
}

/// Re-read the installed version, compare against `expected`, and report.
/// On a confirmed upgrade we drop the matching entry from `app.updates` so
/// the chip vanishes immediately.
fn verify_post_install(
    app: &mut App,
    component: update::Component,
    expected: &str,
) {
    use std::cmp::Ordering;
    match component {
        update::Component::AppleContainer => {
            let installed = std::process::Command::new(crate::runtime::binary())
                .arg("--version")
                .output()
                .ok()
                .and_then(|o| {
                    let s = String::from_utf8_lossy(&o.stdout).into_owned();
                    s.split_whitespace()
                        .find(|t| update::parse_version(t.trim_start_matches('v')).is_some())
                        .map(|s| s.trim_start_matches('v').to_string())
                });
            // Drop the cached release for `container` so the next check (on
            // next launch or `cgui update`) recomputes against fresh data.
            app.prefs.update_cache.retain(|c| c.component != "container");
            app.prefs.save();
            match installed {
                Some(v) => {
                    let cmp = update::compare_versions(&v, expected);
                    if cmp != Ordering::Less {
                        app.set_status(format!("✓ upgraded container to {v}"));
                        app.updates.retain(|u| u.component != component);
                    } else {
                        app.set_status(format!(
                            "⚠ installer ran but container is {v} (expected {expected})"
                        ));
                    }
                }
                None => app.set_status("⚠ post-install version check failed"),
            }
        }
        update::Component::CguiSelf => {
            // cgui self-update happens out-of-process; nothing to verify here.
            app.set_status(format!("brew upgrade cgui finished — restart to pick up {expected}"));
            app.updates.retain(|u| u.component != component);
        }
    }
    app.install_after_download = false;
    app.install_component = None;
    app.install_expected = None;
}

/// Drop into `container exec -ti <id> /bin/sh` for the selected container.
/// We tear the TUI down (leave alt screen, leave raw mode) so the child can
/// own the terminal, then rebuild it on return.
async fn exec_shell<B: ratatui::backend::Backend>(
    term: &mut Terminal<B>,
    app: &mut App,
) -> Result<()> {
    let Some(id) = app.current_container_id() else {
        app.set_status("No selection.");
        return Ok(());
    };

    leave_terminal()?;
    println!("\n--- cgui exec → container exec -ti {id} /bin/sh (Ctrl-D to return) ---\n");

    // Try sh; the user can re-run this if they want bash.
    let status = std::process::Command::new("container")
        .args(["exec", "-ti", &id, "/bin/sh"])
        .status();

    enter_terminal()?;
    term.clear()?;

    match status {
        Ok(s) if s.success() => app.set_status(format!("exec {id}: exited 0")),
        Ok(s) => app.set_status(format!("exec {id}: exited {s}")),
        Err(e) => app.set_status(format!("exec {id}: spawn error: {e}")),
    }
    refresh_now(app).await;
    Ok(())
}

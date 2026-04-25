mod app;
mod cli;
mod container;
mod ui;

use anyhow::Result;
use clap::Parser;
use crossterm::{
    event::{DisableMouseCapture, EnableMouseCapture, Event, EventStream, KeyCode, KeyModifiers},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use futures::StreamExt;
use ratatui::{backend::CrosstermBackend, Terminal};
use std::{io::stdout, time::Duration};
use tokio::time::{interval, MissedTickBehavior};

use crate::app::{App, Mode, Tab};

#[tokio::main]
async fn main() -> Result<()> {
    let cli = cli::Cli::parse();
    if let Some(code) = cli::dispatch_cli(&cli)? {
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
    app.refresh().await.ok();

    let mut events = EventStream::new();
    let mut tick = interval(Duration::from_millis(2000));
    tick.set_missed_tick_behavior(MissedTickBehavior::Skip);
    let mut redraw = interval(Duration::from_millis(150));
    redraw.set_missed_tick_behavior(MissedTickBehavior::Skip);

    let mut pull_handle: Option<tokio::task::JoinHandle<Result<()>>> = None;

    while app.running {
        // Reap finished pull task.
        if let Some(h) = pull_handle.as_ref() {
            if h.is_finished() {
                let h = pull_handle.take().unwrap();
                let res = h.await.unwrap_or_else(|e| Err(anyhow::anyhow!("join: {e}")));
                app.pull_running = false;
                match res {
                    Ok(()) => app.set_status("Pull complete."),
                    Err(e) => app.set_status(format!("Pull failed: {e}")),
                }
                app.refresh().await.ok();
            }
        }

        term.draw(|f| ui::draw(f, &mut app))?;

        tokio::select! {
            _ = tick.tick() => {
                if matches!(app.mode, Mode::Browse | Mode::Filter | Mode::PullProgress | Mode::Detail) {
                    app.refresh().await.ok();
                }
            }
            _ = redraw.tick() => { /* re-render only */ }
            ev = events.next() => {
                if let Some(Ok(Event::Key(k))) = ev {
                    if k.kind != crossterm::event::KeyEventKind::Press { continue; }
                    handle_key(term, &mut app, &mut pull_handle, k.code, k.modifiers).await?;
                }
            }
        }
    }
    Ok(())
}

async fn handle_key<B: ratatui::backend::Backend>(
    term: &mut Terminal<B>,
    app: &mut App,
    pull_handle: &mut Option<tokio::task::JoinHandle<Result<()>>>,
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
                    app.mode = Mode::Browse;
                    app.reset_status();
                }
                KeyCode::Enter => {
                    let reference = std::mem::take(&mut app.prompt_buf);
                    if reference.trim().is_empty() {
                        app.mode = Mode::Browse;
                        app.set_status("pull cancelled (empty reference)");
                        return Ok(());
                    }
                    if let Ok(mut v) = app.pull_log.lock() {
                        v.clear();
                    }
                    app.pull_running = true;
                    *pull_handle = Some(container::spawn_pull(reference.clone(), app.pull_log.clone()));
                    app.mode = Mode::PullProgress;
                    app.set_status(format!("pulling {reference}…"));
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
            app.refresh().await.ok();
            app.set_status("Refreshed.");
        }
        KeyCode::Char('a') => {
            app.show_all = !app.show_all;
            app.set_status(if app.show_all {
                "Showing all"
            } else {
                "Showing running only"
            });
            app.refresh().await.ok();
        }
        KeyCode::Char('/') => {
            app.mode = Mode::Filter;
            app.set_status("Filter…");
        }
        KeyCode::Char('o') => {
            app.sort_key = app.sort_key.cycle(app.tab);
            app.selected = 0;
            app.set_status(format!("sort: {}", app.sort_key.label(app.tab)));
        }
        KeyCode::Enter => open_detail(app).await,
        KeyCode::Char('p') if app.tab == Tab::Images => {
            app.prompt_buf.clear();
            app.mode = Mode::PromptPull;
            app.set_status("Type image reference, Enter to pull");
        }
        KeyCode::Char('s') if app.tab == Tab::Containers => action(app, "start").await,
        KeyCode::Char('x') if app.tab == Tab::Containers => action(app, "stop").await,
        KeyCode::Char('K') if app.tab == Tab::Containers => action(app, "kill").await,
        KeyCode::Char('d') if app.tab == Tab::Containers => action(app, "delete").await,
        KeyCode::Char('l') if app.tab == Tab::Containers => load_logs(app).await,
        KeyCode::Char('e') if app.tab == Tab::Containers => exec_shell(term, app).await?,
        _ => {}
    }
    Ok(())
}

async fn action(app: &mut App, verb: &str) {
    let Some(id) = app.current_container_id() else {
        app.set_status("No selection.");
        return;
    };
    app.set_status(format!("{verb} {id}…"));
    let res = match verb {
        "start" => container::start(&id).await,
        "stop" => container::stop(&id).await,
        "kill" => container::kill(&id).await,
        "delete" => container::delete(&id).await,
        _ => Ok(()),
    };
    match res {
        Ok(()) => app.set_status(format!("{verb} {id}: ok")),
        Err(e) => app.set_status(format!("{verb} {id}: {e}")),
    }
    app.refresh().await.ok();
}

async fn load_logs(app: &mut App) {
    let Some(id) = app.current_container_id() else {
        app.set_status("No selection.");
        return;
    };
    app.set_status(format!("loading logs for {id}…"));
    match container::logs(&id, 500).await {
        Ok(s) => {
            app.logs = s;
            app.log_target = Some(id);
            app.tab = Tab::Logs;
            app.set_status("Logs loaded.");
        }
        Err(e) => app.set_status(format!("logs error: {e}")),
    }
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
        Tab::Logs => None,
    };
    let Some(id) = target else {
        app.set_status("No selection to inspect.");
        return;
    };
    app.set_status(format!("inspecting {id}…"));
    match container::inspect(&id).await {
        Ok(s) => {
            app.detail = s;
            app.detail_scroll = 0;
            app.mode = Mode::Detail;
            app.set_status(format!("inspect {id}"));
        }
        Err(e) => app.set_status(format!("inspect error: {e}")),
    }
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
    app.refresh().await.ok();
    Ok(())
}

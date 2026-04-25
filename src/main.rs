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

use crate::app::{App, Tab};

#[tokio::main]
async fn main() -> Result<()> {
    let cli = cli::Cli::parse();
    if let Some(code) = cli::dispatch_cli(&cli)? {
        std::process::exit(code);
    }
    run_tui().await
}

async fn run_tui() -> Result<()> {
    enable_raw_mode()?;
    let mut out = stdout();
    execute!(out, EnterAlternateScreen, EnableMouseCapture)?;
    let backend = CrosstermBackend::new(out);
    let mut term = Terminal::new(backend)?;

    let result = event_loop(&mut term).await;

    disable_raw_mode()?;
    execute!(
        term.backend_mut(),
        LeaveAlternateScreen,
        DisableMouseCapture
    )?;
    term.show_cursor()?;
    result
}

async fn event_loop<B: ratatui::backend::Backend>(term: &mut Terminal<B>) -> Result<()> {
    let mut app = App::new();
    app.refresh().await.ok();

    let mut events = EventStream::new();
    let mut tick = interval(Duration::from_millis(2000));
    tick.set_missed_tick_behavior(MissedTickBehavior::Skip);
    let mut redraw = interval(Duration::from_millis(150));
    redraw.set_missed_tick_behavior(MissedTickBehavior::Skip);

    while app.running {
        term.draw(|f| ui::draw(f, &mut app))?;

        tokio::select! {
            _ = tick.tick() => {
                app.refresh().await.ok();
            }
            _ = redraw.tick() => { /* re-render only */ }
            ev = events.next() => {
                if let Some(Ok(Event::Key(k))) = ev {
                    if k.kind != crossterm::event::KeyEventKind::Press { continue; }
                    handle_key(&mut app, k.code, k.modifiers).await;
                }
            }
        }
    }
    Ok(())
}

async fn handle_key(app: &mut App, code: KeyCode, mods: KeyModifiers) {
    match code {
        KeyCode::Char('q') | KeyCode::Esc => app.running = false,
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
            app.set_status(if app.show_all { "Showing all" } else { "Showing running only" });
            app.refresh().await.ok();
        }
        KeyCode::Char('s') if app.tab == Tab::Containers => action(app, "start").await,
        KeyCode::Char('x') if app.tab == Tab::Containers => action(app, "stop").await,
        KeyCode::Char('K') if app.tab == Tab::Containers => action(app, "kill").await,
        KeyCode::Char('d') if app.tab == Tab::Containers => action(app, "delete").await,
        KeyCode::Char('l') if app.tab == Tab::Containers => load_logs(app).await,
        _ => {}
    }
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

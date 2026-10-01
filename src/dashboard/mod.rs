//! Interactive dashboard, built as an Elm-style loop: terminal events and
//! background results become `Msg`s, `update` changes `App` and may return a
//! `Cmd`, the `Runtime` executes the `Cmd` off the UI thread, and `view` draws `App`.

mod app;
mod components;
mod config_edit;
mod msg;
mod repo;
mod runtime;
mod state;
mod theme;
mod update;
mod view;

use anyhow::Result;
use crossterm::{
    event::{self, Event},
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
    ExecutableCommand,
};
use ratatui::prelude::*;
use std::io::{stdout, IsTerminal};
use std::time::Duration;

use app::App;
use msg::{Cmd, Msg};
use runtime::Runtime;
use state::DashboardState;

const TICK_RATE: Duration = Duration::from_millis(250);

struct TerminalGuard;

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        let _ = disable_raw_mode();
        let _ = stdout().execute(LeaveAlternateScreen);
    }
}

pub fn run() -> Result<()> {
    if !std::io::stdout().is_terminal() {
        anyhow::bail!(
            "Dashboard requires an interactive terminal. Use 'tether status' for non-interactive output."
        );
    }

    let state = DashboardState::load();
    let deleted = repo::load_deleted_files(&state);
    let mut app = App::new(state, deleted);
    let mut rt = Runtime::new();

    if let Some(config) = app.state.config.clone() {
        rt.execute(Cmd::CollectPackages {
            config: Box::new(config),
            machine_id: app.machine_id().to_string(),
        });
    }

    let _guard = TerminalGuard;
    enable_raw_mode()?;
    stdout().execute(EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout());
    let mut terminal = Terminal::new(backend)?;
    terminal.clear()?;

    while !app.should_quit {
        terminal.draw(|f| view::view(f, &app))?;

        if event::poll(TICK_RATE)? {
            if let Some(msg) = event_to_msg(event::read()?) {
                dispatch(&mut app, &mut rt, msg);
            }
        }
        rt.poll();
        while let Ok(msg) = rt.rx.try_recv() {
            dispatch(&mut app, &mut rt, msg);
        }
        dispatch(&mut app, &mut rt, Msg::Tick);
    }

    rt.shutdown();
    // TerminalGuard handles disable_raw_mode + LeaveAlternateScreen on drop
    Ok(())
}

fn event_to_msg(event: Event) -> Option<Msg> {
    match event {
        Event::Key(key) => Some(Msg::Key(key)),
        _ => None,
    }
}

/// Apply a message, then every message its command and background work have queued.
fn dispatch(app: &mut App, rt: &mut Runtime, msg: Msg) {
    let mut next = Some(msg);
    while let Some(msg) = next {
        if let Some(cmd) = update::update(app, msg) {
            rt.execute(cmd);
        }
        next = rt.rx.try_recv().ok();
    }
}

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
    event::{self, DisableMouseCapture, EnableMouseCapture, Event, MouseEventKind},
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
    ExecutableCommand,
};
use ratatui::prelude::*;
use std::io::{stdout, IsTerminal};
use std::time::{Duration, Instant};

use app::App;
use msg::{Cmd, Msg};
use runtime::Runtime;
use state::DashboardState;

/// Idle wake-up: picks up background results and timers without busy-looping.
const IDLE_POLL: Duration = Duration::from_millis(250);
/// Frame interval while something animates.
const FRAME: Duration = Duration::from_millis(50);
/// Idle redraw, so relative times stay current.
const IDLE_REDRAW: Duration = Duration::from_secs(1);

struct TerminalGuard;

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        let _ = stdout().execute(DisableMouseCapture);
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

    rt.execute(Cmd::LoadActivity);
    if let Some(config) = app.state.config.clone() {
        rt.execute(Cmd::CollectPackages {
            config: Box::new(config),
            machine_id: app.machine_id().to_string(),
        });
    }

    let _guard = TerminalGuard;
    enable_raw_mode()?;
    let setting = app
        .state
        .config
        .as_ref()
        .and_then(|c| c.dashboard.theme.clone());
    app.theme = theme::Theme::select(
        setting.as_deref(),
        theme::truecolor_env(),
        theme::detect_dark_background,
    );
    stdout().execute(EnterAlternateScreen)?;
    stdout().execute(EnableMouseCapture)?;
    let backend = CrosstermBackend::new(stdout());
    let mut terminal = Terminal::new(backend)?;
    terminal.clear()?;
    let size = terminal.size()?;
    app.viewport = Rect::new(0, 0, size.width, size.height);

    // Draw only when something changed, at frame rate while animating.
    let mut dirty = true;
    let mut last_draw = Instant::now();
    while !app.should_quit {
        let animating = app.animating();
        if dirty || animating || last_draw.elapsed() >= IDLE_REDRAW {
            terminal.draw(|f| view::view(f, &app))?;
            dirty = false;
            last_draw = Instant::now();
        }

        let wait = if animating { FRAME } else { IDLE_POLL };
        if event::poll(wait)? {
            // Drain bursts such as wheel scrolls before the next draw.
            loop {
                if let Some(msg) = event_to_msg(event::read()?) {
                    dispatch(&mut app, &mut rt, msg);
                    dirty = true;
                }
                if !event::poll(Duration::ZERO)? {
                    break;
                }
            }
        }
        rt.poll();
        while let Ok(msg) = rt.rx.try_recv() {
            dispatch(&mut app, &mut rt, msg);
            dirty = true;
        }
        dispatch(&mut app, &mut rt, Msg::Tick);
    }

    rt.shutdown();
    // TerminalGuard handles disable_raw_mode + LeaveAlternateScreen on drop
    Ok(())
}

/// Mouse motion is dropped: it would wake the loop for nothing.
fn event_to_msg(event: Event) -> Option<Msg> {
    match event {
        Event::Key(key) if key.kind != event::KeyEventKind::Release => Some(Msg::Key(key)),
        Event::Mouse(m) => match m.kind {
            MouseEventKind::Down(_) | MouseEventKind::ScrollDown | MouseEventKind::ScrollUp => {
                Some(Msg::Mouse(m))
            }
            _ => None,
        },
        Event::Resize(w, h) => Some(Msg::Resize(w, h)),
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

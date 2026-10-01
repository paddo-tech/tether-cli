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
    event::{self, DisableMouseCapture, Event, MouseEventKind},
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
    ExecutableCommand,
};
use ratatui::prelude::*;
use std::io::{stdout, IsTerminal, Write};
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
/// Events handled per pass before the loop draws and polls background work again.
const MAX_DRAIN: usize = 64;
/// Button presses, wheel and drags (1000, 1002) in SGR coordinates (1006). Crossterm's
/// EnableMouseCapture also turns on any-motion tracking (1003), which floods the loop
/// with events while the pointer moves.
const MOUSE_ON: &[u8] = b"\x1b[?1000h\x1b[?1002h\x1b[?1006h";

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
    let mut probe = theme::Probe::default();
    app.theme = theme::Theme::select(setting.as_deref(), theme::truecolor_env(), || {
        probe = theme::probe_background();
        probe.dark
    });
    let mut late_reply = probe
        .late_reply
        .then(|| theme::LateReplyFilter::new(Instant::now(), probe.mid_reply));
    stdout().execute(EnterAlternateScreen)?;
    stdout().write_all(MOUSE_ON)?;
    stdout().flush()?;
    let backend = CrosstermBackend::new(stdout());
    let mut terminal = Terminal::new(backend)?;
    terminal.clear()?;
    let size = terminal.size()?;
    app.viewport = Rect::new(0, 0, size.width, size.height);
    for key in probe.typed {
        dispatch(&mut app, &mut rt, Msg::Key(key));
    }

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
            // Drain bursts such as wheel scrolls before the next draw, up to a limit.
            for _ in 0..MAX_DRAIN {
                let msg = event_to_msg(event::read()?).filter(|msg| match (msg, &mut late_reply) {
                    (Msg::Key(key), Some(filter)) => filter.allow(key, Instant::now()),
                    _ => true,
                });
                if let Some(msg) = msg {
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

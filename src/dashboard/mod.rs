mod config_edit;
mod state;
mod widgets;

use anyhow::Result;
use crossterm::{
    event::{self, Event, KeyCode, KeyModifiers},
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
    ExecutableCommand,
};
use ratatui::prelude::*;
use std::io::{stdout, IsTerminal};
use std::time::{Duration, Instant};

use state::DashboardState;

#[derive(Clone, Copy, PartialEq)]
pub enum Tab {
    Overview,
    Files,
    Packages,
    Machines,
    Config,
}

impl Tab {
    fn title(&self) -> &str {
        match self {
            Tab::Overview => "Overview",
            Tab::Files => "Files",
            Tab::Packages => "Packages",
            Tab::Machines => "Machines",
            Tab::Config => "Config",
        }
    }

    fn all() -> &'static [Tab] {
        &[
            Tab::Overview,
            Tab::Files,
            Tab::Packages,
            Tab::Machines,
            Tab::Config,
        ]
    }
}

#[derive(Clone, Copy, PartialEq)]
enum DaemonOp {
    None,
    Starting,
    Stopping,
}

pub struct App {
    state: DashboardState,
    active_tab: Tab,
    scroll_offsets: [usize; 5],
    should_quit: bool,
    syncing: bool,
    sync_child: Option<std::process::Child>,
    daemon_child: Option<std::process::Child>,
    daemon_op: DaemonOp,
    show_help: bool,
    last_refresh: Instant,
    config_editing: bool,
    config_edit_buf: String,
    config_error: Option<Instant>,
}

impl App {
    fn scroll_offset(&self) -> usize {
        let idx = Tab::all()
            .iter()
            .position(|t| *t == self.active_tab)
            .unwrap_or(0);
        self.scroll_offsets[idx]
    }

    fn scroll_offset_mut(&mut self) -> &mut usize {
        let idx = Tab::all()
            .iter()
            .position(|t| *t == self.active_tab)
            .unwrap_or(0);
        &mut self.scroll_offsets[idx]
    }

    fn item_count(&self) -> usize {
        match self.active_tab {
            Tab::Files => self
                .state
                .sync_state
                .as_ref()
                .map(|s| s.files.len())
                .unwrap_or(0),
            Tab::Packages => self
                .state
                .sync_state
                .as_ref()
                .map(|s| s.packages.len())
                .unwrap_or(0),
            Tab::Machines => self.state.machines.len(),
            Tab::Overview => self
                .state
                .sync_state
                .as_ref()
                .map(|s| s.files.len())
                .unwrap_or(0),
            Tab::Config => config_edit::fields().len(),
        }
    }
}

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
    let mut app = App {
        state,
        active_tab: Tab::Overview,
        scroll_offsets: [0; 5],
        should_quit: false,
        syncing: false,
        sync_child: None,
        daemon_child: None,
        daemon_op: DaemonOp::None,
        show_help: false,
        last_refresh: Instant::now(),
        config_editing: false,
        config_edit_buf: String::new(),
        config_error: None,
    };

    let _guard = TerminalGuard;
    enable_raw_mode()?;
    stdout().execute(EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout());
    let mut terminal = Terminal::new(backend)?;
    terminal.clear()?;

    let tick_rate = Duration::from_millis(250);
    let refresh_interval = Duration::from_secs(30);

    loop {
        terminal.draw(|f| draw(f, &mut app))?;

        if event::poll(tick_rate)? {
            match event::read()? {
                Event::Key(key) => handle_key(&mut app, key),
                Event::Resize(_, _) => {}
                _ => {}
            }
        }

        // Check sync child process
        if let Some(ref mut child) = app.sync_child {
            if let Ok(Some(_)) = child.try_wait() {
                app.syncing = false;
                app.sync_child = None;
                app.state = DashboardState::load();
                app.last_refresh = Instant::now();
            }
        }

        // Check daemon child process
        if let Some(ref mut child) = app.daemon_child {
            if let Ok(Some(_)) = child.try_wait() {
                app.daemon_op = DaemonOp::None;
                app.daemon_child = None;
                app.state = DashboardState::load();
                app.last_refresh = Instant::now();
            }
        }

        if app.last_refresh.elapsed() >= refresh_interval {
            app.state = DashboardState::load();
            app.last_refresh = Instant::now();
        }

        if app.should_quit {
            break;
        }
    }

    if let Some(ref mut child) = app.sync_child {
        let _ = child.kill();
        let _ = child.wait();
    }
    // Don't kill daemon child — let daemon start/stop complete
    if let Some(ref mut child) = app.daemon_child {
        let _ = child.wait();
    }

    // TerminalGuard handles disable_raw_mode + LeaveAlternateScreen on drop
    Ok(())
}

fn handle_key(app: &mut App, key: crossterm::event::KeyEvent) {
    // Ctrl+c always quits
    if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
        app.should_quit = true;
        return;
    }

    // Config edit mode intercepts keys
    if app.config_editing {
        match key.code {
            KeyCode::Esc => {
                app.config_editing = false;
                app.config_edit_buf.clear();
            }
            KeyCode::Enter => {
                let idx = app.scroll_offset();
                let buf = app.config_edit_buf.clone();
                let ok = app
                    .state
                    .config
                    .as_mut()
                    .map(|c| config_edit::set_value(c, idx, &buf))
                    .unwrap_or(false);
                if !ok {
                    app.config_error = Some(Instant::now());
                }
                app.config_editing = false;
                app.config_edit_buf.clear();
            }
            KeyCode::Backspace => {
                app.config_edit_buf.pop();
            }
            KeyCode::Char(c) => {
                app.config_edit_buf.push(c);
            }
            _ => {}
        }
        return;
    }

    // Config tab Enter: toggle bool or start text edit
    if app.active_tab == Tab::Config && key.code == KeyCode::Enter {
        let idx = app.scroll_offset();
        let fields = config_edit::fields();
        if idx < fields.len() {
            match fields[idx].kind {
                config_edit::FieldKind::Bool => {
                    let ok = app
                        .state
                        .config
                        .as_mut()
                        .map(|c| config_edit::toggle(c, idx))
                        .unwrap_or(false);
                    if !ok {
                        app.config_error = Some(Instant::now());
                    }
                }
                config_edit::FieldKind::Text => {
                    if let Some(ref config) = app.state.config {
                        app.config_edit_buf = config_edit::get_value(config, idx);
                        app.config_editing = true;
                    }
                }
            }
        }
        return;
    }

    match key.code {
        KeyCode::Char('q') | KeyCode::Esc => {
            if app.show_help {
                app.show_help = false;
            } else {
                app.should_quit = true;
            }
        }
        KeyCode::Char('s') => {
            if !app.syncing {
                let exe = std::env::current_exe().unwrap_or_else(|_| "tether".into());
                if let Ok(child) = std::process::Command::new(exe)
                    .arg("sync")
                    .stdout(std::process::Stdio::null())
                    .stderr(std::process::Stdio::null())
                    .spawn()
                {
                    app.syncing = true;
                    app.sync_child = Some(child);
                }
            }
        }
        KeyCode::Char('d') => {
            if app.daemon_op == DaemonOp::None && app.daemon_child.is_none() {
                let exe = std::env::current_exe().unwrap_or_else(|_| "tether".into());
                let arg = if app.state.daemon_running {
                    "stop"
                } else {
                    "start"
                };
                if let Ok(child) = std::process::Command::new(exe)
                    .args(["daemon", arg])
                    .stdout(std::process::Stdio::null())
                    .stderr(std::process::Stdio::null())
                    .spawn()
                {
                    app.daemon_op = if app.state.daemon_running {
                        DaemonOp::Stopping
                    } else {
                        DaemonOp::Starting
                    };
                    app.daemon_child = Some(child);
                }
            }
        }
        KeyCode::Char('r') => {
            app.state = DashboardState::load();
            app.last_refresh = Instant::now();
        }
        KeyCode::Tab => {
            let tabs = Tab::all();
            let current = tabs.iter().position(|t| *t == app.active_tab).unwrap_or(0);
            app.active_tab = tabs[(current + 1) % tabs.len()];
        }
        KeyCode::Char('1') => app.active_tab = Tab::Overview,
        KeyCode::Char('2') => app.active_tab = Tab::Files,
        KeyCode::Char('3') => app.active_tab = Tab::Packages,
        KeyCode::Char('4') => app.active_tab = Tab::Machines,
        KeyCode::Char('5') => app.active_tab = Tab::Config,
        KeyCode::Char('j') | KeyCode::Down => {
            let max = app.item_count().saturating_sub(1);
            if app.scroll_offset() < max {
                *app.scroll_offset_mut() += 1;
            }
        }
        KeyCode::Char('k') | KeyCode::Up => {
            let offset = app.scroll_offset_mut();
            *offset = offset.saturating_sub(1);
        }
        KeyCode::Char('?') => {
            app.show_help = !app.show_help;
        }
        _ => {}
    }
}

fn draw(f: &mut Frame, app: &mut App) {
    let main_chunks = Layout::vertical([
        Constraint::Length(3),
        Constraint::Min(4),
        Constraint::Length(1),
    ])
    .split(f.area());

    // Clear config error after 3 seconds
    if let Some(t) = app.config_error {
        if t.elapsed() >= Duration::from_secs(3) {
            app.config_error = None;
        }
    }

    widgets::status::render(
        f,
        main_chunks[0],
        &app.state,
        app.syncing,
        app.daemon_op,
        app.config_error.is_some(),
    );

    // Tab bar
    let tab_titles: Vec<Line> = Tab::all()
        .iter()
        .enumerate()
        .map(|(i, t)| {
            let num = format!("{}", i + 1);
            if *t == app.active_tab {
                Line::from(vec![
                    Span::styled(num, Style::default().fg(Color::Yellow).bold()),
                    Span::raw(":"),
                    Span::styled(t.title(), Style::default().fg(Color::White).bold()),
                ])
            } else {
                Line::from(vec![
                    Span::styled(num, Style::default().fg(Color::DarkGray)),
                    Span::raw(":"),
                    Span::styled(t.title(), Style::default().fg(Color::DarkGray)),
                ])
            }
        })
        .collect();

    let tabs = ratatui::widgets::Tabs::new(tab_titles)
        .divider(Span::styled(" | ", Style::default().fg(Color::DarkGray)))
        .select(
            Tab::all()
                .iter()
                .position(|t| *t == app.active_tab)
                .unwrap_or(0),
        );

    // Split content into tab bar + content
    let content_chunks =
        Layout::vertical([Constraint::Length(1), Constraint::Min(3)]).split(main_chunks[1]);

    f.render_widget(tabs, content_chunks[0]);

    match app.active_tab {
        Tab::Overview => draw_overview(f, content_chunks[1], app),
        Tab::Files => widgets::files::render(f, content_chunks[1], &app.state, app.scroll_offset()),
        Tab::Packages => widgets::packages::render(f, content_chunks[1], &app.state),
        Tab::Machines => widgets::machines::render(f, content_chunks[1], &app.state),
        Tab::Config => widgets::config::render(
            f,
            content_chunks[1],
            &app.state.config,
            app.scroll_offset(),
            app.config_editing,
            &app.config_edit_buf,
        ),
    }

    widgets::help::render_bar(f, main_chunks[2]);

    if app.show_help {
        widgets::help::render_overlay(f);
    }
}

fn draw_overview(f: &mut Frame, area: Rect, app: &App) {
    let content_chunks = Layout::vertical([
        Constraint::Percentage(40),
        Constraint::Percentage(30),
        Constraint::Percentage(30),
    ])
    .split(area);

    let top_chunks = Layout::horizontal([Constraint::Percentage(50), Constraint::Percentage(50)])
        .split(content_chunks[0]);

    widgets::files::render(f, top_chunks[0], &app.state, app.scroll_offset());
    widgets::packages::render(f, top_chunks[1], &app.state);
    widgets::machines::render(f, content_chunks[1], &app.state);
    widgets::activity::render(f, content_chunks[2], &app.state.activity_lines);
}

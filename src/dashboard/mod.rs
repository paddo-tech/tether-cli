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

pub struct ListEditState {
    field_key: &'static str,
    field_label: &'static str,
    is_dotfile: bool,
    items: Vec<String>,
    cursor: usize,
    adding: bool,
    add_buf: String,
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
    flash_error: Option<(Instant, String)>,
    list_edit: Option<ListEditState>,
    // Packages tab state
    pkg_expanded: Option<String>,
    pkg_cursor: usize,
    uninstall_confirm: Option<(String, String)>,
    uninstalling: Option<(String, String)>,
    uninstall_rx: Option<std::sync::mpsc::Receiver<std::result::Result<(), String>>>,
    // Machines tab state
    machine_expanded: Option<String>,
    machine_cursor: usize,
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
            Tab::Packages => {
                widgets::packages::build_rows(&self.state, self.pkg_expanded.as_deref()).len()
            }
            Tab::Machines => {
                widgets::machines::build_rows(&self.state, self.machine_expanded.as_deref()).len()
            }
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
        flash_error: None,
        list_edit: None,
        pkg_expanded: None,
        pkg_cursor: 0,
        uninstall_confirm: None,
        uninstalling: None,
        uninstall_rx: None,
        machine_expanded: None,
        machine_cursor: 0,
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
        terminal.draw(|f| draw(f, &app))?;

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

        // Check uninstall result
        if let Some(ref rx) = app.uninstall_rx {
            if let Ok(result) = rx.try_recv() {
                match result {
                    Ok(()) => {
                        // Trigger a sync so machine state reflects the removal
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
                    Err(msg) => {
                        app.flash_error =
                            Some((Instant::now(), format!("uninstall failed: {}", msg)));
                    }
                }
                app.uninstalling = None;
                app.uninstall_rx = None;
            }
        }

        // Clear flash error after 3 seconds
        if let Some((t, _)) = &app.flash_error {
            if t.elapsed() >= Duration::from_secs(3) {
                app.flash_error = None;
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

    // Uninstall confirmation popup intercepts keys
    if app.uninstall_confirm.is_some() {
        match key.code {
            KeyCode::Char('y') | KeyCode::Enter => {
                if let Some((manager_key, pkg_name)) = app.uninstall_confirm.take() {
                    let (tx, rx) = std::sync::mpsc::channel();
                    let mk = manager_key.clone();
                    let pn = pkg_name.clone();
                    std::thread::spawn(move || {
                        let rt = tokio::runtime::Builder::new_current_thread()
                            .enable_all()
                            .build();
                        let result = match rt {
                            Ok(rt) => rt.block_on(async { run_uninstall(&mk, &pn).await }),
                            Err(e) => Err(e.to_string()),
                        };
                        let _ = tx.send(result);
                    });
                    app.uninstalling = Some((manager_key, pkg_name));
                    app.uninstall_rx = Some(rx);
                }
            }
            KeyCode::Char('n') | KeyCode::Esc => {
                app.uninstall_confirm = None;
            }
            _ => {}
        }
        return;
    }

    // List edit sub-view intercepts keys
    if let Some(ref mut le) = app.list_edit {
        if le.adding {
            match key.code {
                KeyCode::Esc => {
                    le.adding = false;
                    le.add_buf.clear();
                }
                KeyCode::Enter => {
                    let buf = le.add_buf.clone();
                    let field_key = le.field_key;
                    let is_dotfile = le.is_dotfile;
                    le.adding = false;
                    le.add_buf.clear();

                    let ok = if is_dotfile {
                        app.state
                            .config
                            .as_mut()
                            .map(|c| config_edit::add_dotfile(c, &buf, true))
                            .unwrap_or(false)
                    } else {
                        app.state
                            .config
                            .as_mut()
                            .map(|c| config_edit::add_list_item(c, field_key, &buf))
                            .unwrap_or(false)
                    };
                    if !ok {
                        app.flash_error = Some((Instant::now(), "save failed".into()));
                    }
                    // Refresh items
                    refresh_list_edit(app);
                }
                KeyCode::Backspace => {
                    le.add_buf.pop();
                }
                KeyCode::Char(c) => {
                    le.add_buf.push(c);
                }
                _ => {}
            }
            return;
        }

        match key.code {
            KeyCode::Esc => {
                app.list_edit = None;
            }
            KeyCode::Char('j') | KeyCode::Down => {
                let max = le.items.len().saturating_sub(1);
                if le.cursor < max {
                    le.cursor += 1;
                }
            }
            KeyCode::Char('k') | KeyCode::Up => {
                le.cursor = le.cursor.saturating_sub(1);
            }
            KeyCode::Char('a') => {
                le.adding = true;
                le.add_buf.clear();
            }
            KeyCode::Char('d') | KeyCode::Delete => {
                let cursor = le.cursor;
                let field_key = le.field_key;
                let is_dotfile = le.is_dotfile;

                let ok = if is_dotfile {
                    app.state
                        .config
                        .as_mut()
                        .map(|c| config_edit::remove_dotfile(c, cursor))
                        .unwrap_or(false)
                } else {
                    app.state
                        .config
                        .as_mut()
                        .map(|c| config_edit::remove_list_item(c, field_key, cursor))
                        .unwrap_or(false)
                };
                if !ok {
                    app.flash_error = Some((Instant::now(), "save failed".into()));
                }
                refresh_list_edit(app);
                // Adjust cursor if needed
                if let Some(ref mut le) = app.list_edit {
                    if le.cursor > 0 && le.cursor >= le.items.len() {
                        le.cursor = le.items.len().saturating_sub(1);
                    }
                }
            }
            KeyCode::Char('t') => {
                if le.is_dotfile {
                    let cursor = le.cursor;
                    let ok = app
                        .state
                        .config
                        .as_mut()
                        .map(|c| config_edit::toggle_dotfile_create(c, cursor))
                        .unwrap_or(false);
                    if !ok {
                        app.flash_error = Some((Instant::now(), "save failed".into()));
                    }
                    refresh_list_edit(app);
                }
            }
            _ => {}
        }
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
                    app.flash_error = Some((Instant::now(), "save failed".into()));
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

    // Config tab Enter: toggle bool, start text edit, or open list sub-view
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
                        app.flash_error = Some((Instant::now(), "save failed".into()));
                    }
                }
                config_edit::FieldKind::Text => {
                    if let Some(ref config) = app.state.config {
                        app.config_edit_buf = config_edit::get_value(config, idx);
                        app.config_editing = true;
                    }
                }
                config_edit::FieldKind::List => {
                    if let Some(ref config) = app.state.config {
                        let items = config_edit::get_list_items(config, fields[idx].key);
                        app.list_edit = Some(ListEditState {
                            field_key: fields[idx].key,
                            field_label: fields[idx].label,
                            is_dotfile: false,
                            items,
                            cursor: 0,
                            adding: false,
                            add_buf: String::new(),
                        });
                    }
                }
                config_edit::FieldKind::DotfileList => {
                    if let Some(ref config) = app.state.config {
                        let dotfiles = config_edit::get_dotfile_items(config);
                        let items = dotfiles
                            .iter()
                            .map(|(path, create)| {
                                format!("{}  create: {}", path, if *create { "yes" } else { "no" })
                            })
                            .collect();
                        app.list_edit = Some(ListEditState {
                            field_key: "dotfiles.files",
                            field_label: "Dotfiles",
                            is_dotfile: true,
                            items,
                            cursor: 0,
                            adding: false,
                            add_buf: String::new(),
                        });
                    }
                }
            }
        }
        return;
    }

    // Machines tab Enter: expand/collapse
    if app.active_tab == Tab::Machines && key.code == KeyCode::Enter {
        let rows = widgets::machines::build_rows(&app.state, app.machine_expanded.as_deref());
        if app.machine_cursor < rows.len() {
            if let widgets::machines::MachineRow::Header { machine_id, .. } =
                &rows[app.machine_cursor]
            {
                if app.machine_expanded.as_deref() == Some(machine_id.as_str()) {
                    app.machine_expanded = None;
                } else {
                    app.machine_expanded = Some(machine_id.clone());
                }
                // Clamp cursor to new row count
                let new_rows =
                    widgets::machines::build_rows(&app.state, app.machine_expanded.as_deref());
                if app.machine_cursor >= new_rows.len() {
                    app.machine_cursor = new_rows.len().saturating_sub(1);
                }
            }
        }
        return;
    }

    // Packages tab Enter: expand/collapse or uninstall
    if app.active_tab == Tab::Packages && key.code == KeyCode::Enter {
        let rows = widgets::packages::build_rows(&app.state, app.pkg_expanded.as_deref());
        if app.pkg_cursor < rows.len() {
            match &rows[app.pkg_cursor] {
                widgets::packages::PkgRow::Header { manager_key, .. } => {
                    if app.pkg_expanded.as_deref() == Some(manager_key.as_str()) {
                        app.pkg_expanded = None;
                    } else {
                        app.pkg_expanded = Some(manager_key.clone());
                    }
                    // Clamp cursor to new row count
                    let new_rows =
                        widgets::packages::build_rows(&app.state, app.pkg_expanded.as_deref());
                    if app.pkg_cursor >= new_rows.len() {
                        app.pkg_cursor = new_rows.len().saturating_sub(1);
                    }
                }
                widgets::packages::PkgRow::Package {
                    manager_key, name, ..
                } => {
                    if app.uninstalling.is_none() && manager_key != "brew_taps" {
                        app.uninstall_confirm = Some((manager_key.clone(), name.clone()));
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
            if app.active_tab == Tab::Packages {
                let max = app.item_count().saturating_sub(1);
                if app.pkg_cursor < max {
                    app.pkg_cursor += 1;
                }
            } else if app.active_tab == Tab::Machines {
                let max = app.item_count().saturating_sub(1);
                if app.machine_cursor < max {
                    app.machine_cursor += 1;
                }
            } else {
                let max = app.item_count().saturating_sub(1);
                if app.scroll_offset() < max {
                    *app.scroll_offset_mut() += 1;
                }
            }
        }
        KeyCode::Char('k') | KeyCode::Up => {
            if app.active_tab == Tab::Packages {
                app.pkg_cursor = app.pkg_cursor.saturating_sub(1);
            } else if app.active_tab == Tab::Machines {
                app.machine_cursor = app.machine_cursor.saturating_sub(1);
            } else {
                let offset = app.scroll_offset_mut();
                *offset = offset.saturating_sub(1);
            }
        }
        KeyCode::Char('?') => {
            app.show_help = !app.show_help;
        }
        _ => {}
    }
}

/// Refresh list_edit items from current config state
fn refresh_list_edit(app: &mut App) {
    let Some(ref le) = app.list_edit else {
        return;
    };
    let Some(ref config) = app.state.config else {
        return;
    };
    let field_key = le.field_key;
    let field_label = le.field_label;
    let is_dotfile = le.is_dotfile;
    let cursor = le.cursor;

    let items = if is_dotfile {
        config_edit::get_dotfile_items(config)
            .iter()
            .map(|(path, create)| {
                format!("{}  create: {}", path, if *create { "yes" } else { "no" })
            })
            .collect()
    } else {
        config_edit::get_list_items(config, field_key)
    };

    app.list_edit = Some(ListEditState {
        field_key,
        field_label,
        is_dotfile,
        items,
        cursor,
        adding: false,
        add_buf: String::new(),
    });
}

async fn run_uninstall(manager_key: &str, package: &str) -> std::result::Result<(), String> {
    use crate::packages::*;

    let manager: Box<dyn PackageManager> = match manager_key {
        "brew_formulae" | "brew_casks" => Box::new(BrewManager),
        "npm" => Box::new(NpmManager),
        "pnpm" => Box::new(PnpmManager),
        "bun" => Box::new(BunManager),
        "gem" => Box::new(GemManager),
        "uv" => Box::new(UvManager),
        _ => return Err(format!("Unknown manager: {}", manager_key)),
    };

    manager.uninstall(package).await.map_err(|e| e.to_string())
}

fn draw(f: &mut Frame, app: &App) {
    let main_chunks = Layout::vertical([
        Constraint::Length(3),
        Constraint::Min(4),
        Constraint::Length(1),
    ])
    .split(f.area());

    widgets::status::render(
        f,
        main_chunks[0],
        &app.state,
        app.syncing,
        app.daemon_op,
        app.flash_error.as_ref().map(|(_, msg)| msg.as_str()),
        app.uninstalling.as_ref(),
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
        Tab::Packages => {
            widgets::packages::render(
                f,
                content_chunks[1],
                &app.state,
                app.pkg_expanded.as_deref(),
                app.pkg_cursor,
            );
        }
        Tab::Machines => widgets::machines::render(
            f,
            content_chunks[1],
            &app.state,
            app.machine_expanded.as_deref(),
            app.machine_cursor,
        ),
        Tab::Config => widgets::config::render(
            f,
            content_chunks[1],
            &app.state.config,
            app.scroll_offset(),
            app.config_editing,
            &app.config_edit_buf,
            app.list_edit.as_ref(),
        ),
    }

    widgets::help::render_bar(f, main_chunks[2], app.active_tab);

    if app.show_help {
        widgets::help::render_overlay(f);
    }

    // Uninstall confirmation popup
    if let Some((ref manager_key, ref pkg_name)) = app.uninstall_confirm {
        render_uninstall_popup(f, manager_key, pkg_name);
    }
}

fn render_uninstall_popup(f: &mut Frame, manager_key: &str, pkg_name: &str) {
    let area = f.area();
    let label = widgets::manager_label(manager_key);
    let msg = format!("Uninstall {} ({})?", pkg_name, label);
    let width = (msg.len() as u16 + 8).min(area.width.saturating_sub(4));
    let height = 5u16.min(area.height.saturating_sub(2));
    let x = (area.width.saturating_sub(width)) / 2;
    let y = (area.height.saturating_sub(height)) / 2;
    let popup_area = Rect::new(x, y, width, height);

    f.render_widget(ratatui::widgets::Clear, popup_area);

    let text = vec![
        Line::from(""),
        Line::from(Span::styled(
            format!("  {}", msg),
            Style::default().fg(Color::White),
        )),
        Line::from(""),
        Line::from(vec![
            Span::styled("  y", Style::default().fg(Color::Yellow).bold()),
            Span::styled(" confirm    ", Style::default().fg(Color::DarkGray)),
            Span::styled("n/Esc", Style::default().fg(Color::Yellow).bold()),
            Span::styled(" cancel", Style::default().fg(Color::DarkGray)),
        ]),
    ];

    let paragraph = ratatui::widgets::Paragraph::new(text).block(
        ratatui::widgets::Block::default()
            .title(" Uninstall ")
            .borders(ratatui::widgets::Borders::ALL)
            .border_style(Style::default().fg(Color::Red)),
    );
    f.render_widget(paragraph, popup_area);
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
    widgets::packages::render_overview(f, top_chunks[1], &app.state);
    widgets::machines::render_overview(f, content_chunks[1], &app.state);
    widgets::activity::render(f, content_chunks[2], &app.state.activity_lines);
}

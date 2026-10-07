use super::app::{Action, App, DaemonOp, Hit, InstallOp, Job, Overlay, Tab};
use super::components::palette::{self, Palette, Target};
use super::components::{
    backup_picker, config, confirm, file_import, files, log_view, machines, overview,
    package_profiles, packages, pkg_import, profile_picker, security,
};
use super::msg::{Cmd, KeyOutcome, Msg};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::layout::Rect;
use std::time::{Duration, Instant};

const REFRESH_INTERVAL: Duration = Duration::from_secs(30);

/// Any message other than a tick may change what is on screen, so the regions from the
/// last draw are dropped. A click before the next draw hits nothing instead of a stale row.
pub fn update(app: &mut App, msg: Msg) -> Option<Cmd> {
    let tick = matches!(msg, Msg::Tick);
    let cmd = apply(app, msg);
    if !tick {
        app.hits.borrow_mut().clear();
    }
    cmd
}

fn apply(app: &mut App, msg: Msg) -> Option<Cmd> {
    match msg {
        Msg::Key(key) => on_key(app, key),
        Msg::Mouse(m) => on_mouse(app, m),
        Msg::Resize(w, h) => {
            app.viewport = Rect::new(0, 0, w, h);
            None
        }
        Msg::Tick => on_tick(app),
        Msg::Activity(counts) => {
            app.sync_activity = counts;
            None
        }
        Msg::JobStarted(job) => {
            if let Job::Rollback {
                manager,
                short_hash,
                ..
            } = &job
            {
                app.flash_success(format!("Rolling back {} to {}", manager, short_hash));
            }
            app.running = Some(job);
            None
        }
        Msg::JobSpawnFailed(job) => {
            // A follow-up sync sent before the running job's start was seen; retry when it exits.
            if matches!(job, Job::Sync) && app.running.is_some() {
                app.sync_pending = true;
            } else if matches!(job, Job::Rollback { .. } | Job::Sync) {
                app.flash_error("Could not start tether");
            }
            None
        }
        Msg::JobExited { job, success } => {
            if !success {
                app.flash_error(format!("tether {} failed", job.args().join(" ")));
            }
            app.running = None;
            app.reload_state();
            if std::mem::take(&mut app.sync_pending) {
                app.sync_cmd()
            } else {
                None
            }
        }
        Msg::DaemonOpStarted(op) => {
            app.daemon_op = op;
            None
        }
        Msg::DaemonOpExited => {
            app.daemon_op = DaemonOp::None;
            app.reload_state();
            None
        }
        Msg::UninstallDone(result) => {
            app.uninstalling = None;
            match result {
                Ok(not_saved) => {
                    if let Some(e) = not_saved {
                        app.flash_error(e);
                    }
                    app.follow_up_sync()
                }
                Err(e) => {
                    app.flash_error(format!("uninstall failed: {}", e));
                    None
                }
            }
        }
        Msg::ProfilesSaved(result) => {
            app.reload_state();
            match result {
                Ok(msg) => app.flash_success(msg),
                Err(e) => app.flash_error(format!("saving profiles failed: {}", e)),
            }
            None
        }
        Msg::InstallDone { op, result } => on_install_done(app, op, result),
        Msg::OsvUnreachable { op, error } => {
            if app.installing.as_ref().map(|i| i.id) == Some(op.id) {
                app.installing = None;
                app.overlays
                    .push(Overlay::Confirm(confirm::Confirm::InstallWithoutOsv {
                        manager_key: op.manager_key,
                        name: op.name,
                        error,
                        arming: Default::default(),
                    }));
            }
            None
        }
        Msg::ApproveOsvUnreachable {
            op,
            result,
            unchecked,
        } => {
            if app.installing.as_ref().map(|i| i.id) == Some(op.id) {
                app.installing = None;
                if let Err(e) = result {
                    app.flash_error(format!("install failed: {}", e));
                }
                app.overlays
                    .push(Overlay::Confirm(confirm::Confirm::ApproveWithoutOsv {
                        items: unchecked,
                        arming: Default::default(),
                    }));
                // Items OSV did check are approved and installed already. Their sync waits
                // for the answer, because `y` needs the sync lock
                security::reload(app);
            }
            None
        }
        Msg::InboxDone(result) => {
            security::reload(app);
            match result {
                Ok(msg) => app.flash_success(msg),
                Err(e) => app.flash_error(e),
            }
            None
        }
        Msg::MachineRemoved { machine_id, result } => match result {
            Ok(untrusted) => {
                app.reload_state();
                app.machines.expanded = None;
                super::components::clamp_cursor(&mut app.machines.cursor, app.state.machines.len());
                app.flash_success(if untrusted {
                    format!(
                        "Removed old record {} and untrusted its key on this machine",
                        machine_id
                    )
                } else {
                    format!("Removed old record {}", machine_id)
                });
                // The sync pushes the removal commit
                app.follow_up_sync()
            }
            Err(e) => {
                app.flash_error(format!("remove failed: {}", e));
                None
            }
        },
        Msg::LocalPackages(packages) => {
            on_local_packages(app, packages);
            None
        }
        Msg::RestoreDone {
            dotfile,
            short_hash,
            result,
        } => match result {
            Ok(()) => {
                app.flash_success(format!("Restored {} to {}", dotfile, short_hash));
                app.follow_up_sync()
            }
            Err(e) => {
                app.flash_error(format!("restore failed: {}", e));
                None
            }
        },
    }
}

/// Expire toasts and show held warnings; every refresh interval reload state and recount
/// sync activity.
fn on_tick(app: &mut App) -> Option<Cmd> {
    let now = Instant::now();
    app.toasts.retain(|t| t.alive(now));
    for warning in crate::cli::Output::take_warnings() {
        app.flash_info(warning);
    }
    if app.last_refresh.elapsed() >= REFRESH_INTERVAL {
        app.reload_state();
        app.hits.borrow_mut().clear();
        return Some(Cmd::LoadActivity);
    }
    None
}

/// Keys go to the Ctrl keys, the top modal overlay, then the active tab, then the global
/// keymap.
fn on_key(app: &mut App, key: KeyEvent) -> Option<Cmd> {
    if let KeyOutcome::Handled(cmd) = ctrl_key(app, key) {
        return cmd;
    }

    if app.overlays.last().is_some_and(Overlay::is_modal) {
        return match app.overlays.pop()? {
            Overlay::Confirm(c) => confirm::handle_key(app, c, key),
            Overlay::FileImport(p) => file_import::handle_key(app, p, key),
            Overlay::PkgImport(p) => pkg_import::handle_key(app, p, key),
            Overlay::ProfilePicker(p) => profile_picker::handle_key(app, p, key),
            Overlay::PackageProfiles(p) => package_profiles::handle_key(app, p, key),
            Overlay::Log(l) => log_view::handle_key(app, l, key),
            Overlay::BackupPicker(p) => backup_picker::handle_key(app, p, key),
            Overlay::Palette(p) => match palette::handle_key(app, p, key) {
                Some(target) => run_target(app, target),
                None => None,
            },
            Overlay::Help => unreachable!("help is not modal"),
        };
    }

    // Help draws over the tab, so Esc closes it before it reaches the tab
    if key.code == KeyCode::Esc && app.help_open() {
        app.overlays.retain(|o| !matches!(o, Overlay::Help));
        return None;
    }

    if let KeyOutcome::Handled(cmd) = tab_key(app, key) {
        return cmd;
    }
    match global_key(app, key) {
        KeyOutcome::Handled(cmd) => cmd,
        KeyOutcome::Ignored => None,
    }
}

/// Ctrl-C quits and Ctrl-K opens the palette, before a tab can read them as plain keys.
fn ctrl_key(app: &mut App, key: KeyEvent) -> KeyOutcome {
    if !key.modifiers.contains(KeyModifiers::CONTROL) {
        return KeyOutcome::Ignored;
    }
    match key.code {
        KeyCode::Char('c') => request_quit(app),
        KeyCode::Char('k') if !app.overlays.last().is_some_and(Overlay::is_modal) => {
            let entries = palette::entries(app);
            app.overlays.push(Overlay::Palette(Palette::new(entries)));
        }
        _ => return KeyOutcome::Ignored,
    }
    KeyOutcome::Handled(None)
}

fn tab_key(app: &mut App, key: KeyEvent) -> KeyOutcome {
    match app.active_tab {
        Tab::Overview => overview::handle_key(app, key),
        Tab::Files => files::handle_key(app, key),
        Tab::Packages => packages::handle_key(app, key),
        Tab::Machines => machines::handle_key(app, key),
        Tab::Config => config::handle_key(app, key),
        Tab::Security => security::handle_key(app, key),
    }
}

fn global_key(app: &mut App, key: KeyEvent) -> KeyOutcome {
    let cmd = match key.code {
        KeyCode::Esc => {
            app.overlays.retain(|o| !matches!(o, Overlay::Help));
            None
        }
        KeyCode::Char('q') => {
            if app.help_open() {
                app.overlays.retain(|o| !matches!(o, Overlay::Help));
            } else {
                request_quit(app);
            }
            None
        }
        KeyCode::Char('s') => run_action(app, Action::Sync),
        KeyCode::Char('d') => run_action(app, Action::ToggleDaemon),
        KeyCode::Char('r') => run_action(app, Action::Refresh),
        KeyCode::Tab => {
            let tabs = Tab::all();
            let current = tabs.iter().position(|t| *t == app.active_tab).unwrap_or(0);
            app.active_tab = tabs[(current + 1) % tabs.len()];
            None
        }
        KeyCode::Char(c @ '1'..='9') if c as usize - ('1' as usize) < Tab::all().len() => {
            app.active_tab = Tab::all()[c as usize - '1' as usize];
            None
        }
        KeyCode::Char('?') => run_action(app, Action::Help),
        _ => return KeyOutcome::Ignored,
    };
    KeyOutcome::Handled(cmd)
}

pub fn run_action(app: &mut App, action: Action) -> Option<Cmd> {
    match action {
        Action::Sync => return app.sync_cmd(),
        // Starting is harmless; stopping ends the automatic sync, so it asks
        Action::ToggleDaemon if app.daemon_op == DaemonOp::None => {
            if !app.state.daemon_running {
                return Some(Cmd::Daemon(DaemonOp::Starting));
            }
            app.overlays
                .push(Overlay::Confirm(confirm::Confirm::StopDaemon {
                    arming: Default::default(),
                }));
        }
        Action::ToggleDaemon => {}
        Action::Refresh => app.reload_state(),
        Action::Help => {
            if app.help_open() {
                app.overlays.retain(|o| !matches!(o, Overlay::Help));
            } else {
                app.overlays.push(Overlay::Help);
            }
        }
        Action::Quit => request_quit(app),
        Action::ImportPackages => {
            app.active_tab = Tab::Packages;
            if app.installing.is_none() {
                open_or_report(app, packages::open_import, "No packages to import");
            }
        }
        Action::ImportDotfile => {
            app.active_tab = Tab::Files;
            open_or_report(app, files::open_import, "No dotfiles to import");
        }
        Action::PickProfile => {
            app.active_tab = Tab::Machines;
            machines::open_profile_picker(app);
        }
        Action::ApproveAll => {
            app.active_tab = Tab::Security;
            security::confirm_approve_all(app);
        }
        Action::DaemonLog => app.overlays.push(Overlay::Log(log_view::LogView::open())),
    }
    None
}

fn open_or_report(app: &mut App, open: fn(&mut App), empty: &str) {
    let before = app.overlays.len();
    open(app);
    if app.overlays.len() == before {
        app.flash_info(empty);
    }
}

/// Run what the palette picked: an action, or a jump to a tab, file or package.
fn run_target(app: &mut App, target: Target) -> Option<Cmd> {
    match target {
        Target::Action(action) => return run_action(app, action),
        Target::Inbox(id) => {
            app.active_tab = Tab::Security;
            security::open(app, &id);
        }
        Target::RemoveOldRecord(id) => {
            app.active_tab = Tab::Machines;
            if let Some(i) = app.state.machines.iter().position(|m| m.machine_id == id) {
                app.machines.cursor = i;
            }
            machines::confirm_remove(app, &id);
        }
        Target::Tab(tab) => app.active_tab = tab,
        Target::File { section, path } => {
            app.active_tab = Tab::Files;
            app.files.collapsed.remove(&section);
            let rows = files::build_rows(&app.state, &app.files);
            let mut in_section = false;
            for (i, row) in rows.iter().enumerate() {
                match row {
                    files::FileRow::SectionHeader { label, .. } => in_section = *label == section,
                    files::FileRow::File { path: p, .. } if in_section && *p == path => {
                        app.files.cursor = i;
                        break;
                    }
                    _ => {}
                }
            }
        }
        Target::Package { manager_key, name } => {
            app.active_tab = Tab::Packages;
            app.packages.expanded = Some(manager_key.clone());
            let rows = packages::build_rows(&app.state, &app.packages);
            if let Some(i) = rows.iter().position(|r| {
                matches!(r, packages::PkgRow::Package { manager_key: k, name: n } if *k == manager_key && *n == name)
            }) {
                app.packages.cursor = i;
            }
        }
    }
    None
}

/// Clicks hit what the last draw recorded; the wheel scrolls like arrow keys.
fn on_mouse(app: &mut App, m: MouseEvent) -> Option<Cmd> {
    match m.kind {
        // Help covers the tab, so the wheel does not scroll it.
        MouseEventKind::ScrollDown | MouseEventKind::ScrollUp if app.help_open() => None,
        MouseEventKind::ScrollDown => on_key(app, KeyEvent::from(KeyCode::Down)),
        MouseEventKind::ScrollUp => on_key(app, KeyEvent::from(KeyCode::Up)),
        MouseEventKind::Down(MouseButton::Left) => match app.hit_at(m.column, m.row)? {
            // Under a modal only its own buttons and items answer; a row click would become
            // Enter on the modal and could accept a destructive confirm.
            Hit::Tab(_) | Hit::Row(_) if app.overlays.last().is_some_and(Overlay::is_modal) => None,
            Hit::Tab(tab) => {
                app.active_tab = tab;
                None
            }
            Hit::Key(key) => on_key(app, key),
            Hit::Row(i) => click_row(app, i),
            Hit::Item(i) => click_item(app, i),
            Hit::CloseHelp => {
                app.overlays.retain(|o| !matches!(o, Overlay::Help));
                None
            }
            Hit::Block => None,
            Hit::Toast(i) => {
                if i < app.toasts.len() {
                    app.toasts.remove(i);
                }
                None
            }
        },
        _ => None,
    }
}

/// The first click selects a row; a click on the selected row opens it like Enter.
fn click_row(app: &mut App, i: usize) -> Option<Cmd> {
    if app.active_tab == Tab::Security && app.security.cursor != i {
        security::move_cursor(app, i);
        return None;
    }
    let cursor = match app.active_tab {
        Tab::Overview => return None,
        Tab::Files => &mut app.files.cursor,
        Tab::Packages => &mut app.packages.cursor,
        Tab::Machines => &mut app.machines.cursor,
        Tab::Security => &mut app.security.cursor,
        Tab::Config => {
            // A click mid-edit would move the edit to another field.
            if app.config.editing || app.config.list_edit.as_ref().is_some_and(|l| l.adding) {
                return None;
            }
            match app.config.list_edit.as_mut() {
                Some(le) => &mut le.cursor,
                None => &mut app.config.selected,
            }
        }
    };
    if *cursor == i {
        on_key(app, KeyEvent::from(KeyCode::Enter))
    } else {
        *cursor = i;
        None
    }
}

fn click_item(app: &mut App, i: usize) -> Option<Cmd> {
    let cursor = match app.overlays.last_mut()? {
        Overlay::Palette(p) => &mut p.cursor,
        Overlay::FileImport(p) => &mut p.cursor,
        Overlay::PkgImport(p) if p.confirm.is_none() => &mut p.cursor,
        Overlay::ProfilePicker(p) => &mut p.cursor,
        Overlay::PackageProfiles(p) => &mut p.cursor,
        Overlay::BackupPicker(p) => &mut p.cursor,
        _ => return None,
    };
    if *cursor == i {
        on_key(app, KeyEvent::from(KeyCode::Enter))
    } else {
        *cursor = i;
        None
    }
}

/// A killed rollback leaves packages half-removed with no tombstones.
fn request_quit(app: &mut App) {
    if app.rollback_running() {
        app.flash_error("Rollback in progress, wait for it to finish");
    } else {
        app.should_quit = true;
    }
}

fn on_install_done(app: &mut App, op: InstallOp, result: Result<(), String>) -> Option<Cmd> {
    // A second install replaces the first; its result is no longer awaited.
    if app.installing.as_ref().map(|i| i.id) != Some(op.id) {
        return None;
    }
    let InstallOp {
        manager_key, name, ..
    } = op;
    app.installing = None;
    if let Err(e) = result {
        app.flash_error(format!("install failed: {}", e));
        // A batch approval removes items and installs some before one fails
        security::reload(app);
        return app.follow_up_sync();
    }
    app.flash_success(format!("installed {}", name));
    if let Some(pos) = app
        .overlays
        .iter()
        .position(|o| matches!(o, Overlay::PkgImport(_)))
    {
        if let Overlay::PkgImport(picker) = &mut app.overlays[pos] {
            if !picker.remove(&manager_key, &name) {
                app.overlays.remove(pos);
            }
        }
    }
    app.follow_up_sync()
}

/// Live package lists show in place of this machine's record until a later sync writes it.
/// Only a sync writes this machine's record. It reads names and versions from the managers
/// together, so a signed record never pairs new names with old versions.
fn on_local_packages(app: &mut App, packages: std::collections::HashMap<String, Vec<String>>) {
    app.local_packages = Some((chrono::Utc::now(), packages));
    app.show_local_packages();
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dashboard::components::confirm::Confirm;
    use crate::dashboard::components::toast::{Toast, ToastKind, MAX_TOASTS};
    use crate::dashboard::state::DashboardState;
    use crate::packages::inbox::{InboxItem, Kind, Reason};
    use crate::sync::{ConflictState, TeamManifest};
    use std::collections::{BTreeSet, HashMap};
    use std::time::Instant;

    fn app() -> App {
        let state = DashboardState {
            config: None,
            sync_state: None,
            conflicts: ConflictState::default(),
            machines: Vec::new(),
            team_manifest: TeamManifest::default(),
            daemon_pid: None,
            daemon_running: false,
            activity_lines: Vec::new(),
            inbox: Default::default(),
            trusted: Vec::new(),
            old_ids: Vec::new(),
            record_status: Vec::new(),
            old_builds: Vec::new(),
            membership: None,
            membership_error: None,
        };
        App::new(state, HashMap::new())
    }

    fn last_toast(app: &App) -> Option<(ToastKind, &str)> {
        app.toasts.last().map(|t| (t.kind, t.text.as_str()))
    }

    fn key(app: &mut App, code: KeyCode) -> Option<Cmd> {
        update(app, Msg::Key(KeyEvent::from(code)))
    }

    /// Press a key once the top confirm has been on screen for the arming delay.
    fn armed_key(app: &mut App, code: KeyCode) -> Option<Cmd> {
        if let Some(Overlay::Confirm(c)) = app.overlays.last() {
            c.arming().drawn_long_ago();
        }
        key(app, code)
    }

    #[test]
    fn number_keys_and_tab_switch_tabs() {
        let mut app = app();
        key(&mut app, KeyCode::Char('4'));
        assert_eq!(app.active_tab, Tab::Machines);
        key(&mut app, KeyCode::Tab);
        assert_eq!(app.active_tab, Tab::Config);
        key(&mut app, KeyCode::Char('6'));
        assert_eq!(app.active_tab, Tab::Security);
        key(&mut app, KeyCode::Tab);
        assert_eq!(app.active_tab, Tab::Overview);
        key(&mut app, KeyCode::Char('9'));
        assert_eq!(app.active_tab, Tab::Overview);
    }

    #[test]
    fn escape_closes_help_and_never_quits() {
        let mut app = app();
        key(&mut app, KeyCode::Char('?'));
        assert!(app.help_open());
        key(&mut app, KeyCode::Esc);
        assert!(!app.help_open());
        for tab in Tab::all() {
            app.active_tab = *tab;
            key(&mut app, KeyCode::Esc);
            assert!(!app.should_quit);
        }
        key(&mut app, KeyCode::Char('q'));
        assert!(app.should_quit);
    }

    #[test]
    fn escape_collapses_what_enter_opened() {
        let mut app = packages_app();
        key(&mut app, KeyCode::Enter);
        assert!(app.packages.expanded.is_some());
        key(&mut app, KeyCode::Esc);
        assert!(app.packages.expanded.is_none());
        assert!(!app.should_quit);

        app.state
            .machines
            .push(crate::sync::MachineState::new("other"));
        app.active_tab = Tab::Machines;
        key(&mut app, KeyCode::Enter);
        assert!(app.machines.expanded.is_some());
        key(&mut app, KeyCode::Esc);
        assert!(app.machines.expanded.is_none());

        app.active_tab = Tab::Security;
        key(&mut app, KeyCode::Enter);
        assert!(app.security.detail);
        key(&mut app, KeyCode::Esc);
        assert!(!app.security.detail);
        assert!(!app.should_quit);
    }

    #[test]
    fn sync_key_starts_one_job_at_a_time() {
        let mut app = app();
        assert!(matches!(
            key(&mut app, KeyCode::Char('s')),
            Some(Cmd::Run(Job::Sync))
        ));
        update(&mut app, Msg::JobStarted(Job::Sync));
        assert!(key(&mut app, KeyCode::Char('s')).is_none());
    }

    #[test]
    fn quit_waits_for_rollback() {
        let mut app = app();
        let job = Job::Rollback {
            manager: "npm".into(),
            commit: "abc123".into(),
            short_hash: "abc".into(),
        };
        update(&mut app, Msg::JobStarted(job));
        assert_eq!(
            last_toast(&app),
            Some((ToastKind::Success, "Rolling back npm to abc"))
        );
        update(
            &mut app,
            Msg::Key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL)),
        );
        assert!(!app.should_quit);
        assert_eq!(last_toast(&app).map(|t| t.0), Some(ToastKind::Error));
    }

    #[test]
    fn daemon_key_waits_for_pending_op() {
        let mut app = app();
        assert!(matches!(
            key(&mut app, KeyCode::Char('d')),
            Some(Cmd::Daemon(DaemonOp::Starting))
        ));
        update(&mut app, Msg::DaemonOpStarted(DaemonOp::Starting));
        assert!(key(&mut app, KeyCode::Char('d')).is_none());
    }

    #[test]
    fn config_refuses_invalid_values_with_the_reason() {
        let mut app = app();
        let mut config = crate::config::Config::default();
        config.packages.allow_scripts = vec!["esbuild".into()];
        app.state.config = Some(config);
        app.active_tab = Tab::Config;
        let field = |key: &str| {
            crate::dashboard::config_edit::fields()
                .iter()
                .position(|f| f.key == key)
                .unwrap()
        };
        app.config.selected = field("interval");
        key(&mut app, KeyCode::Enter);
        app.config.edit_buf = "often".into();
        key(&mut app, KeyCode::Enter);
        // The field stays open with the value, and the toast says why
        assert!(app.config.editing);
        assert_eq!(app.config.edit_buf, "often");
        assert_eq!(
            last_toast(&app),
            Some((
                ToastKind::Error,
                "Sync interval needs a number and s, m or h, such as 5m"
            ))
        );
        key(&mut app, KeyCode::Esc);
        assert!(!app.config.editing);

        // A list item is removed with x, after a question
        app.config.selected = field("allow_scripts");
        key(&mut app, KeyCode::Enter);
        key(&mut app, KeyCode::Char('x'));
        assert!(matches!(
            app.overlays.last(),
            Some(Overlay::Confirm(Confirm::RemoveListItem { .. }))
        ));
        assert!(armed_key(&mut app, KeyCode::Enter).is_none());
        assert!(app.overlays.is_empty());
        assert_eq!(
            app.state.config.as_ref().unwrap().packages.allow_scripts,
            vec!["esbuild".to_string()]
        );
        key(&mut app, KeyCode::Esc);
        assert!(app.config.list_edit.is_none());
        assert!(!app.should_quit);
    }

    /// Keys a handler takes, among every printable key and the common special keys.
    fn handled(
        fixture: impl Fn() -> App,
        handler: fn(&mut App, KeyEvent) -> KeyOutcome,
        modifiers: KeyModifiers,
    ) -> BTreeSet<String> {
        use KeyCode::*;
        let mut codes: Vec<KeyCode> = (' '..='~').map(Char).collect();
        codes.extend([
            Enter, Esc, Tab, BackTab, Backspace, Delete, Up, Down, Left, Right, PageUp, PageDown,
            Home, End,
        ]);
        codes
            .into_iter()
            .filter(|code| {
                let mut app = fixture();
                matches!(
                    handler(&mut app, KeyEvent::new(*code, modifiers)),
                    KeyOutcome::Handled(_)
                )
            })
            .map(|code| format!("{:?}", code))
            .collect()
    }

    fn listed(
        bindings: &[crate::dashboard::components::keymap::Binding],
        ctrl: bool,
    ) -> BTreeSet<String> {
        bindings
            .iter()
            .filter(|b| b.ctrl == ctrl)
            .flat_map(|b| b.codes.iter().map(|code| format!("{:?}", code)))
            .collect()
    }

    /// The footer and help come from the keymap, so it must list exactly the keys each
    /// handler takes.
    #[test]
    fn keymap_lists_exactly_the_handled_keys() {
        use crate::dashboard::components::keymap;

        for tab in Tab::all() {
            let fixture = || {
                let mut app = app();
                app.active_tab = *tab;
                app
            };
            assert_eq!(
                handled(fixture, tab_key, KeyModifiers::NONE),
                listed(keymap::tab(*tab), false),
                "{:?}",
                tab
            );
        }
        let list_fixture = || {
            let mut app = app();
            let mut config = crate::config::Config::default();
            // An empty list, so no probed key can save the config
            config.dotfiles.files.clear();
            app.state.config = Some(config);
            app.active_tab = Tab::Config;
            app.config.selected = crate::dashboard::config_edit::fields()
                .iter()
                .position(|f| f.key == "dotfiles.files")
                .unwrap();
            key(&mut app, KeyCode::Enter);
            assert!(app.config.list_edit.is_some());
            app
        };
        assert_eq!(
            handled(list_fixture, tab_key, KeyModifiers::NONE),
            listed(keymap::CONFIG_LIST, false)
        );
        assert_eq!(
            handled(app, global_key, KeyModifiers::NONE),
            listed(keymap::GLOBAL, false)
        );
        assert_eq!(
            handled(app, ctrl_key, KeyModifiers::CONTROL),
            listed(keymap::GLOBAL, true)
        );
    }

    #[test]
    fn footer_keeps_the_help_key_and_the_first_keys_at_every_width() {
        use ratatui::{backend::TestBackend, Terminal};

        let mut app = with_inbox();
        for tab in Tab::all() {
            app.active_tab = *tab;
            for w in [60, 100, 120, 160] {
                let mut terminal = Terminal::new(TestBackend::new(w, 30)).unwrap();
                terminal
                    .draw(|f| crate::dashboard::view::view(f, &app))
                    .unwrap();
                let footer: String = (0..w)
                    .map(|x| terminal.backend().buffer()[(x, 29)].symbol().to_string())
                    .collect();
                assert!(footer.contains("? more"), "{:?} at {}: {}", tab, w, footer);
                if w >= 100 {
                    assert!(footer.contains("s sync") && footer.contains("q quit"));
                }
            }
        }
    }

    #[test]
    fn daemon_log_opens_read_only_and_scrolls() {
        use crate::dashboard::components::log_view::LogView;
        use ratatui::{backend::TestBackend, Terminal};

        let mut app = app();
        key(&mut app, KeyCode::Enter);
        assert!(matches!(app.overlays.last(), Some(Overlay::Log(_))));
        app.overlays.clear();
        assert!(palette::entries(&app)
            .iter()
            .any(|e| e.target == Target::Action(Action::DaemonLog)));

        app.overlays.push(Overlay::Log(LogView {
            lines: (0..100).map(|i| format!("✓ line {}", i)).collect(),
            from_end: 0,
            rows: std::cell::Cell::new(1),
        }));
        let mut terminal = Terminal::new(TestBackend::new(100, 30)).unwrap();
        terminal
            .draw(|f| crate::dashboard::view::view(f, &app))
            .unwrap();
        assert!(screen(&terminal).contains("line 99"));
        for code in [KeyCode::Char('g'), KeyCode::Char('s'), KeyCode::Char('d')] {
            // Keys that would act elsewhere do nothing here
            assert!(key(&mut app, code).is_none());
        }
        terminal
            .draw(|f| crate::dashboard::view::view(f, &app))
            .unwrap();
        let text = screen(&terminal);
        assert!(text.contains("line 0") && !text.contains("line 99"));
        key(&mut app, KeyCode::Esc);
        assert!(app.overlays.is_empty());
        assert!(!app.should_quit);
    }

    #[test]
    fn backup_restores_only_after_y() {
        use crate::dashboard::components::backup_picker::BackupPicker;

        let mut app = app();
        app.active_tab = Tab::Files;
        app.overlays.push(Overlay::BackupPicker(BackupPicker {
            path: ".zshrc".into(),
            backups: vec!["2026-10-03T14-55-00".into(), "2026-10-01T09-00-00".into()],
            cursor: 0,
        }));
        key(&mut app, KeyCode::Char('j'));
        // Enter picks the backup and asks; a second Enter cancels
        assert!(key(&mut app, KeyCode::Enter).is_none());
        assert!(matches!(
            app.overlays.last(),
            Some(Overlay::Confirm(Confirm::RestoreBackup { .. }))
        ));
        assert!(armed_key(&mut app, KeyCode::Enter).is_none());
        assert!(app.overlays.is_empty());

        app.overlays.push(Overlay::Confirm(Confirm::RestoreBackup {
            path: ".zshrc".into(),
            timestamp: "2026-10-01T09-00-00".into(),
            arming: Default::default(),
        }));
        let Some(Cmd::RestoreBackup { path, timestamp }) = armed_key(&mut app, KeyCode::Char('y'))
        else {
            panic!("expected a restore command");
        };
        assert_eq!(
            (path.as_str(), timestamp.as_str()),
            (".zshrc", "2026-10-01T09-00-00")
        );
    }

    #[test]
    fn daemon_key_asks_before_it_stops() {
        let mut app = app();
        app.state.daemon_running = true;
        assert!(key(&mut app, KeyCode::Char('d')).is_none());
        assert!(matches!(
            app.overlays.last(),
            Some(Overlay::Confirm(Confirm::StopDaemon { .. }))
        ));
        assert!(armed_key(&mut app, KeyCode::Enter).is_none());
        assert!(app.overlays.is_empty());
        key(&mut app, KeyCode::Char('d'));
        assert!(matches!(
            armed_key(&mut app, KeyCode::Char('y')),
            Some(Cmd::Daemon(DaemonOp::Stopping))
        ));
    }

    #[test]
    fn confirm_overlay_takes_keys_until_answered() {
        let mut app = app();
        app.overlays.push(Overlay::Confirm(Confirm::Uninstall {
            manager_key: "npm".into(),
            name: "left-pad".into(),
            arming: Default::default(),
        }));
        key(&mut app, KeyCode::Char('2'));
        assert_eq!(app.active_tab, Tab::Overview);
        assert_eq!(app.overlays.len(), 1);
        // Not drawn yet, so not armed: `y` typed for something else does not uninstall
        assert!(key(&mut app, KeyCode::Char('y')).is_none());
        let cmd = armed_key(&mut app, KeyCode::Char('y'));
        assert!(matches!(cmd, Some(Cmd::Uninstall { .. })));
        assert!(app.overlays.is_empty());
        assert_eq!(
            app.uninstalling,
            Some(("npm".to_string(), "left-pad".to_string()))
        );
        update(&mut app, Msg::UninstallDone(Err("boom".into())));
        assert!(app.uninstalling.is_none());
        assert_eq!(
            last_toast(&app),
            Some((ToastKind::Error, "uninstall failed: boom"))
        );
        // The package is gone, so the sync still runs; the toast names what did not save
        let cmd = update(
            &mut app,
            Msg::UninstallDone(Ok(Some("profiles not saved".into()))),
        );
        assert!(matches!(cmd, Some(Cmd::Run(Job::Sync))));
        assert_eq!(
            last_toast(&app),
            Some((ToastKind::Error, "profiles not saved"))
        );
    }

    #[test]
    fn sync_refused_by_busy_runtime_runs_after_job() {
        let mut app = app();
        update(&mut app, Msg::JobStarted(Job::Sync));
        update(&mut app, Msg::JobSpawnFailed(Job::Sync));
        let cmd = update(
            &mut app,
            Msg::JobExited {
                job: Job::Sync,
                success: true,
            },
        );
        assert!(matches!(cmd, Some(Cmd::Run(Job::Sync))));
    }

    #[test]
    fn follow_up_sync_waits_for_running_job() {
        let mut app = app();
        update(&mut app, Msg::JobStarted(Job::Sync));
        app.uninstalling = Some(("npm".into(), "left-pad".into()));
        assert!(update(&mut app, Msg::UninstallDone(Ok(None))).is_none());
        let cmd = update(
            &mut app,
            Msg::JobExited {
                job: Job::Sync,
                success: true,
            },
        );
        assert!(matches!(cmd, Some(Cmd::Run(Job::Sync))));
        assert!(!app.sync_pending);
    }

    #[test]
    fn local_packages_show_without_writing_the_record() {
        let mut app = app();
        app.state.sync_state = Some(
            serde_json::from_value(serde_json::json!({
                "machine_id": "me",
                "last_sync": "2026-01-01T00:00:00Z",
                "files": {},
                "packages": {},
            }))
            .unwrap(),
        );
        let mut record = crate::sync::MachineState::new("me");
        record.packages.insert("npm".into(), vec!["old".into()]);
        app.state.machines = vec![record.clone()];
        let live = HashMap::from([("npm".to_string(), vec!["new".to_string()])]);
        assert!(update(&mut app, Msg::LocalPackages(live.clone())).is_none());
        assert_eq!(app.state.machines[0].packages, live);
        // A reload reads the record again; the live list still shows
        app.state.machines = vec![record.clone()];
        app.show_local_packages();
        assert_eq!(app.state.machines[0].packages, live);
        // A later sync wrote the record from the managers, so the record shows
        record.last_sync = chrono::Utc::now() + chrono::Duration::seconds(1);
        app.state.machines = vec![record.clone()];
        app.show_local_packages();
        assert_eq!(app.state.machines[0].packages, record.packages);
        assert!(app.local_packages.is_none());
    }

    /// This machine "me" with npm package zx, in profile dev of profiles dev and server.
    fn packages_app() -> App {
        let mut app = app();
        app.state.sync_state = Some(
            serde_json::from_value(serde_json::json!({
                "machine_id": "me",
                "last_sync": "2026-01-01T00:00:00Z",
                "files": {},
                "packages": {},
            }))
            .unwrap(),
        );
        let mut config = crate::config::Config::default();
        for name in ["dev", "server"] {
            config
                .profiles
                .insert(name.to_string(), crate::config::ProfileConfig::default());
        }
        let mut record = crate::sync::MachineState::new("me");
        record.packages.insert("npm".into(), vec!["zx".into()]);
        app.state.membership = Some(crate::sync::membership::Membership::new(
            &config,
            &Default::default(),
            &record,
            &[],
        ));
        app.state.config = Some(config);
        app.state.machines = vec![record];
        app.active_tab = Tab::Packages;
        app
    }

    #[test]
    fn enter_and_double_click_never_uninstall() {
        let mut app = packages_app();
        key(&mut app, KeyCode::Enter);
        key(&mut app, KeyCode::Char('j'));
        assert!(key(&mut app, KeyCode::Enter).is_none());
        app.add_hit(Rect::new(0, 5, 40, 1), Hit::Row(1));
        click(&mut app, 2, 5);
        assert!(app.overlays.is_empty());

        key(&mut app, KeyCode::Char('x'));
        assert!(matches!(
            app.overlays.last(),
            Some(Overlay::Confirm(Confirm::Uninstall { .. }))
        ));
        draw(&app);
        // Enter cancels even once armed
        assert!(armed_key(&mut app, KeyCode::Enter).is_none());
        assert!(app.overlays.is_empty());
        key(&mut app, KeyCode::Char('x'));
        assert!(matches!(
            armed_key(&mut app, KeyCode::Char('y')),
            Some(Cmd::Uninstall { .. })
        ));
    }

    #[test]
    fn t_on_a_package_opens_its_profile_checklist() {
        let mut app = packages_app();
        key(&mut app, KeyCode::Enter);
        key(&mut app, KeyCode::Char('j'));
        key(&mut app, KeyCode::Char('t'));
        let Some(Overlay::PackageProfiles(picker)) = app.overlays.last() else {
            panic!("expected the profile checklist");
        };
        assert_eq!(picker.name, "zx");
        assert_eq!(picker.checked, BTreeSet::from(["dev".to_string()]));
        key(&mut app, KeyCode::Char('j'));
        key(&mut app, KeyCode::Char(' '));
        let Some(Overlay::PackageProfiles(picker)) = app.overlays.last() else {
            panic!("expected the profile checklist");
        };
        assert_eq!(
            picker.checked,
            BTreeSet::from(["dev".to_string(), "server".to_string()])
        );
        key(&mut app, KeyCode::Esc);
        assert!(app.overlays.is_empty());
    }

    #[test]
    fn second_install_waits_for_the_first() {
        let mut app = app();
        let Some(Cmd::Install { op: first, .. }) =
            app.start_install("npm".into(), "left-pad".into(), true)
        else {
            panic!("expected an install command");
        };
        assert!(app.start_install("npm".into(), "zx".into(), true).is_none());
        assert_eq!(
            last_toast(&app),
            Some((ToastKind::Error, "Install in progress"))
        );
        assert_eq!(app.installing.as_ref(), Some(&first));
        let stale = InstallOp {
            id: first.id + 1,
            ..first.clone()
        };
        update(
            &mut app,
            Msg::InstallDone {
                op: stale,
                result: Ok(()),
            },
        );
        assert!(app.installing.is_some());
        update(
            &mut app,
            Msg::InstallDone {
                op: first,
                result: Err("boom".into()),
            },
        );
        assert!(app.installing.is_none());
    }

    #[test]
    fn unreachable_osv_asks_and_defaults_to_no() {
        let mut app = app();
        let Some(Cmd::Install { op, .. }) = app.start_install("npm".into(), "zx".into(), true)
        else {
            panic!("expected an install command");
        };
        let unreachable = |op: InstallOp| Msg::OsvUnreachable {
            op,
            error: "timeout".into(),
        };
        update(&mut app, unreachable(op.clone()));
        assert!(app.installing.is_none());
        assert!(armed_key(&mut app, KeyCode::Enter).is_none());
        assert!(app.overlays.is_empty());

        let Some(Cmd::Install { op, .. }) = app.start_install("npm".into(), "zx".into(), true)
        else {
            panic!("expected an install command");
        };
        update(&mut app, unreachable(op));
        // A `y` typed before the question was drawn, or just after, does not answer it
        assert!(key(&mut app, KeyCode::Char('y')).is_none());
        draw(&app);
        assert!(app.animating());
        assert!(key(&mut app, KeyCode::Char('y')).is_none());
        assert!(matches!(
            app.overlays.last(),
            Some(Overlay::Confirm(Confirm::InstallWithoutOsv { .. }))
        ));
        let cmd = armed_key(&mut app, KeyCode::Char('y'));
        assert!(matches!(
            cmd,
            Some(Cmd::Install {
                osv_required: false,
                ..
            })
        ));
    }

    #[test]
    fn package_warnings_become_toasts_while_the_dashboard_runs() {
        let mut app = app();
        crate::cli::Output::capture_warnings(true);
        crate::cli::Output::warning("OSV check incomplete for 1 packages: timeout");
        update(&mut app, Msg::Tick);
        crate::cli::Output::capture_warnings(false);
        assert!(app
            .toasts
            .iter()
            .any(|t| t.text == "OSV check incomplete for 1 packages: timeout"));
    }

    #[test]
    fn tick_expires_old_toasts() {
        let mut app = app();
        app.flash_success("fresh");
        app.toasts.insert(
            0,
            Toast::new(
                ToastKind::Info,
                "old".into(),
                Instant::now() - Duration::from_secs(4),
            ),
        );
        update(&mut app, Msg::Tick);
        assert_eq!(app.toasts.len(), 1);
        assert_eq!(last_toast(&app), Some((ToastKind::Success, "fresh")));
    }

    #[test]
    fn toasts_are_capped() {
        let mut app = app();
        for i in 0..10 {
            app.flash_info(format!("n{}", i));
        }
        assert_eq!(app.toasts.len(), MAX_TOASTS);
        assert_eq!(last_toast(&app), Some((ToastKind::Info, "n9")));
    }

    #[test]
    fn ctrl_k_opens_palette_and_runs_tab_jump() {
        let mut app = app();
        update(
            &mut app,
            Msg::Key(KeyEvent::new(KeyCode::Char('k'), KeyModifiers::CONTROL)),
        );
        assert!(matches!(app.overlays.last(), Some(Overlay::Palette(_))));
        for c in "machines".chars() {
            key(&mut app, KeyCode::Char(c));
        }
        // Typed letters reach the palette, not the global keymap.
        assert_eq!(app.active_tab, Tab::Overview);
        key(&mut app, KeyCode::Enter);
        assert!(app.overlays.is_empty());
        assert_eq!(app.active_tab, Tab::Machines);
    }

    #[test]
    fn palette_closes_on_escape() {
        let mut app = app();
        update(
            &mut app,
            Msg::Key(KeyEvent::new(KeyCode::Char('k'), KeyModifiers::CONTROL)),
        );
        key(&mut app, KeyCode::Esc);
        assert!(app.overlays.is_empty());
        assert!(!app.should_quit);
    }

    fn click(app: &mut App, x: u16, y: u16) -> Option<Cmd> {
        update(
            app,
            Msg::Mouse(MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: x,
                row: y,
                modifiers: KeyModifiers::NONE,
            }),
        )
    }

    #[test]
    fn clicks_hit_recorded_regions() {
        let mut app = app();
        app.add_hit(Rect::new(10, 1, 8, 1), Hit::Tab(Tab::Config));
        click(&mut app, 12, 1);
        assert_eq!(app.active_tab, Tab::Config);
        click(&mut app, 30, 1);
        assert_eq!(app.active_tab, Tab::Config);

        app.active_tab = Tab::Files;
        app.add_hit(Rect::new(0, 5, 40, 1), Hit::Row(3));
        click(&mut app, 2, 5);
        assert_eq!(app.files.cursor, 3);
    }

    #[test]
    fn second_click_before_redraw_hits_nothing() {
        let mut app = app();
        app.add_hit(
            Rect::new(0, 0, 5, 1),
            Hit::Key(KeyEvent::from(KeyCode::Char('?'))),
        );
        click(&mut app, 1, 0);
        click(&mut app, 1, 0);
        assert!(app.help_open());
    }

    #[test]
    fn row_click_cannot_answer_a_confirm() {
        let mut app = app();
        app.active_tab = Tab::Files;
        app.overlays.push(Overlay::Confirm(Confirm::Uninstall {
            manager_key: "npm".into(),
            name: "left-pad".into(),
            arming: Default::default(),
        }));
        app.add_hit(Rect::new(0, 5, 40, 1), Hit::Row(0));
        assert!(click(&mut app, 2, 5).is_none());
        assert_eq!(app.overlays.len(), 1);
        assert!(app.uninstalling.is_none());
    }

    #[test]
    fn help_and_toasts_take_clicks_before_the_tab() {
        use ratatui::{backend::TestBackend, Terminal};

        let mut app = app();
        app.active_tab = Tab::Config;
        run_action(&mut app, Action::Help);
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
        terminal
            .draw(|f| crate::dashboard::view::view(f, &app))
            .unwrap();
        assert_eq!(app.hit_at(40, 12), Some(Hit::Block));
        click(&mut app, 40, 12);
        assert!(app.help_open());
        assert_eq!(app.config.selected, 0);
        update(
            &mut app,
            Msg::Mouse(MouseEvent {
                kind: MouseEventKind::ScrollDown,
                column: 40,
                row: 12,
                modifiers: KeyModifiers::NONE,
            }),
        );
        assert_eq!(app.config.selected, 0);
        terminal
            .draw(|f| crate::dashboard::view::view(f, &app))
            .unwrap();
        click(&mut app, 0, 23);
        assert!(!app.help_open());

        app.flash_info("hello");
        // Past the slide-in, so the toast is fully on screen.
        app.toasts[0].born = Instant::now() - Duration::from_secs(1);
        terminal
            .draw(|f| crate::dashboard::view::view(f, &app))
            .unwrap();
        let (x, y) = (0..80u16)
            .flat_map(|x| (0..24u16).map(move |y| (x, y)))
            .find(|&(x, y)| app.hit_at(x, y) == Some(Hit::Toast(0)))
            .expect("toast is clickable");
        click(&mut app, x, y);
        assert!(app.toasts.is_empty());
    }

    #[test]
    fn clicked_key_hint_acts_like_the_key() {
        let mut app = app();
        app.add_hit(
            Rect::new(0, 0, 5, 1),
            Hit::Key(KeyEvent::from(KeyCode::Char('?'))),
        );
        click(&mut app, 1, 0);
        assert!(app.help_open());
    }

    #[test]
    fn wheel_scrolls_like_arrow_keys() {
        let mut app = app();
        app.active_tab = Tab::Config;
        let wheel = |kind| {
            Msg::Mouse(MouseEvent {
                kind,
                column: 0,
                row: 0,
                modifiers: KeyModifiers::NONE,
            })
        };
        update(&mut app, wheel(MouseEventKind::ScrollDown));
        update(&mut app, wheel(MouseEventKind::ScrollDown));
        update(&mut app, wheel(MouseEventKind::ScrollUp));
        assert_eq!(app.config.selected, 1);
    }

    #[test]
    fn install_confirm_draws_over_the_picker() {
        use crate::dashboard::components::pkg_import::{PkgImport, PkgImportItem};
        use ratatui::{backend::TestBackend, Terminal};

        let mut app = app();
        app.overlays.push(Overlay::PkgImport(PkgImport {
            items: (0..20)
                .map(|i| PkgImportItem {
                    manager_key: "npm".into(),
                    name: format!("package-with-a-long-name-{}", i),
                    sources: vec!["other".into()],
                })
                .collect(),
            cursor: 0,
            confirm: Some(("npm".into(), "left-pad".into())),
        }));
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
        terminal
            .draw(|f| crate::dashboard::view::view(f, &app))
            .unwrap();
        let screen: String = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|c| c.symbol())
            .collect();
        assert!(screen.contains("Install left-pad (npm)?"));

        // Enter opened the question, so Enter never answers it
        assert!(key(&mut app, KeyCode::Enter).is_none());
        let Some(Overlay::PkgImport(picker)) = app.overlays.last_mut() else {
            panic!("expected the picker");
        };
        assert!(picker.confirm.is_none());
        key(&mut app, KeyCode::Enter);
        assert!(matches!(
            key(&mut app, KeyCode::Char('y')),
            Some(Cmd::Install { .. })
        ));
    }

    fn inbox_item(name: &str, reasons: Vec<Reason>) -> InboxItem {
        InboxItem {
            kind: Kind::Package,
            signer: None,
            manager: "npm".into(),
            name: name.into(),
            version: Some("1.0.0".into()),
            tap: None,
            source_machine: Some("other".into()),
            commit: Some("abc1234def".into()),
            reasons,
            advisories: vec!["MAL-2025-1".into(), "GHSA-xxxx".into()],
            first_seen: chrono::Utc::now(),
        }
    }

    fn with_inbox() -> App {
        let mut app = app();
        app.state.inbox.items = vec![
            inbox_item("evil", vec![Reason::Malicious, Reason::Unsigned]),
            inbox_item(
                "left-pad",
                vec![Reason::Unsigned, Reason::CooldownUnsupported],
            ),
        ];
        app
    }

    fn screen(terminal: &ratatui::Terminal<ratatui::backend::TestBackend>) -> String {
        terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|c| c.symbol())
            .collect()
    }

    #[test]
    fn security_tab_renders_inbox_at_every_size() {
        use ratatui::{backend::TestBackend, Terminal};

        let mut app = with_inbox();
        app.active_tab = Tab::Security;
        for detail in [false, true] {
            app.security.detail = detail;
            for (w, h) in [(10, 4), (20, 6), (80, 24), (160, 48), (200, 60)] {
                let mut terminal = Terminal::new(TestBackend::new(w, h)).unwrap();
                terminal
                    .draw(|f| crate::dashboard::view::view(f, &app))
                    .unwrap();
            }
        }
        let mut terminal = Terminal::new(TestBackend::new(160, 48)).unwrap();
        terminal
            .draw(|f| crate::dashboard::view::view(f, &app))
            .unwrap();
        let text = screen(&terminal);
        assert!(text.contains("malicious"));
        assert!(text.contains("Inbox 2"));
        assert!(text.contains("approval is blocked"));

        app.state.inbox.items.clear();
        terminal
            .draw(|f| crate::dashboard::view::view(f, &app))
            .unwrap();
        assert!(screen(&terminal).contains("This machine is clean"));
    }

    #[test]
    fn pending_badge_opens_security_tab() {
        use ratatui::{backend::TestBackend, Terminal};

        let mut app = with_inbox();
        let mut terminal = Terminal::new(TestBackend::new(160, 48)).unwrap();
        terminal
            .draw(|f| crate::dashboard::view::view(f, &app))
            .unwrap();
        let text = screen(&terminal);
        let first_row: String = text.chars().take(160).collect();
        let x = first_row
            .find("Inbox 2")
            .map(|b| first_row[..b].chars().count());
        click(&mut app, x.unwrap() as u16, 0);
        assert_eq!(app.active_tab, Tab::Security);
    }

    #[test]
    fn security_tab_groups_a_flood_by_machine() {
        use ratatui::{backend::TestBackend, Terminal};

        let mut app = app();
        app.active_tab = Tab::Security;
        let from = |machine: &str, name: String| InboxItem {
            source_machine: Some(machine.into()),
            advisories: Vec::new(),
            ..inbox_item(&name, vec![Reason::Unsigned])
        };
        let mut items: Vec<InboxItem> = (0..200)
            .map(|i| from("laptop", format!("gem{i}")))
            .chain((0..80).map(|i| from("desk", format!("npm{i}"))))
            .collect();
        items.push(inbox_item(
            "evil",
            vec![Reason::Malicious, Reason::Unsigned],
        ));
        crate::packages::inbox::sort_by_group(&mut items);
        app.state.inbox.items = items;
        app.state.old_builds = vec!["oldbox".into()];

        let mut terminal = Terminal::new(TestBackend::new(160, 48)).unwrap();
        terminal
            .draw(|f| crate::dashboard::view::view(f, &app))
            .unwrap();
        let text = screen(&terminal);
        assert!(text.contains("281 pending in 3 groups"));
        assert!(text.contains("200 items"));
        assert!(text.contains("80 items"));
        assert!(text.contains("oldbox on 1.x"));

        // The cursor starts on desk's first package; approve all from desk
        key(&mut app, KeyCode::Char('M'));
        let (names, held) = approve_all_names(&app).unwrap();
        assert_eq!((names.len(), held), (80, 0));
        assert!(names.iter().all(|n| n.starts_with("npm")));
        app.overlays.clear();
        // other's malicious package stays out
        let evil = app
            .state
            .inbox
            .items
            .iter()
            .position(|i| i.name == "evil")
            .unwrap();
        security::move_cursor(&mut app, evil);
        key(&mut app, KeyCode::Char('M'));
        assert!(approve_all_names(&app).is_none());
    }

    #[test]
    fn security_keys_move_and_open_details() {
        let mut app = with_inbox();
        app.active_tab = Tab::Security;
        key(&mut app, KeyCode::Char('j'));
        key(&mut app, KeyCode::Char('j'));
        assert_eq!(app.security.cursor, 1);
        key(&mut app, KeyCode::Enter);
        assert!(app.security.detail);
        app.installing = Some(InstallOp {
            id: 9,
            manager_key: "npm".into(),
            name: "busy".into(),
        });
        // Approval waits for the running install, so the inbox stays as it was.
        assert!(key(&mut app, KeyCode::Char('a')).is_none());
        key(&mut app, KeyCode::Char('A'));
        assert!(app.overlays.is_empty());
        assert_eq!(app.state.inbox.items.len(), 2);
    }

    fn approve_all_names(app: &App) -> Option<(Vec<String>, usize)> {
        match app.overlays.last() {
            Some(Overlay::Confirm(Confirm::ApproveAll { items, held, .. })) => {
                Some((items.iter().map(|i| i.name.clone()).collect(), *held))
            }
            _ => None,
        }
    }

    fn approved_names(cmd: Option<Cmd>) -> Vec<String> {
        match cmd {
            Some(Cmd::ApprovePackages { items, .. }) => items.into_iter().map(|i| i.name).collect(),
            _ => Vec::new(),
        }
    }

    #[test]
    fn approve_all_confirm_counts_only_safe_items() {
        let mut app = with_inbox();
        app.active_tab = Tab::Security;
        key(&mut app, KeyCode::Char('A'));
        assert_eq!(
            approve_all_names(&app),
            Some((vec!["left-pad".to_string()], 1))
        );
    }

    #[test]
    fn approve_all_approves_only_the_items_it_showed() {
        let mut app = with_inbox();
        app.active_tab = Tab::Security;
        key(&mut app, KeyCode::Char('A'));
        // A sync adds an item while the confirm is open
        app.state
            .inbox
            .items
            .push(inbox_item("late", vec![Reason::Unsigned]));
        let cmd = armed_key(&mut app, KeyCode::Char('y'));
        assert_eq!(approved_names(cmd), vec!["left-pad"]);
    }

    #[test]
    fn approve_asks_before_it_installs_without_osv() {
        let mut app = with_inbox();
        app.active_tab = Tab::Security;
        key(&mut app, KeyCode::Char('A'));
        let Some(Cmd::ApprovePackages {
            op,
            items,
            osv_required: true,
        }) = armed_key(&mut app, KeyCode::Char('y'))
        else {
            panic!("expected an approval that requires OSV");
        };
        let unreachable = |op: InstallOp| Msg::ApproveOsvUnreachable {
            op,
            result: Ok(()),
            unchecked: items
                .iter()
                .map(|i| (i.clone(), "timeout".into()))
                .collect(),
        };
        // The sync for the items OSV did check waits for the answer: it would hold the sync
        // lock that `y` needs
        assert!(update(&mut app, unreachable(op)).is_none());
        assert!(app.installing.is_none());
        assert!(matches!(
            armed_key(&mut app, KeyCode::Enter),
            Some(Cmd::Run(Job::Sync))
        ));
        assert!(app.overlays.is_empty());

        let Some(Cmd::ApprovePackages { op, .. }) =
            security::approve_all(&mut app, items.clone(), true)
        else {
            panic!("expected an approval");
        };
        update(&mut app, unreachable(op));
        let cmd = armed_key(&mut app, KeyCode::Char('y'));
        assert!(matches!(
            cmd,
            Some(Cmd::ApprovePackages {
                osv_required: false,
                ..
            })
        ));
        assert_eq!(approved_names(cmd), vec!["left-pad"]);
    }

    #[test]
    fn approve_without_osv_shows_each_package_error() {
        let mut app = with_inbox();
        let Some(Cmd::ApprovePackages { op, .. }) = security::approve_all(
            &mut app,
            vec![
                inbox_item("aaa", vec![Reason::Unsigned]),
                inbox_item("bbb", vec![Reason::Unsigned]),
            ],
            true,
        ) else {
            panic!("expected an approval");
        };
        update(
            &mut app,
            Msg::ApproveOsvUnreachable {
                op,
                result: Ok(()),
                unchecked: vec![
                    (inbox_item("aaa", vec![]), "MALICIOUS releases".into()),
                    (inbox_item("bbb", vec![]), "timeout".into()),
                ],
            },
        );
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(200, 48)).unwrap();
        terminal
            .draw(|f| crate::dashboard::view::view(f, &app))
            .unwrap();
        let text: String = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|c| c.symbol())
            .collect();
        // A timeout on one package does not hide the malicious-releases warning on another
        assert!(text.contains("aaa (npm): MALICIOUS releases"));
        assert!(text.contains("bbb (npm): timeout"));
    }

    #[test]
    fn approve_all_lists_every_item_it_approves() {
        let mut app = with_inbox();
        app.active_tab = Tab::Security;
        for i in 0..40 {
            app.state
                .inbox
                .items
                .push(inbox_item(&format!("pkg{:02}", i), vec![Reason::Unsigned]));
        }
        key(&mut app, KeyCode::Char('A'));
        let mut shown = std::collections::BTreeSet::new();
        for _ in 0..50 {
            let mut terminal =
                ratatui::Terminal::new(ratatui::backend::TestBackend::new(100, 24)).unwrap();
            terminal
                .draw(|f| crate::dashboard::view::view(f, &app))
                .unwrap();
            let buffer = terminal.backend().buffer().clone();
            let text: String = buffer.content().iter().map(|c| c.symbol()).collect();
            for name in std::iter::once("left-pad".to_string())
                .chain((0..40).map(|i| format!("pkg{:02}", i)))
            {
                if text.contains(&format!("{} 1.0.0 (npm)", name)) {
                    shown.insert(name);
                }
            }
            key(&mut app, KeyCode::Char('j'));
        }
        let cmd = armed_key(&mut app, KeyCode::Char('y'));
        let approved: std::collections::BTreeSet<String> =
            approved_names(cmd).into_iter().collect();
        assert_eq!(approved.len(), 41);
        assert_eq!(shown, approved);
    }

    fn draw(app: &App) {
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(160, 48)).unwrap();
        terminal
            .draw(|f| crate::dashboard::view::view(f, app))
            .unwrap();
    }

    #[test]
    fn reload_keeps_the_selection_on_its_item() {
        let mut app = with_inbox();
        app.active_tab = Tab::Security;
        key(&mut app, KeyCode::Char('j'));
        draw(&app);
        // A reload puts a new item first, so the old index now holds another item
        app.state
            .inbox
            .items
            .insert(0, inbox_item("new", vec![Reason::Unsigned]));
        security::reselect(&mut app);
        assert_eq!(app.security.cursor, 2);
        let cmd = key(&mut app, KeyCode::Char('a'));
        assert_eq!(approved_names(cmd), vec!["left-pad"]);
    }

    #[test]
    fn item_replaced_since_the_last_draw_is_not_approved() {
        let mut app = with_inbox();
        app.active_tab = Tab::Security;
        key(&mut app, KeyCode::Char('j'));
        // Moving the cursor alone does not show the item
        assert!(key(&mut app, KeyCode::Char('a')).is_none());
        draw(&app);
        // A reload replaces the version under the same id before the next draw
        app.state.inbox.items[1].version = Some("6.6.6".into());
        security::reselect(&mut app);
        assert!(key(&mut app, KeyCode::Char('a')).is_none());
        assert!(key(&mut app, KeyCode::Char('x')).is_none());
        assert_eq!(last_toast(&app).map(|(k, _)| k), Some(ToastKind::Error));
        draw(&app);
        let Some(Cmd::ApprovePackages { items, .. }) = key(&mut app, KeyCode::Char('a')) else {
            panic!("expected an approval");
        };
        assert_eq!(items[0].version.as_deref(), Some("6.6.6"));
    }

    #[test]
    fn selection_that_left_the_inbox_waits_for_a_draw() {
        use ratatui::{backend::TestBackend, Terminal};

        let mut app = with_inbox();
        app.active_tab = Tab::Security;
        key(&mut app, KeyCode::Char('j'));
        app.state.inbox.items.pop();
        app.state
            .inbox
            .items
            .push(inbox_item("other", vec![Reason::Unsigned]));
        security::reselect(&mut app);
        assert!(key(&mut app, KeyCode::Char('a')).is_none());
        assert!(key(&mut app, KeyCode::Char('x')).is_none());
        let mut terminal = Terminal::new(TestBackend::new(160, 48)).unwrap();
        terminal
            .draw(|f| crate::dashboard::view::view(f, &app))
            .unwrap();
        let cmd = key(&mut app, KeyCode::Char('a'));
        assert_eq!(approved_names(cmd), vec!["other"]);
    }

    #[test]
    fn decisions_go_to_the_runtime_with_the_displayed_item() {
        let mut app = with_inbox();
        app.active_tab = Tab::Security;
        key(&mut app, KeyCode::Char('j'));
        draw(&app);
        // Reject asks first, and Enter does not answer
        assert!(key(&mut app, KeyCode::Char('x')).is_none());
        draw(&app);
        assert!(armed_key(&mut app, KeyCode::Enter).is_none());
        assert!(app.overlays.is_empty());
        key(&mut app, KeyCode::Char('x'));
        let Some(Cmd::Reject(item)) = armed_key(&mut app, KeyCode::Char('y')) else {
            panic!("expected a reject command");
        };
        assert_eq!(item.name, "left-pad");
        // Nothing changes on the UI thread until the runtime reports back
        assert_eq!(app.state.inbox.items.len(), 2);
    }

    #[test]
    fn machine_key_items_render_and_stay_out_of_approve_all() {
        use ratatui::{backend::TestBackend, Terminal};

        let mut app = with_inbox();
        let mut machine = inbox_item("laptop", vec![Reason::KeyChanged]);
        machine.manager = "machine".into();
        machine.kind = Kind::TrustMachine {
            public_key: "ssh-ed25519 AAAA".into(),
            fingerprint: "SHA256:abc".into(),
        };
        app.state.inbox.items.insert(0, machine);
        app.active_tab = Tab::Security;
        let mut terminal = Terminal::new(TestBackend::new(160, 48)).unwrap();
        terminal
            .draw(|f| crate::dashboard::view::view(f, &app))
            .unwrap();
        let text = screen(&terminal);
        assert!(text.contains("key changed"));
        assert!(text.contains("SHA256:abc"));
        assert!(text.contains("trust key"));
        key(&mut app, KeyCode::Char('A'));
        assert_eq!(
            approve_all_names(&app),
            Some((vec!["left-pad".to_string()], 1))
        );
        app.overlays.clear();
        // Trust asks first and shows the whole fingerprint
        assert!(key(&mut app, KeyCode::Char('a')).is_none());
        terminal
            .draw(|f| crate::dashboard::view::view(f, &app))
            .unwrap();
        assert!(screen(&terminal).contains("key      SHA256:abc"));
        let Some(Cmd::TrustKey { item, label }) = armed_key(&mut app, KeyCode::Char('y')) else {
            panic!("expected a trust command");
        };
        assert_eq!((item.name.as_str(), label.as_str()), ("laptop", "laptop"));
    }

    #[test]
    fn approve_all_marks_items_with_advisories() {
        use ratatui::{backend::TestBackend, Terminal};

        let mut app = with_inbox();
        app.state.inbox.items[1].advisories = vec!["GHSA-1".into(), "GHSA-2".into()];
        app.active_tab = Tab::Security;
        key(&mut app, KeyCode::Char('A'));
        let mut terminal = Terminal::new(TestBackend::new(120, 36)).unwrap();
        terminal
            .draw(|f| crate::dashboard::view::view(f, &app))
            .unwrap();
        let text = screen(&terminal);
        assert!(text.contains("1 has OSV advisories"));
        assert!(text.contains("left-pad 1.0.0 (npm)  ▲ 2 advisories"));
    }

    /// This machine "me", and "laptop" whose key this machine trusts.
    fn machines_app() -> App {
        let mut app = app();
        app.state.sync_state = Some(
            serde_json::from_value(serde_json::json!({
                "machine_id": "me",
                "last_sync": "2026-01-01T00:00:00Z",
                "files": {},
                "packages": {},
            }))
            .unwrap(),
        );
        app.state.machines = ["me", "laptop"]
            .map(|id| crate::sync::MachineState {
                hostname: id.into(),
                ..crate::sync::MachineState::new(id)
            })
            .into();
        app.state.trusted = vec![crate::packages::inbox::TrustedMachine {
            machine_id: "laptop".into(),
            fingerprint: "SHA256:0123456789abcdefghijklmnopqrstuvwxyzABCDEFG".into(),
        }];
        app.active_tab = Tab::Machines;
        app
    }

    #[test]
    fn profile_is_set_only_on_this_machines_card() {
        let mut app = machines_app();
        let mut config = crate::config::Config::default();
        config
            .profiles
            .insert("dev".into(), crate::config::ProfileConfig::default());
        app.state.config = Some(config);
        key(&mut app, KeyCode::Char('l'));
        key(&mut app, KeyCode::Char('p'));
        assert!(app.overlays.is_empty());
        assert_eq!(
            last_toast(&app),
            Some((ToastKind::Info, "Set the profile of laptop on that machine"))
        );
        key(&mut app, KeyCode::Char('h'));
        key(&mut app, KeyCode::Char('p'));
        assert!(matches!(
            app.overlays.last(),
            Some(Overlay::ProfilePicker(_))
        ));
    }

    #[test]
    fn machine_details_show_the_full_fingerprint() {
        use ratatui::{backend::TestBackend, Terminal};

        let mut app = machines_app();
        key(&mut app, KeyCode::Char('l'));
        key(&mut app, KeyCode::Enter);
        let mut terminal = Terminal::new(TestBackend::new(100, 30)).unwrap();
        terminal
            .draw(|f| crate::dashboard::view::view(f, &app))
            .unwrap();
        assert!(screen(&terminal)
            .contains("trusted  SHA256:0123456789abcdefghijklmnopqrstuvwxyzABCDEFG"));
    }

    #[test]
    fn machines_tab_untrusts_after_y_with_the_full_fingerprint() {
        use ratatui::{backend::TestBackend, Terminal};

        let mut app = machines_app();
        // This machine's own key is never untrusted or trusted again
        assert!(key(&mut app, KeyCode::Char('x')).is_none());
        assert!(key(&mut app, KeyCode::Char('a')).is_none());
        assert!(app.overlays.is_empty());
        key(&mut app, KeyCode::Char('l'));
        key(&mut app, KeyCode::Char('a'));
        assert!(app.overlays.is_empty());
        assert_eq!(
            last_toast(&app),
            Some((ToastKind::Info, "laptop is trusted already"))
        );
        key(&mut app, KeyCode::Char('x'));
        let mut terminal = Terminal::new(TestBackend::new(100, 30)).unwrap();
        terminal
            .draw(|f| crate::dashboard::view::view(f, &app))
            .unwrap();
        assert!(screen(&terminal)
            .contains("key      SHA256:0123456789abcdefghijklmnopqrstuvwxyzABCDEFG"));
        assert!(armed_key(&mut app, KeyCode::Enter).is_none());
        key(&mut app, KeyCode::Char('x'));
        let Some(Cmd::Untrust { machine_id, .. }) = armed_key(&mut app, KeyCode::Char('y')) else {
            panic!("expected an untrust command");
        };
        assert_eq!(machine_id, "laptop");
    }

    #[test]
    fn signature_failed_items_ask_again_and_stay_out_of_approve_all() {
        use ratatui::{backend::TestBackend, Terminal};

        let mut app = with_inbox();
        app.state
            .inbox
            .items
            .insert(0, inbox_item("forged", vec![Reason::SignatureFailed]));
        app.active_tab = Tab::Security;
        let mut terminal = Terminal::new(TestBackend::new(160, 48)).unwrap();
        terminal
            .draw(|f| crate::dashboard::view::view(f, &app))
            .unwrap();
        assert!(screen(&terminal).contains("signature failed"));
        key(&mut app, KeyCode::Char('A'));
        assert_eq!(
            approve_all_names(&app),
            Some((vec!["left-pad".to_string()], 2))
        );
        app.overlays.clear();
        draw(&app);
        assert!(key(&mut app, KeyCode::Char('a')).is_none());
        assert!(matches!(
            app.overlays.last(),
            Some(Overlay::Confirm(Confirm::ApproveSignatureFailed { .. }))
        ));
        let Some(Cmd::ApprovePackages { items, .. }) = armed_key(&mut app, KeyCode::Char('y'))
        else {
            panic!("expected an approval");
        };
        assert_eq!(items[0].name, "forged");
    }

    #[test]
    fn palette_shows_inbox_items_but_never_decides() {
        let mut app = with_inbox();
        let targets: Vec<Target> = palette::entries(&app)
            .into_iter()
            .map(|e| e.target)
            .collect();
        assert!(targets.contains(&Target::Inbox("npm:left-pad".into())));
        assert!(targets.contains(&Target::Inbox("npm:evil".into())));
        assert!(targets.contains(&Target::Action(Action::ApproveAll)));
        assert!(run_target(&mut app, Target::Inbox("npm:left-pad".into())).is_none());
        assert_eq!(app.active_tab, Tab::Security);
        assert_eq!(app.security.cursor, 1);
        assert!(app.security.detail);
        assert!(app.overlays.is_empty());
    }

    #[test]
    fn rollback_confirm_runs_the_gated_cli_rollback() {
        let mut app = app();
        let plan = crate::dashboard::repo::RollbackPlan {
            manager: "npm".into(),
            commit: "abc123".into(),
            short_hash: "abc".into(),
            to_install: vec![("evil".into(), Some("1.0.0".into()))],
            uninstall: 0,
        };
        app.overlays.push(Overlay::Confirm(Confirm::Rollback {
            plan,
            arming: Default::default(),
        }));
        let cmd = armed_key(&mut app, KeyCode::Char('y'));
        let Some(Cmd::Run(job)) = cmd else {
            panic!("expected the rollback job");
        };
        assert_eq!(
            job.args(),
            vec!["rollback", "packages", "npm", "abc123", "--yes"]
        );
    }

    #[test]
    fn old_record_is_removed_only_after_y() {
        let mut app = app();
        app.state.sync_state = Some(
            serde_json::from_value(serde_json::json!({
                "machine_id": "me",
                "last_sync": "2026-01-01T00:00:00Z",
                "files": {},
                "packages": {},
            }))
            .unwrap(),
        );
        app.state.machines = vec![
            crate::sync::MachineState::new("me"),
            crate::sync::MachineState::new("mac.local"),
            crate::sync::MachineState::new("studio"),
        ];
        app.state.old_ids = vec![crate::sync::signing::OldId {
            machine_id: "mac.local".into(),
            digest: "d1".into(),
        }];
        app.active_tab = Tab::Machines;

        // This machine's record and another machine's record never get the question
        for cursor in [0, 2] {
            app.machines.cursor = cursor;
            assert!(key(&mut app, KeyCode::Char('D')).is_none());
            assert!(app.overlays.is_empty());
        }
        run_target(&mut app, Target::RemoveOldRecord("me".into()));
        assert!(app.overlays.is_empty());

        app.machines.cursor = 1;
        key(&mut app, KeyCode::Char('D'));
        assert!(matches!(
            app.overlays.last(),
            Some(Overlay::Confirm(Confirm::RemoveMachine { .. }))
        ));
        draw(&app);
        // Enter cancels, so a stray Enter never deletes a record
        assert!(armed_key(&mut app, KeyCode::Enter).is_none());
        assert!(app.overlays.is_empty());
        key(&mut app, KeyCode::Char('D'));
        // The command carries the record as shown, so the runtime can refuse a changed one
        let Some(Cmd::RemoveMachine { machine_id, digest }) =
            armed_key(&mut app, KeyCode::Char('y'))
        else {
            panic!("expected a remove command");
        };
        assert_eq!((machine_id.as_str(), digest.as_str()), ("mac.local", "d1"));

        let palette = palette::entries(&app);
        assert!(palette
            .iter()
            .any(|e| e.target == Target::RemoveOldRecord("mac.local".into())));
        run_target(&mut app, Target::RemoveOldRecord("mac.local".into()));
        assert!(matches!(
            app.overlays.last(),
            Some(Overlay::Confirm(Confirm::RemoveMachine { .. }))
        ));
    }

    #[test]
    fn every_view_renders_at_tiny_and_large_sizes() {
        use ratatui::{backend::TestBackend, Terminal};

        let mut app = app();
        app.flash_error("a long error message that has to wrap inside the toast box somehow");
        for (w, h) in [(20, 6), (80, 24), (200, 60)] {
            let mut terminal = Terminal::new(TestBackend::new(w, h)).unwrap();
            for tab in Tab::all() {
                app.active_tab = *tab;
                terminal
                    .draw(|f| crate::dashboard::view::view(f, &app))
                    .unwrap();
            }
            run_action(&mut app, Action::Help);
            terminal
                .draw(|f| crate::dashboard::view::view(f, &app))
                .unwrap();
            run_action(&mut app, Action::Help);
            app.overlays
                .push(Overlay::Palette(Palette::new(palette::entries(&app))));
            terminal
                .draw(|f| crate::dashboard::view::view(f, &app))
                .unwrap();
            app.overlays.clear();
        }
    }
}

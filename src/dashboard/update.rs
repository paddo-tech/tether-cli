use super::app::{Action, App, DaemonOp, Hit, InstallOp, Job, Overlay, Tab};
use super::components::palette::{self, Palette, Target};
use super::components::{
    config, confirm, file_import, files, machines, overview, packages, pkg_import, profile_picker,
    security,
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
            if matches!(job, Job::Rollback { .. }) {
                app.flash_error("Could not start tether");
            }
            // A follow-up sync sent before the running job's start was seen; retry when it exits.
            if matches!(job, Job::Sync) && app.running.is_some() {
                app.sync_pending = true;
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
                Ok(()) => app.follow_up_sync(),
                Err(e) => {
                    app.flash_error(format!("uninstall failed: {}", e));
                    None
                }
            }
        }
        Msg::InstallDone { op, result } => on_install_done(app, op, result),
        Msg::InboxDone(result) => {
            security::reload(app);
            match result {
                Ok(msg) => app.flash_success(msg),
                Err(e) => app.flash_error(e),
            }
            None
        }
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

/// Expire toasts; every refresh interval reload state and recount sync activity.
fn on_tick(app: &mut App) -> Option<Cmd> {
    let now = Instant::now();
    app.toasts.retain(|t| t.alive(now));
    if app.last_refresh.elapsed() >= REFRESH_INTERVAL {
        app.reload_state();
        app.hits.borrow_mut().clear();
        return Some(Cmd::LoadActivity);
    }
    None
}

/// Keys go to the top modal overlay, then the active tab, then the global keymap.
fn on_key(app: &mut App, key: KeyEvent) -> Option<Cmd> {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    if ctrl && key.code == KeyCode::Char('c') {
        request_quit(app);
        return None;
    }
    if ctrl && key.code == KeyCode::Char('k') && !app.overlays.last().is_some_and(Overlay::is_modal)
    {
        let entries = palette::entries(app);
        app.overlays.push(Overlay::Palette(Palette::new(entries)));
        return None;
    }

    if app.overlays.last().is_some_and(Overlay::is_modal) {
        return match app.overlays.pop()? {
            Overlay::Confirm(c) => confirm::handle_key(app, c, key),
            Overlay::FileImport(p) => file_import::handle_key(app, p, key),
            Overlay::PkgImport(p) => pkg_import::handle_key(app, p, key),
            Overlay::ProfilePicker(p) => profile_picker::handle_key(app, p, key),
            Overlay::Palette(p) => match palette::handle_key(app, p, key) {
                Some(target) => run_target(app, target),
                None => None,
            },
            Overlay::Help => unreachable!("help is not modal"),
        };
    }

    let outcome = match app.active_tab {
        Tab::Overview => overview::handle_key(app, key),
        Tab::Files => files::handle_key(app, key),
        Tab::Packages => packages::handle_key(app, key),
        Tab::Machines => machines::handle_key(app, key),
        Tab::Config => config::handle_key(app, key),
        Tab::Security => security::handle_key(app, key),
    };
    if let KeyOutcome::Handled(cmd) = outcome {
        return cmd;
    }

    match key.code {
        KeyCode::Char('q') | KeyCode::Esc => {
            if app.help_open() {
                app.overlays.retain(|o| !matches!(o, Overlay::Help));
            } else {
                request_quit(app);
            }
        }
        KeyCode::Char('s') => return run_action(app, Action::Sync),
        KeyCode::Char('d') => return run_action(app, Action::ToggleDaemon),
        KeyCode::Char('r') => return run_action(app, Action::Refresh),
        KeyCode::Tab => {
            let tabs = Tab::all();
            let current = tabs.iter().position(|t| *t == app.active_tab).unwrap_or(0);
            app.active_tab = tabs[(current + 1) % tabs.len()];
        }
        KeyCode::Char(c @ '1'..='9') => {
            if let Some(tab) = Tab::all().get(c as usize - '1' as usize) {
                app.active_tab = *tab;
            }
        }
        KeyCode::Char('?') => return run_action(app, Action::Help),
        _ => {}
    }
    None
}

pub fn run_action(app: &mut App, action: Action) -> Option<Cmd> {
    match action {
        Action::Sync => return app.sync_cmd(),
        Action::ToggleDaemon => {
            if app.daemon_op == DaemonOp::None {
                let op = if app.state.daemon_running {
                    DaemonOp::Stopping
                } else {
                    DaemonOp::Starting
                };
                return Some(Cmd::Daemon(op));
            }
        }
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
        return None;
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

/// Live package lists replace this machine's state and persist, so disk reloads keep them.
fn on_local_packages(app: &mut App, packages: std::collections::HashMap<String, Vec<String>>) {
    let machine_id = app.machine_id().to_string();
    let sync_path = crate::sync::SyncEngine::sync_path().ok();
    if let Some(machine) = app
        .state
        .machines
        .iter_mut()
        .find(|m| m.machine_id == machine_id)
    {
        machine.packages = packages;
        if let Some(ref sync_path) = sync_path {
            let _ = machine.save_to_repo(sync_path);
        }
    } else if !machine_id.is_empty() {
        let mut ms = crate::sync::MachineState::new(&machine_id);
        ms.packages = packages;
        if let Some(ref sync_path) = sync_path {
            let _ = ms.save_to_repo(sync_path);
        }
        app.state.machines.push(ms);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dashboard::components::confirm::Confirm;
    use crate::dashboard::components::toast::{Toast, ToastKind, MAX_TOASTS};
    use crate::dashboard::state::DashboardState;
    use crate::packages::inbox::{InboxItem, Kind, Reason};
    use crate::sync::{ConflictState, TeamManifest};
    use std::collections::HashMap;
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
        };
        App::new(state, HashMap::new())
    }

    fn last_toast(app: &App) -> Option<(ToastKind, &str)> {
        app.toasts.last().map(|t| (t.kind, t.text.as_str()))
    }

    fn key(app: &mut App, code: KeyCode) -> Option<Cmd> {
        update(app, Msg::Key(KeyEvent::from(code)))
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
    fn escape_closes_help_before_quitting() {
        let mut app = app();
        key(&mut app, KeyCode::Char('?'));
        assert!(app.help_open());
        key(&mut app, KeyCode::Esc);
        assert!(!app.help_open());
        assert!(!app.should_quit);
        key(&mut app, KeyCode::Char('q'));
        assert!(app.should_quit);
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
    fn confirm_overlay_takes_keys_until_answered() {
        let mut app = app();
        app.overlays.push(Overlay::Confirm(Confirm::Uninstall {
            manager_key: "npm".into(),
            name: "left-pad".into(),
        }));
        key(&mut app, KeyCode::Char('2'));
        assert_eq!(app.active_tab, Tab::Overview);
        assert_eq!(app.overlays.len(), 1);
        let cmd = key(&mut app, KeyCode::Char('y'));
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
        assert!(update(&mut app, Msg::UninstallDone(Ok(()))).is_none());
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
    fn second_install_waits_for_the_first() {
        let mut app = app();
        let Some(Cmd::Install { op: first, .. }) =
            app.start_install("npm".into(), "left-pad".into())
        else {
            panic!("expected an install command");
        };
        assert!(app.start_install("npm".into(), "zx".into()).is_none());
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
        assert!(text.contains("MALICIOUS"));
        assert!(text.contains("2 pending"));
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
            .find("2 pending")
            .map(|b| first_row[..b].chars().count());
        click(&mut app, x.unwrap() as u16, 0);
        assert_eq!(app.active_tab, Tab::Security);
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
            Some(Overlay::Confirm(Confirm::ApproveAll { items, malicious })) => {
                Some((items.iter().map(|i| i.name.clone()).collect(), *malicious))
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
        let cmd = key(&mut app, KeyCode::Char('y'));
        assert_eq!(approved_names(cmd), vec!["left-pad"]);
    }

    #[test]
    fn reload_keeps_the_selection_on_its_item() {
        let mut app = with_inbox();
        app.active_tab = Tab::Security;
        key(&mut app, KeyCode::Char('j'));
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
        let Some(Cmd::Reject(item)) = key(&mut app, KeyCode::Char('x')) else {
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
        assert!(text.contains("KEY CHANGED"));
        assert!(text.contains("SHA256:abc"));
        assert!(text.contains("trust key"));
        key(&mut app, KeyCode::Char('A'));
        assert_eq!(
            approve_all_names(&app),
            Some((vec!["left-pad".to_string()], 1))
        );
        app.overlays.clear();
        let Some(Cmd::TrustKey { item, label }) = key(&mut app, KeyCode::Char('a')) else {
            panic!("expected a trust command");
        };
        assert_eq!((item.name.as_str(), label.as_str()), ("laptop", "laptop"));
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
        app.overlays.push(Overlay::Confirm(Confirm::Rollback(plan)));
        let cmd = key(&mut app, KeyCode::Char('y'));
        let Some(Cmd::Run(job)) = cmd else {
            panic!("expected the rollback job");
        };
        assert_eq!(job.args(), vec!["rollback", "packages", "npm", "abc123"]);
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

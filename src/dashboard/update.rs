use super::app::{App, DaemonOp, Job, Overlay, Tab};
use super::components::{
    config, confirm, file_import, files, machines, overview, packages, pkg_import, profile_picker,
};
use super::msg::{Cmd, KeyOutcome, Msg};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use std::time::Duration;

const FLASH_TTL: Duration = Duration::from_secs(3);
const REFRESH_INTERVAL: Duration = Duration::from_secs(30);

pub fn update(app: &mut App, msg: Msg) -> Option<Cmd> {
    match msg {
        Msg::Key(key) => on_key(app, key),
        Msg::Tick => {
            on_tick(app);
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
        Msg::InstallDone {
            manager_key,
            name,
            result,
        } => on_install_done(app, manager_key, name, result),
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

fn on_tick(app: &mut App) {
    if app
        .flash_error
        .as_ref()
        .is_some_and(|(t, _)| t.elapsed() >= FLASH_TTL)
    {
        app.flash_error = None;
    }
    if app
        .flash_message
        .as_ref()
        .is_some_and(|(t, _)| t.elapsed() >= FLASH_TTL)
    {
        app.flash_message = None;
    }
    if app.last_refresh.elapsed() >= REFRESH_INTERVAL {
        app.reload_state();
    }
}

/// Keys go to the top modal overlay, then the active tab, then the global keymap.
fn on_key(app: &mut App, key: KeyEvent) -> Option<Cmd> {
    if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
        request_quit(app);
        return None;
    }

    if app.overlays.last().is_some_and(Overlay::is_modal) {
        return match app.overlays.pop()? {
            Overlay::Confirm(c) => confirm::handle_key(app, c, key),
            Overlay::FileImport(p) => file_import::handle_key(app, p, key),
            Overlay::PkgImport(p) => pkg_import::handle_key(app, p, key),
            Overlay::ProfilePicker(p) => profile_picker::handle_key(app, p, key),
            Overlay::Help => unreachable!("help is not modal"),
        };
    }

    let outcome = match app.active_tab {
        Tab::Overview => overview::handle_key(app, key),
        Tab::Files => files::handle_key(app, key),
        Tab::Packages => packages::handle_key(app, key),
        Tab::Machines => machines::handle_key(app, key),
        Tab::Config => config::handle_key(app, key),
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
        KeyCode::Char('s') => return app.sync_cmd(),
        KeyCode::Char('d') => {
            if app.daemon_op == DaemonOp::None {
                let op = if app.state.daemon_running {
                    DaemonOp::Stopping
                } else {
                    DaemonOp::Starting
                };
                return Some(Cmd::Daemon(op));
            }
        }
        KeyCode::Char('r') => app.reload_state(),
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
        KeyCode::Char('?') => {
            if app.help_open() {
                app.overlays.retain(|o| !matches!(o, Overlay::Help));
            } else {
                app.overlays.push(Overlay::Help);
            }
        }
        _ => {}
    }
    None
}

/// A killed rollback leaves packages half-removed with no tombstones.
fn request_quit(app: &mut App) {
    if app.rollback_running() {
        app.flash_error("Rollback in progress, wait for it to finish");
    } else {
        app.should_quit = true;
    }
}

fn on_install_done(
    app: &mut App,
    manager_key: String,
    name: String,
    result: Result<(), String>,
) -> Option<Cmd> {
    // A second install replaces the first; its result is no longer awaited.
    if app.installing.as_ref() != Some(&(manager_key.clone(), name.clone())) {
        return None;
    }
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
    use crate::dashboard::state::DashboardState;
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
        };
        App::new(state, HashMap::new())
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
            app.flash_message.as_ref().map(|(_, m)| m.as_str()),
            Some("Rolling back npm to abc")
        );
        update(
            &mut app,
            Msg::Key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL)),
        );
        assert!(!app.should_quit);
        assert!(app.flash_error.is_some());
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
            app.flash_error.as_ref().map(|(_, m)| m.as_str()),
            Some("uninstall failed: boom")
        );
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
    fn tick_expires_flash_messages() {
        let mut app = app();
        let old = Instant::now() - Duration::from_secs(4);
        app.flash_error = Some((old, "old".into()));
        app.flash_success("fresh");
        update(&mut app, Msg::Tick);
        assert!(app.flash_error.is_none());
        assert!(app.flash_message.is_some());
    }
}

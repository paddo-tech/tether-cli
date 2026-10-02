//! Executes `Cmd`s off the UI thread and reports results as `Msg`s.

use super::app::{DaemonOp, InstallOp, Job};
use super::msg::{Cmd, Msg};
use crate::packages::inbox::{InboxItem, Kind, OsvUnchecked};
use std::collections::HashMap;
use std::future::Future;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::Arc;

/// Days of history in the Overview activity chart.
const ACTIVITY_DAYS: usize = 90;

pub struct Runtime {
    tx: Sender<Msg>,
    pub rx: Receiver<Msg>,
    job: Option<(Job, Child)>,
    daemon: Option<Child>,
    /// A refresh skips the activity count while the last one still runs.
    activity_running: Arc<AtomicBool>,
}

impl Runtime {
    pub fn new() -> Self {
        let (tx, rx) = channel();
        Self {
            tx,
            rx,
            job: None,
            daemon: None,
            activity_running: Arc::new(AtomicBool::new(false)),
        }
    }

    pub fn execute(&mut self, cmd: Cmd) {
        match cmd {
            Cmd::Run(job) => self.run_job(job),
            Cmd::Daemon(op) => self.run_daemon(op),
            Cmd::Uninstall { manager_key, name } => {
                self.spawn(
                    async move { Msg::UninstallDone(run_uninstall(&manager_key, &name).await) },
                    |e| Some(Msg::UninstallDone(Err(e))),
                );
            }
            Cmd::Install {
                op,
                machine_id,
                osv_required,
            } => {
                let failed_op = op.clone();
                self.spawn(
                    async move {
                        // No sync may run between the install and the tombstone removal, or
                        // save this machine's record while this saves it
                        let _sync_lock = match crate::sync::acquire_sync_lock(false) {
                            Ok(lock) => lock,
                            Err(e) => {
                                return Msg::InstallDone {
                                    op,
                                    result: Err(e.to_string()),
                                }
                            }
                        };
                        let result = match install_check(&op, osv_required).await {
                            Err(Blocked::OsvUnreachable(error)) => {
                                return Msg::OsvUnreachable { op, error };
                            }
                            Err(Blocked::Refused(e)) => Err(e),
                            Ok(version) => run_install(&op.manager_key, &op.name, version).await,
                        };
                        if result.is_ok() {
                            // Sync would uninstall it again while it is still tombstoned.
                            remove_from_removed_packages(&machine_id, &op.manager_key, &op.name);
                        }
                        Msg::InstallDone { op, result }
                    },
                    move |e| {
                        Some(Msg::InstallDone {
                            op: failed_op,
                            result: Err(e),
                        })
                    },
                );
            }
            Cmd::ApprovePackages {
                op,
                items,
                osv_required,
            } => {
                let failed_op = op.clone();
                self.spawn(
                    async move {
                        match approve_and_install(&items, osv_required).await {
                            (result, unchecked) if unchecked.is_empty() => {
                                Msg::InstallDone { op, result }
                            }
                            (result, unchecked) => Msg::ApproveOsvUnreachable {
                                op,
                                result,
                                unchecked,
                            },
                        }
                    },
                    move |e| {
                        Some(Msg::InstallDone {
                            op: failed_op,
                            result: Err(e),
                        })
                    },
                );
            }
            Cmd::TrustKey { item, label } => {
                self.spawn(
                    async move {
                        Msg::InboxDone(match crate::packages::inbox::approve(&item) {
                            Ok(item) => match item.kind {
                                Kind::TrustMachine { fingerprint, .. } => {
                                    Ok(format!("Trusted {} ({})", label, fingerprint))
                                }
                                Kind::Package => Ok(format!("Approved {}", item.name)),
                            },
                            Err(e) => Err(e.to_string()),
                        })
                    },
                    |e| Some(Msg::InboxDone(Err(e))),
                );
            }
            Cmd::Reject(item) => {
                self.spawn(
                    async move {
                        Msg::InboxDone(
                            crate::packages::inbox::reject(&item)
                                .map(|item| format!("Rejected {}", item.name))
                                .map_err(|e| e.to_string()),
                        )
                    },
                    |e| Some(Msg::InboxDone(Err(e))),
                );
            }
            Cmd::RemoveMachine { machine_id, digest } => {
                let failed_id = machine_id.clone();
                self.spawn(
                    async move {
                        let result = crate::sync::acquire_sync_lock(false)
                            .and_then(|_lock| {
                                crate::cli::commands::machines::remove_old_record(
                                    &machine_id,
                                    &digest,
                                )
                            })
                            .map_err(|e| e.to_string());
                        Msg::MachineRemoved { machine_id, result }
                    },
                    move |e| {
                        Some(Msg::MachineRemoved {
                            machine_id: failed_id,
                            result: Err(e),
                        })
                    },
                );
            }
            Cmd::LoadActivity => {
                if self.activity_running.swap(true, Ordering::SeqCst) {
                    return;
                }
                let running = self.activity_running.clone();
                let failed = self.activity_running.clone();
                self.spawn(
                    async move {
                        let counts = super::repo::commit_activity(ACTIVITY_DAYS).await;
                        running.store(false, Ordering::SeqCst);
                        Msg::Activity(counts)
                    },
                    move |_| {
                        failed.store(false, Ordering::SeqCst);
                        None
                    },
                );
            }
            Cmd::CollectPackages { config, machine_id } => {
                // An empty list would wipe this machine's packages, so a failure sends nothing.
                self.spawn(
                    async move {
                        Msg::LocalPackages(collect_local_packages(&config, &machine_id).await)
                    },
                    |_| None,
                );
            }
            Cmd::Restore {
                repo_path,
                dotfile,
                commit,
                short_hash,
            } => {
                // Runs inline: a detached thread dies mid-write when the dashboard quits.
                let result = run_restore(&repo_path, &dotfile, &commit);
                let _ = self.tx.send(Msg::RestoreDone {
                    dotfile,
                    short_hash,
                    result,
                });
            }
        }
    }

    /// Report finished child processes.
    pub fn poll(&mut self) {
        if let Some((_, ref mut child)) = self.job {
            if let Ok(Some(status)) = child.try_wait() {
                let (job, _) = self.job.take().expect("job checked above");
                let _ = self.tx.send(Msg::JobExited {
                    job,
                    success: status.success(),
                });
            }
        }
        if let Some(ref mut child) = self.daemon {
            if let Ok(Some(_)) = child.try_wait() {
                self.daemon = None;
                let _ = self.tx.send(Msg::DaemonOpExited);
            }
        }
    }

    pub fn shutdown(&mut self) {
        if let Some((_, ref mut child)) = self.job {
            let _ = child.kill();
            let _ = child.wait();
        }
        // Don't kill the daemon child: let daemon start/stop complete.
        if let Some(ref mut child) = self.daemon {
            let _ = child.wait();
        }
    }

    fn run_job(&mut self, job: Job) {
        if self.job.is_some() {
            let _ = self.tx.send(Msg::JobSpawnFailed(job));
            return;
        }
        let spawned = Command::new(tether_exe())
            .args(job.args())
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn();
        let msg = match spawned {
            Ok(child) => {
                self.job = Some((job.clone(), child));
                Msg::JobStarted(job)
            }
            Err(_) => Msg::JobSpawnFailed(job),
        };
        let _ = self.tx.send(msg);
    }

    fn run_daemon(&mut self, op: DaemonOp) {
        if self.daemon.is_some() {
            return;
        }
        let arg = if op == DaemonOp::Stopping {
            "stop"
        } else {
            "start"
        };
        if let Ok(child) = Command::new(tether_exe())
            .args(["daemon", arg])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
        {
            self.daemon = Some(child);
            let _ = self.tx.send(Msg::DaemonOpStarted(op));
        }
    }

    /// Run a future on its own thread and current-thread runtime, then send its `Msg`.
    /// If the runtime cannot start or the future panics, send `on_fail`'s message instead,
    /// so no operation stays pending.
    fn spawn<F, E>(&self, fut: F, on_fail: E)
    where
        F: Future<Output = Msg> + Send + 'static,
        E: FnOnce(String) -> Option<Msg> + Send + 'static,
    {
        let tx = self.tx.clone();
        std::thread::spawn(move || {
            let msg = match tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            {
                Ok(rt) => match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    rt.block_on(fut)
                })) {
                    Ok(msg) => Some(msg),
                    Err(_) => on_fail("background task panicked".to_string()),
                },
                Err(e) => on_fail(format!("could not start async runtime: {}", e)),
            };
            if let Some(msg) = msg {
                let _ = tx.send(msg);
            }
        });
    }
}

fn tether_exe() -> std::path::PathBuf {
    std::env::current_exe().unwrap_or_else(|_| "tether".into())
}

async fn run_uninstall(manager_key: &str, package: &str) -> Result<(), String> {
    use crate::packages::*;

    let manager: Box<dyn PackageManager> = match manager_key {
        "brew_formulae" | "brew_casks" => Box::new(BrewManager),
        _ => manager_for_key(manager_key)
            .ok_or_else(|| format!("Unknown manager: {}", manager_key))?,
    };

    manager.uninstall(package).await.map_err(|e| e.to_string())
}

/// Why a dashboard install did not run.
enum Blocked {
    Refused(String),
    OsvUnreachable(String),
}

/// The checks of `inbox::check_osv`, and the version to install. Unlike a sync, a dashboard
/// install is not checked against trusted records, so when `osv_required` a release OSV
/// could not check blocks it until the user agrees.
async fn install_check(op: &InstallOp, osv_required: bool) -> Result<Option<String>, Blocked> {
    crate::packages::inbox::check_osv(&op.manager_key, &op.name, None, osv_required)
        .await
        .map_err(|e| match e.downcast::<OsvUnchecked>() {
            Ok(unchecked) => Blocked::OsvUnreachable(unchecked.error),
            Err(e) => Blocked::Refused(e.to_string()),
        })
}

/// Approve each item as displayed and install it, and report every failure together.
/// An item that changed since it was displayed is not approved. With `osv_required`, an
/// item that OSV cannot check is neither approved nor installed: it is returned, with its
/// error, for the user to decide.
async fn approve_and_install(
    items: &[InboxItem],
    osv_required: bool,
) -> (Result<(), String>, Vec<(InboxItem, String)>) {
    // The daemon must not install the same packages meanwhile; the dashboard cannot wait on it
    let _sync_lock = match crate::sync::acquire_sync_lock(false) {
        Ok(lock) => lock,
        Err(e) => return (Err(e.to_string()), Vec::new()),
    };
    let mut failed = Vec::new();
    let mut unchecked = Vec::new();
    for item in items {
        let check = crate::packages::inbox::check_osv(
            &item.manager,
            &item.name,
            item.version.as_deref(),
            osv_required,
        )
        .await;
        let result = match check {
            Err(e) => match e.downcast::<OsvUnchecked>() {
                Ok(e) => {
                    unchecked.push((item.clone(), e.error));
                    continue;
                }
                Err(e) => Err(e),
            },
            Ok(version) => match crate::packages::inbox::approve(item) {
                Ok(item) => {
                    let item = InboxItem { version, ..item };
                    crate::packages::inbox::install(&item, false).await
                }
                Err(e) => Err(e),
            },
        };
        if let Err(e) = result {
            failed.push(format!("{}: {}", item.name, e));
        }
    }
    let result = if failed.is_empty() {
        Ok(())
    } else {
        Err(failed.join("; "))
    };
    (result, unchecked)
}

async fn run_install(
    manager_key: &str,
    package: &str,
    version: Option<String>,
) -> Result<(), String> {
    use crate::packages::*;

    if manager_key == "brew_casks" {
        return BrewManager
            .install_cask(package, false)
            .await
            .map(|_| ())
            .map_err(|e| e.to_string());
    }

    let manager: Box<dyn PackageManager> = match manager_key {
        "brew_formulae" => Box::new(BrewManager),
        _ => manager_for_key(manager_key)
            .ok_or_else(|| format!("Unknown manager: {}", manager_key))?,
    };

    manager
        .install(&PackageInfo {
            name: package.to_string(),
            version,
        })
        .await
        .map_err(|e| e.to_string())
}

async fn collect_local_packages(
    config: &crate::config::Config,
    machine_id: &str,
) -> HashMap<String, Vec<String>> {
    use crate::packages::*;

    let mut packages = HashMap::new();

    if config.is_manager_enabled(machine_id, "brew") {
        let brew = BrewManager::new();
        if brew.is_available().await {
            if let Ok(formulae) = brew.list_installed().await {
                packages.insert(
                    "brew_formulae".to_string(),
                    formulae.iter().map(|p| p.name.clone()).collect(),
                );
            }
            if let Ok(casks) = brew.list_installed_casks().await {
                packages.insert("brew_casks".to_string(), casks);
            }
            if let Ok(taps) = brew.list_taps().await {
                packages.insert("brew_taps".to_string(), taps);
            }
        }
    }

    let managers: Vec<(&str, Box<dyn PackageManager>)> = vec![
        ("npm", Box::new(NpmManager::new())),
        ("pnpm", Box::new(PnpmManager::new())),
        ("bun", Box::new(BunManager::new())),
        ("gem", Box::new(GemManager::new())),
        ("uv", Box::new(UvManager::new())),
    ];

    for (key, manager) in managers {
        if config.is_manager_enabled(machine_id, key) && manager.is_available().await {
            if let Ok(pkgs) = manager.list_installed().await {
                packages.insert(
                    manager.name().to_string(),
                    pkgs.iter().map(|p| p.name.clone()).collect(),
                );
            }
        }
    }

    packages
}

fn remove_from_removed_packages(machine_id: &str, manager_key: &str, pkg_name: &str) {
    if machine_id.is_empty() {
        return;
    }
    if let Ok(sync_path) = crate::sync::SyncEngine::sync_path() {
        if let Ok(Some(mut machine)) = crate::sync::signing::own_record(&sync_path, machine_id) {
            if let Some(removed) = machine.removed_packages.get_mut(manager_key) {
                removed.retain(|p| p != pkg_name);
                if removed.is_empty() {
                    machine.removed_packages.remove(manager_key);
                }
                let _ = crate::sync::signing::save_record(&sync_path, &machine);
            }
        }
    }
}

fn run_restore(repo_path: &str, dotfile_path: &str, commit_hash: &str) -> Result<(), String> {
    let config = crate::config::Config::load().map_err(|e| e.to_string())?;
    let sync_path = crate::sync::SyncEngine::sync_path().map_err(|e| e.to_string())?;
    let git = crate::sync::GitBackend::open(&sync_path).map_err(|e| e.to_string())?;
    let home = crate::home_dir().map_err(|e| e.to_string())?;

    let content = git
        .show_at_commit(commit_hash, repo_path)
        .map_err(|e| e.to_string())?;

    let plaintext = if config.security.encrypt_dotfiles {
        let key = crate::security::get_encryption_key().map_err(|e| e.to_string())?;
        crate::security::decrypt(&content, &key).map_err(|e| e.to_string())?
    } else {
        content
    };

    let dest = home.join(dotfile_path);
    if dest.exists() {
        let backup_dir = crate::sync::create_backup_dir().map_err(|e| e.to_string())?;
        crate::sync::backup_file(&backup_dir, "dotfiles", dotfile_path, &dest)
            .map_err(|e| e.to_string())?;
    }

    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    std::fs::write(&dest, &plaintext).map_err(|e| e.to_string())?;

    // Don't update state hash here. Leaving state unchanged makes the next sync
    // see "local changed, remote unchanged" → push restored content to repo.
    // If we updated state to match restored content, sync would see "local unchanged,
    // remote changed" and overwrite local with the latest repo version.

    Ok(())
}

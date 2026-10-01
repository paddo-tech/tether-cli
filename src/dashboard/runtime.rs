//! Executes `Cmd`s off the UI thread and reports results as `Msg`s.

use super::app::{DaemonOp, Job};
use super::msg::{Cmd, Msg};
use std::collections::HashMap;
use std::future::Future;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{channel, Receiver, Sender};

pub struct Runtime {
    tx: Sender<Msg>,
    pub rx: Receiver<Msg>,
    job: Option<(Job, Child)>,
    daemon: Option<Child>,
}

impl Runtime {
    pub fn new() -> Self {
        let (tx, rx) = channel();
        Self {
            tx,
            rx,
            job: None,
            daemon: None,
        }
    }

    pub fn execute(&mut self, cmd: Cmd) {
        match cmd {
            Cmd::Run(job) => self.run_job(job),
            Cmd::Daemon(op) => self.run_daemon(op),
            Cmd::Uninstall { manager_key, name } => {
                self.spawn(
                    async move { Msg::UninstallDone(run_uninstall(&manager_key, &name).await) },
                );
            }
            Cmd::Install {
                manager_key,
                name,
                machine_id,
            } => {
                self.spawn(async move {
                    let result = run_install(&manager_key, &name).await;
                    if result.is_ok() {
                        // Sync would uninstall it again while it is still tombstoned.
                        remove_from_removed_packages(&machine_id, &manager_key, &name);
                    }
                    Msg::InstallDone {
                        manager_key,
                        name,
                        result,
                    }
                });
            }
            Cmd::CollectPackages { config, machine_id } => {
                self.spawn(async move {
                    Msg::LocalPackages(collect_local_packages(&config, &machine_id).await)
                });
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
    fn spawn<F>(&self, fut: F)
    where
        F: Future<Output = Msg> + Send + 'static,
    {
        let tx = self.tx.clone();
        std::thread::spawn(move || {
            if let Ok(rt) = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            {
                let _ = tx.send(rt.block_on(fut));
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

async fn run_install(manager_key: &str, package: &str) -> Result<(), String> {
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
            version: None,
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
        let machines_dir = sync_path.join("machines");
        let path = machines_dir.join(format!("{}.json", machine_id));
        if let Ok(content) = std::fs::read_to_string(&path) {
            if let Ok(mut machine) = serde_json::from_str::<crate::sync::MachineState>(&content) {
                if let Some(removed) = machine.removed_packages.get_mut(manager_key) {
                    removed.retain(|p| p != pkg_name);
                    if removed.is_empty() {
                        machine.removed_packages.remove(manager_key);
                    }
                    let _ = machine.save_to_repo(&sync_path);
                }
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

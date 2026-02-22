use crate::config::Config;
use crate::packages::{
    BrewManager, BunManager, GemManager, NpmManager, PackageManager, PnpmManager, UvManager,
};
use crate::sync::{
    import_packages, notify_deferred_casks, GitBackend, MachineState, SyncEngine, SyncState,
};
use anyhow::Result;
use chrono::Local;
use sha2::{Digest, Sha256};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, SystemTime};
use tokio::time::Interval;

#[cfg(unix)]
use tokio::signal::unix::{signal, SignalKind};

const DEFAULT_SYNC_INTERVAL_SECS: u64 = 300; // 5 minutes
const MAX_LOG_BYTES: u64 = 5_000_000; // 5 MB

/// Thread-safe flag indicating daemon mode (avoids unsafe std::env::set_var in async)
static DAEMON_MODE: AtomicBool = AtomicBool::new(false);

/// Check if running in daemon mode
pub fn is_daemon_mode() -> bool {
    DAEMON_MODE.load(Ordering::Relaxed)
}

enum TickResult {
    Continue,
    Exit,
}

pub struct DaemonServer {
    sync_interval: Duration,
    last_update_date: Option<chrono::NaiveDate>,
    binary_path: PathBuf,
    binary_mtime: Option<SystemTime>,
}

impl DaemonServer {
    pub fn new() -> Self {
        let binary_path = std::env::current_exe().unwrap_or_default();
        let binary_mtime = std::fs::metadata(&binary_path)
            .and_then(|m| m.modified())
            .ok();

        Self {
            sync_interval: Duration::from_secs(DEFAULT_SYNC_INTERVAL_SECS),
            last_update_date: None,
            binary_path,
            binary_mtime,
        }
    }

    fn sync_interval(&self) -> Interval {
        tokio::time::interval(self.sync_interval)
    }

    /// Check if the binary has been updated since daemon started
    fn binary_updated(&self) -> bool {
        let current_mtime = std::fs::metadata(&self.binary_path)
            .and_then(|m| m.modified())
            .ok();

        match (self.binary_mtime, current_mtime) {
            (Some(start), Some(current)) => current > start,
            _ => false,
        }
    }

    pub async fn run(&mut self) -> Result<()> {
        // Set daemon mode flag (thread-safe alternative to env var)
        DAEMON_MODE.store(true, Ordering::Relaxed);

        log::info!("Daemon starting (pid {})", std::process::id());
        log::info!("Sync interval: {} seconds", self.sync_interval.as_secs());

        #[cfg(unix)]
        {
            let mut sync_timer = self.sync_interval();
            let mut sigterm = signal(SignalKind::terminate())?;
            let mut sighup = signal(SignalKind::hangup())?;
            let ctrl_c = tokio::signal::ctrl_c();
            tokio::pin!(ctrl_c);
            sync_timer.tick().await;

            loop {
                tokio::select! {
                    _ = sync_timer.tick() => {
                        if let TickResult::Exit = self.run_tick().await { break; }
                    },
                    _ = &mut ctrl_c => {
                        log::info!("Received Ctrl+C, stopping daemon");
                        break;
                    },
                    _ = sigterm.recv() => {
                        log::info!("Received SIGTERM, stopping daemon");
                        break;
                    },
                    _ = sighup.recv() => {
                        log::info!("Received SIGHUP, running immediate sync");
                        if let Err(e) = self.run_sync().await {
                            log::error!("Sync failed: {}", e);
                        }
                    },
                };
            }
        }

        #[cfg(not(unix))]
        {
            let mut sync_timer = self.sync_interval();
            let ctrl_c = tokio::signal::ctrl_c();
            tokio::pin!(ctrl_c);
            sync_timer.tick().await;

            loop {
                tokio::select! {
                    _ = sync_timer.tick() => {
                        if let TickResult::Exit = self.run_tick().await { break; }
                    },
                    _ = &mut ctrl_c => {
                        log::info!("Received Ctrl+C, stopping daemon");
                        break;
                    },
                };
            }
        }

        log::info!("Daemon stopped");
        Ok(())
    }

    /// Rotate daemon.log if it exceeds MAX_LOG_BYTES.
    /// Copies to .log.1 and truncates in-place to keep the logger's fd valid.
    fn rotate_log_if_needed(&self) {
        let log_path = match crate::config::Config::config_dir() {
            Ok(d) => d.join("daemon.log"),
            Err(_) => return,
        };
        if let Ok(meta) = std::fs::metadata(&log_path) {
            if meta.len() > MAX_LOG_BYTES {
                let backup = log_path.with_extension("log.1");
                let _ = std::fs::copy(&log_path, &backup);
                let _ = std::fs::File::create(&log_path); // truncate in-place
                log::info!("Rotated daemon.log ({} bytes)", meta.len());
            }
        }
    }

    /// Shared tick logic: sync + conditional package updates + binary update checks
    async fn run_tick(&mut self) -> TickResult {
        self.rotate_log_if_needed();

        if self.binary_updated() {
            log::info!("Binary updated, exiting for restart");
            return TickResult::Exit;
        }

        log::info!("Running periodic sync...");
        if let Err(e) = self.run_sync().await {
            log::error!("Sync failed: {}", e);
        }

        if self.should_run_update() {
            log::info!("Running daily package update...");
            if let Err(e) = self.run_package_updates().await {
                log::error!("Package update failed: {}", e);
            }
            if self.binary_updated() {
                log::info!("Binary updated during package upgrade, exiting for restart");
                return TickResult::Exit;
            }
        }

        TickResult::Continue
    }

    async fn run_sync(&self) -> Result<()> {
        let _sync_lock = match crate::sync::acquire_sync_lock(false) {
            Ok(lock) => lock,
            Err(_) => {
                log::info!("Sync already in progress, skipping this tick");
                return Ok(());
            }
        };

        let mut config = Config::load()?;

        // No personal features: only sync team repos
        if !config.has_personal_features() {
            return self.run_team_only_sync(&config).await;
        }

        let sync_path = SyncEngine::sync_path()?;
        let home = crate::home_dir()?;

        // Pull latest changes
        log::debug!("Pulling latest changes...");
        let git = GitBackend::open(&sync_path)?;
        git.pull()?;

        crate::sync::check_sync_format_version(&sync_path)?;

        // Pull from team repo if enabled
        if let Some(team) = &config.team {
            if team.enabled {
                let team_sync_dir = Config::team_sync_dir()?;
                if team_sync_dir.exists() {
                    let team_git = GitBackend::open(&team_sync_dir)?;
                    team_git.pull()?;
                    log::debug!("Team configs updated");
                }
            }
        }

        let dotfiles_dir = sync_path.join("dotfiles");
        std::fs::create_dir_all(&dotfiles_dir)?;

        // Import remote config before using it
        if config.security.encrypt_dotfiles {
            if let Some(new_config) =
                crate::cli::commands::sync::sync_tether_config(&sync_path, &home)?
            {
                config = new_config;
            }
        }

        // Load state and machine state
        let mut state = SyncState::load()?;

        // Auto-assign machine to "dev" profile on first run after v2 migration
        if !config.profiles.is_empty()
            && !config.machine_profiles.contains_key(&state.machine_id)
        {
            config
                .machine_profiles
                .insert(state.machine_id.clone(), "dev".to_string());
            let _ = config.save();
        }

        let machine_state_for_decrypt =
            MachineState::load_from_repo(&sync_path, &state.machine_id)?.unwrap_or_default();

        // Apply remote changes (dotfiles, config dirs, project configs)
        if config.security.encrypt_dotfiles {
            crate::cli::commands::sync::decrypt_from_repo(
                &config,
                &sync_path,
                &home,
                &mut state,
                &machine_state_for_decrypt,
                false,
            )?;
        }

        // Now sync local changes to remote
        let conflict_state = crate::sync::ConflictState::load().unwrap_or_default();

        // Sync dotfiles to remote (only if feature enabled)
        if config.features.personal_dotfiles {
            let daemon_machine_id = state.machine_id.clone();
            let daemon_profile = config.profile_name(&daemon_machine_id).to_string();

            for entry in config.effective_dotfiles(&daemon_machine_id) {
                // Security: validate path to prevent traversal attacks
                if !entry.is_safe_path() {
                    log::warn!("Skipping unsafe dotfile path: {}", entry.path());
                    continue;
                }

                let pattern = entry.path();
                let shared = config.is_dotfile_shared(&daemon_machine_id, pattern);
                let expanded = crate::sync::expand_dotfile_glob(pattern, &home);

                for file in expanded {
                    // Skip files with conflicts (by expanded name)
                    if conflict_state.conflicts.iter().any(|c| c.file_path == file) {
                        continue;
                    }

                    let source = home.join(&file);
                    if source.exists() {
                        if let Ok(content) = std::fs::read(&source) {
                            let hash = format!("{:x}", Sha256::digest(&content));
                            let file_changed = state
                                .files
                                .get(&file)
                                .map(|f| f.hash != hash)
                                .unwrap_or(true);

                            if file_changed {
                                log::info!("File changed: {}", file);

                                if config.security.encrypt_dotfiles {
                                    let key = crate::security::get_encryption_key()?;
                                    let encrypted = crate::security::encrypt(&content, &key)?;
                                    let repo_path = crate::sync::dotfile_to_repo_path_profiled(
                                        &file,
                                        true,
                                        &daemon_profile,
                                        shared,
                                    );
                                    let dest = sync_path.join(&repo_path);
                                    if let Some(parent) = dest.parent() {
                                        std::fs::create_dir_all(parent)?;
                                    }
                                    std::fs::write(&dest, encrypted)?;
                                } else {
                                    let repo_path = crate::sync::dotfile_to_repo_path_profiled(
                                        &file,
                                        false,
                                        &daemon_profile,
                                        shared,
                                    );
                                    let dest = sync_path.join(&repo_path);
                                    if let Some(parent) = dest.parent() {
                                        std::fs::create_dir_all(parent)?;
                                    }
                                    std::fs::write(&dest, &content)?;
                                }

                                state.update_file(&file, hash.clone());
                            }
                        }
                    }
                }
            }
            // Auto-discover directories sourced from shell configs
            let effective = config.effective_dotfiles(&daemon_machine_id);
            let discovered = crate::sync::discover_sourced_dirs(&home, &effective);
            let mut config_changed = false;
            for dir in discovered {
                let current_profile = config.profile_name(&daemon_machine_id).to_string();
                if let Some(profile) = config.profiles.get_mut(&current_profile) {
                    if !profile.dirs.contains(&dir) {
                        log::info!("Auto-discovered sourced directory: {}", dir);
                        profile.dirs.push(dir);
                        config_changed = true;
                    }
                } else if !config.dotfiles.dirs.contains(&dir) {
                    log::info!("Auto-discovered sourced directory: {}", dir);
                    config.dotfiles.dirs.push(dir);
                    config_changed = true;
                }
            }
            if config_changed {
                config.dotfiles.dirs.sort();
                for profile in config.profiles.values_mut() {
                    profile.dirs.sort();
                }
                config.save()?;
            }

            // Sync global config directories
            if !config.effective_dirs(&daemon_machine_id).is_empty() {
                crate::cli::commands::sync::sync_directories(
                    &config,
                    &daemon_machine_id,
                    &mut state,
                    &sync_path,
                    &home,
                    false,
                )?;
            }

            // Sync project-local configs
            if config.project_configs.enabled {
                crate::cli::commands::sync::sync_project_configs(
                    &config, &mut state, &sync_path, &home, false,
                )?;
            }
        } // end personal_dotfiles feature block

        // Sync team project secrets
        if let Err(e) =
            crate::cli::commands::sync::sync_team_project_secrets(&config, &home, &mut state)
        {
            log::warn!("Failed to sync team project secrets: {}", e);
        }

        // Build machine state (packages, dotfiles, project configs, checkouts)
        let mut machine_state =
            crate::cli::commands::sync::build_machine_state(&config, &state, &sync_path).await?;

        // Import packages (daemon mode: defer casks that need password)
        if config.features.personal_packages {
            let previously_deferred = state.deferred_casks.clone();
            let deferred_casks = import_packages(
                &config,
                &sync_path,
                &mut state,
                &machine_state,
                true, // daemon_mode
                &previously_deferred,
            )
            .await?;

            // Handle newly deferred casks
            if !deferred_casks.is_empty() {
                // Merge with existing deferred casks (dedupe)
                let mut all_deferred: std::collections::HashSet<_> =
                    state.deferred_casks.iter().cloned().collect();
                for cask in &deferred_casks {
                    all_deferred.insert(cask.clone());
                }
                state.deferred_casks = all_deferred.into_iter().collect();
                state.deferred_casks.sort();

                // Only notify if list changed (avoid repeated notifications)
                let hash = format!(
                    "{:x}",
                    Sha256::digest(state.deferred_casks.join(",").as_bytes())
                );
                if state.deferred_casks_hash.as_ref() != Some(&hash) {
                    notify_deferred_casks(&state.deferred_casks).ok();
                    state.deferred_casks_hash = Some(hash);
                    log::info!(
                        "Deferred {} cask{} (require password): {}",
                        state.deferred_casks.len(),
                        if state.deferred_casks.len() == 1 {
                            ""
                        } else {
                            "s"
                        },
                        state.deferred_casks.join(", ")
                    );
                }

                state.save()?;
            }

            // Rebuild machine state after import to capture newly installed packages
            machine_state =
                crate::cli::commands::sync::build_machine_state(&config, &state, &sync_path)
                    .await?;
        }

        // Export package manifests using union of all machine states
        if config.features.personal_packages {
            crate::sync::sync_packages(&config, &mut state, &sync_path, &machine_state, false)
                .await?;
        }

        // Save machine state
        machine_state.save_to_repo(&sync_path)?;

        // Export tether config to sync repo
        if config.security.encrypt_dotfiles {
            crate::cli::commands::sync::export_tether_config(&sync_path, &home, &mut state)?;
        }

        // Commit and push if changes made
        let has_changes = git.has_changes()?;
        if has_changes {
            log::info!("Committing changes...");
            git.commit("Auto-sync from daemon", &state.machine_id)?;
            git.push()?;
            log::info!("Sync complete - changes pushed");
        } else {
            log::debug!("No changes to sync");
        }

        state.mark_synced();

        // Push team repo changes (if write access enabled)
        if let Some(team) = &config.team {
            if team.enabled && !team.read_only {
                let team_sync_dir = Config::team_sync_dir()?;
                if team_sync_dir.exists() {
                    let team_git = GitBackend::open(&team_sync_dir)?;
                    if team_git.has_changes()? {
                        let dotfiles_dir = team_sync_dir.join("dotfiles");
                        if dotfiles_dir.exists() {
                            for entry in std::fs::read_dir(&dotfiles_dir)? {
                                let entry = entry?;
                                if entry.file_type()?.is_file() {
                                    if let Ok(findings) =
                                        crate::security::scan_for_secrets(&entry.path())
                                    {
                                        if !findings.is_empty() {
                                            log::error!(
                                                "Team push blocked: {} contains {} secret(s)",
                                                entry.file_name().to_string_lossy(),
                                                findings.len()
                                            );
                                            anyhow::bail!(
                                                "Cannot push secrets to team repo. Remove sensitive data first."
                                            );
                                        }
                                    }
                                }
                            }
                        }
                        team_git.commit("Update team configs", &state.machine_id)?;
                        team_git.push()?;
                    }
                }
            }
        }

        // Sync collab secrets
        if config.features.collab_secrets {
            if let Err(e) =
                crate::cli::commands::sync::sync_collab_secrets(&config, &home, &mut state)
            {
                log::warn!("Failed to sync collab secrets: {}", e);
            }
        }

        // Prune old backups
        if let Ok(pruned) = crate::sync::prune_old_backups() {
            if pruned > 0 {
                log::debug!("Pruned {} old backup(s)", pruned);
            }
        }

        // Always save state
        state.save()?;

        Ok(())
    }

    /// Team-only sync: only sync team repositories.
    /// Note: caller (run_sync) already holds the sync lock.
    async fn run_team_only_sync(&self, config: &Config) -> Result<()> {
        let teams = match &config.teams {
            Some(t) if !t.active.is_empty() => t,
            _ => {
                log::debug!("Team-only mode with no teams configured, skipping sync");
                return Ok(());
            }
        };

        // Pull from each active team repo
        for team_name in &teams.active {
            let team_config = match teams.teams.get(team_name) {
                Some(c) if c.enabled => c,
                _ => continue,
            };

            let team_repo_dir = Config::team_repo_dir(team_name)?;
            if !team_repo_dir.exists() {
                log::warn!("Team '{}' repo not found", team_name);
                continue;
            }

            let team_git = GitBackend::open(&team_repo_dir)?;
            team_git.pull()?;
            log::debug!("Team '{}' synced", team_name);

            // Push changes if we have write access
            if !team_config.read_only && team_git.has_changes()? {
                let state = SyncState::load()?;
                team_git.commit("Update team configs", &state.machine_id)?;
                team_git.push()?;
            }
        }

        // Sync team project secrets
        let home = crate::home_dir()?;
        let mut state = SyncState::load()?;
        if let Err(e) =
            crate::cli::commands::sync::sync_team_project_secrets(config, &home, &mut state)
        {
            log::warn!("Failed to sync team project secrets: {}", e);
        }
        state.save()?;

        log::info!("Team-only sync complete");
        Ok(())
    }

    /// Check if we should run daily package updates (once per 24h, catches up on missed runs)
    fn should_run_update(&mut self) -> bool {
        // In-memory guard: don't run twice in same session day
        let today = Local::now().date_naive();
        if self.last_update_date == Some(today) {
            return false;
        }

        // Check persisted state for last upgrade time
        let state = match SyncState::load() {
            Ok(s) => s,
            Err(_) => return true,
        };

        let should_run = match state.last_upgrade {
            None => true,
            Some(last) => {
                let elapsed = chrono::Utc::now() - last;
                elapsed.num_hours() >= 24
            }
        };

        if should_run {
            self.last_update_date = Some(today);
        }
        should_run
    }

    /// Update all enabled package managers
    async fn run_package_updates(&self) -> Result<()> {
        let config = Config::load()?;
        let mut any_actual_updates = false;

        let managers: Vec<(Box<dyn PackageManager>, bool)> = vec![
            (Box::new(BrewManager::new()), config.packages.brew.enabled),
            (Box::new(NpmManager::new()), config.packages.npm.enabled),
            (Box::new(PnpmManager::new()), config.packages.pnpm.enabled),
            (Box::new(BunManager::new()), config.packages.bun.enabled),
            (Box::new(GemManager::new()), config.packages.gem.enabled),
            (Box::new(UvManager::new()), config.packages.uv.enabled),
        ];

        for (manager, enabled) in &managers {
            if !enabled || !manager.is_available().await {
                continue;
            }
            log::info!("Updating {} packages...", manager.name());
            let hash_before = manager.compute_manifest_hash().await.ok();
            if let Err(e) = manager.update_all().await {
                log::error!("{} update failed: {}", manager.name(), e);
            } else {
                let hash_after = manager.compute_manifest_hash().await.ok();
                if hash_before != hash_after {
                    any_actual_updates = true;
                }
            }
        }

        // Update state
        let mut state = SyncState::load()?;
        let now = chrono::Utc::now();
        state.last_upgrade = Some(now);
        if any_actual_updates {
            state.last_upgrade_with_updates = Some(now);
            log::info!("Package updates complete (changes detected)");
        } else {
            log::info!("Package updates complete (no changes)");
        }
        state.save()?;

        Ok(())
    }
}

impl Default for DaemonServer {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_daemon_mode_flag_default_false() {
        // Reset to known state (other tests may have set it)
        DAEMON_MODE.store(false, Ordering::Relaxed);
        assert!(!is_daemon_mode());
    }

    #[test]
    fn test_daemon_mode_flag_set_true() {
        DAEMON_MODE.store(true, Ordering::Relaxed);
        assert!(is_daemon_mode());
        // Reset
        DAEMON_MODE.store(false, Ordering::Relaxed);
    }

    #[test]
    fn test_daemon_server_default_interval() {
        let server = DaemonServer::new();
        assert_eq!(server.sync_interval, Duration::from_secs(300));
    }

    #[test]
    fn test_daemon_server_initial_state() {
        let server = DaemonServer::new();
        assert!(server.last_update_date.is_none());
        assert!(!server.binary_path.as_os_str().is_empty());
    }

    #[test]
    fn test_binary_updated_false_when_unchanged() {
        let server = DaemonServer::new();
        // Binary hasn't changed since construction
        assert!(!server.binary_updated());
    }

    #[test]
    fn test_binary_updated_false_when_no_mtime() {
        let server = DaemonServer {
            sync_interval: Duration::from_secs(300),
            last_update_date: None,
            binary_path: PathBuf::from("/nonexistent/binary"),
            binary_mtime: None,
        };
        assert!(!server.binary_updated());
    }

    #[test]
    fn test_binary_updated_detects_newer_mtime() {
        use std::time::SystemTime;

        let server = DaemonServer {
            sync_interval: Duration::from_secs(300),
            last_update_date: None,
            binary_path: std::env::current_exe().unwrap(),
            // Set start mtime to epoch so current binary is always "newer"
            binary_mtime: Some(SystemTime::UNIX_EPOCH),
        };
        assert!(server.binary_updated());
    }
}

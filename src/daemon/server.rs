use crate::config::Config;
use crate::packages::{
    BrewManager, BunManager, GemManager, NpmManager, PackageManager, PnpmManager, UvManager,
};
use crate::sync::{
    detect_conflict, import_packages, notify_conflicts, notify_deferred_casks, ConflictState,
    GitBackend, MachineState, SyncEngine, SyncState,
};
use anyhow::Result;
use chrono::{Local, Utc};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, SystemTime};
use tokio::time::Interval;

#[cfg(unix)]
use std::os::unix::fs::OpenOptionsExt;
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

        let config = Config::load()?;

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

        // Load state and conflict tracking
        let mut state = SyncState::load()?;
        let mut conflict_state = match ConflictState::load() {
            Ok(state) => state,
            Err(e) => {
                log::warn!("Failed to load conflict state: {}", e);
                ConflictState::default()
            }
        };
        let mut new_conflicts = Vec::new();

        // Load machine state for ignored_dotfiles filtering
        let machine_state_for_decrypt =
            MachineState::load_from_repo(&sync_path, &state.machine_id)?.unwrap_or_default();

        // Lazy backup dir for overwrite protection
        let mut backup_dir: Option<PathBuf> = None;

        // Apply remote changes first (with conflict detection)
        // Only sync dotfiles if feature enabled
        if config.features.personal_dotfiles && config.security.encrypt_dotfiles {
            let key = crate::security::get_encryption_key()?;
            for entry in &config.dotfiles.files {
                // Security: validate path to prevent traversal attacks
                if !entry.is_safe_path() {
                    log::warn!("Skipping unsafe dotfile path: {}", entry.path());
                    continue;
                }

                let pattern = entry.path();
                let create_if_missing =
                    entry.create_if_missing() || crate::sync::is_glob_pattern(pattern);

                // Expand glob patterns by scanning sync repo for matching .enc files
                let expanded = crate::sync::expand_from_sync_repo(pattern, &dotfiles_dir);

                for file in expanded {
                    // Skip files ignored on this machine
                    if machine_state_for_decrypt
                        .ignored_dotfiles
                        .iter()
                        .any(|f| f == &file)
                    {
                        continue;
                    }

                    let filename = file.trim_start_matches('.');
                    let enc_file = dotfiles_dir.join(format!("{}.enc", filename));

                    if enc_file.exists() {
                        if let Ok(encrypted_content) = std::fs::read(&enc_file) {
                            if let Ok(plaintext) =
                                crate::security::decrypt(&encrypted_content, &key)
                            {
                                let local_file = home.join(&file);

                                // Skip if file doesn't exist and create_if_missing is false
                                if !local_file.exists() && !create_if_missing {
                                    continue;
                                }

                                let last_synced_hash =
                                    state.files.get(&file).map(|f| f.hash.as_str());

                                if let Some(conflict) = detect_conflict(
                                    &file,
                                    &local_file,
                                    &plaintext,
                                    last_synced_hash,
                                ) {
                                    log::warn!("Conflict detected in {}", file);
                                    new_conflicts.push((
                                        file.to_string(),
                                        conflict.local_hash,
                                        conflict.remote_hash,
                                    ));
                                } else {
                                    // No true conflict - preserve local-only changes
                                    let remote_hash = format!("{:x}", Sha256::digest(&plaintext));
                                    let local_hash = std::fs::read(&local_file)
                                        .ok()
                                        .map(|c| format!("{:x}", Sha256::digest(&c)));
                                    let local_unchanged = local_hash.as_deref() == last_synced_hash;
                                    if local_unchanged && local_hash.as_ref() != Some(&remote_hash)
                                    {
                                        // Backup before overwriting
                                        if local_file.exists() {
                                            if backup_dir.is_none() {
                                                backup_dir =
                                                    Some(crate::sync::create_backup_dir()?);
                                            }
                                            crate::sync::backup_file(
                                                backup_dir.as_ref().unwrap(),
                                                "dotfiles",
                                                &file,
                                                &local_file,
                                            )?;
                                        }
                                        if let Some(parent) = local_file.parent() {
                                            std::fs::create_dir_all(parent)?;
                                        }
                                        write_file_secure(&local_file, &plaintext)?;
                                        log::debug!("Applied remote changes to {}", file);
                                    } else if !local_unchanged {
                                        log::debug!("Preserving local changes to {}", file);
                                    }
                                    conflict_state.remove_conflict(&file);
                                }
                            }
                        }
                    }
                }
            }
        }

        // Save conflicts and notify
        for (file, local_hash, remote_hash) in &new_conflicts {
            conflict_state.add_conflict(file, local_hash, remote_hash);
        }
        conflict_state.save()?;
        if !new_conflicts.is_empty() {
            notify_conflicts(new_conflicts.len()).ok();
            log::info!(
                "{} conflicts detected, user notification sent",
                new_conflicts.len()
            );
        }

        // Now sync local changes to remote
        let mut changes_made = false;

        // Sync dotfiles to remote (only if feature enabled)
        if config.features.personal_dotfiles {
            for entry in &config.dotfiles.files {
                // Security: validate path to prevent traversal attacks
                if !entry.is_safe_path() {
                    log::warn!("Skipping unsafe dotfile path: {}", entry.path());
                    continue;
                }

                let pattern = entry.path();
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
                                let filename = file.trim_start_matches('.');

                                if config.security.encrypt_dotfiles {
                                    let key = crate::security::get_encryption_key()?;
                                    let encrypted = crate::security::encrypt(&content, &key)?;
                                    let dest = dotfiles_dir.join(format!("{}.enc", filename));
                                    if let Some(parent) = dest.parent() {
                                        std::fs::create_dir_all(parent)?;
                                    }
                                    std::fs::write(&dest, encrypted)?;
                                } else {
                                    let dest = dotfiles_dir.join(filename);
                                    if let Some(parent) = dest.parent() {
                                        std::fs::create_dir_all(parent)?;
                                    }
                                    std::fs::write(&dest, &content)?;
                                }

                                state.update_file(&file, hash.clone());
                                changes_made = true;
                            }
                        }
                    }
                }
            }
        } // end personal_dotfiles feature block

        // Import packages (daemon mode: defer casks that need password)
        if config.features.personal_packages {
            let machine_state = MachineState::load_from_repo(&sync_path, &state.machine_id)?
                .unwrap_or_else(|| MachineState::new(&state.machine_id));
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

            // Sync packages (export)
            changes_made |= self.sync_packages(&config, &mut state, &sync_path).await?;
        }

        // Update machine state with current CLI version
        let mut machine_state = MachineState::load_from_repo(&sync_path, &state.machine_id)?
            .unwrap_or_else(|| MachineState::new(&state.machine_id));
        machine_state.cli_version = env!("CARGO_PKG_VERSION").to_string();
        machine_state.last_sync = chrono::Utc::now();
        machine_state.save_to_repo(&sync_path)?;

        // Commit and push if changes made (including machine state updates)
        let has_changes = git.has_changes()?;
        if changes_made || has_changes {
            log::info!("Committing changes...");
            git.commit("Auto-sync from daemon", &state.machine_id)?;
            git.push()?;
            state.mark_synced();
            state.save()?;
            log::info!("Sync complete - changes pushed");
        } else {
            log::debug!("No changes to sync");
        }

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

    async fn sync_packages(
        &self,
        config: &Config,
        state: &mut SyncState,
        sync_path: &Path,
    ) -> Result<bool> {
        let manifests_dir = sync_path.join("manifests");
        std::fs::create_dir_all(&manifests_dir)?;

        let mut changes_made = false;

        // Homebrew
        if config.packages.brew.enabled {
            let brew = BrewManager::new();
            if brew.is_available().await {
                if let Ok(manifest) = brew.export_manifest().await {
                    let hash = format!("{:x}", Sha256::digest(manifest.as_bytes()));
                    if state
                        .packages
                        .get("brew")
                        .map(|p| p.hash != hash)
                        .unwrap_or(true)
                    {
                        std::fs::write(manifests_dir.join("Brewfile"), &manifest)?;
                        let now = Utc::now();
                        let existing = state.packages.get("brew");
                        state.packages.insert(
                            "brew".to_string(),
                            crate::sync::state::PackageState {
                                last_sync: now,
                                last_modified: Some(now),
                                last_upgrade: existing.and_then(|e| e.last_upgrade),
                                hash,
                            },
                        );
                        changes_made = true;
                        log::info!("Brewfile updated");
                    }
                }
            }
        }

        let managers: Vec<(Box<dyn PackageManager>, &str, bool)> = vec![
            (
                Box::new(NpmManager::new()),
                "npm.txt",
                config.packages.npm.enabled,
            ),
            (
                Box::new(PnpmManager::new()),
                "pnpm.txt",
                config.packages.pnpm.enabled,
            ),
            (
                Box::new(BunManager::new()),
                "bun.txt",
                config.packages.bun.enabled,
            ),
            (
                Box::new(GemManager::new()),
                "gems.txt",
                config.packages.gem.enabled,
            ),
            (
                Box::new(UvManager::new()),
                "uv.txt",
                config.packages.uv.enabled,
            ),
        ];

        for (manager, filename, enabled) in &managers {
            if *enabled {
                changes_made |= self
                    .sync_package_manager(
                        manager.as_ref(),
                        manager.name(),
                        filename,
                        state,
                        &manifests_dir,
                    )
                    .await?;
            }
        }

        Ok(changes_made)
    }

    async fn sync_package_manager(
        &self,
        manager: &dyn PackageManager,
        name: &str,
        filename: &str,
        state: &mut SyncState,
        manifests_dir: &Path,
    ) -> Result<bool> {
        if !manager.is_available().await {
            return Ok(false);
        }

        if let Ok(manifest) = manager.export_manifest().await {
            let hash = format!("{:x}", Sha256::digest(manifest.as_bytes()));
            if state
                .packages
                .get(name)
                .map(|p| p.hash != hash)
                .unwrap_or(true)
            {
                std::fs::write(manifests_dir.join(filename), &manifest)?;
                let now = Utc::now();
                let existing = state.packages.get(name);
                state.packages.insert(
                    name.to_string(),
                    crate::sync::state::PackageState {
                        last_sync: now,
                        last_modified: Some(now),
                        last_upgrade: existing.and_then(|e| e.last_upgrade),
                        hash,
                    },
                );
                log::info!("{} manifest updated", name);
                return Ok(true);
            }
        }

        Ok(false)
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

/// Write file with secure permissions (0o600 on Unix)
fn write_file_secure(path: &Path, contents: &[u8]) -> Result<()> {
    #[cfg(unix)]
    {
        use std::fs::OpenOptions;
        let mut file = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(path)?;
        std::io::Write::write_all(&mut file, contents)?;
        Ok(())
    }
    #[cfg(not(unix))]
    {
        std::fs::write(path, contents)?;
        Ok(())
    }
}

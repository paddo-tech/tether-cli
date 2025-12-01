use crate::config::Config;
use crate::packages::{
    BrewManager, BunManager, GemManager, NpmManager, PackageManager, PnpmManager,
};
use crate::sync::{
    detect_conflict, notify_conflicts, ConflictState, GitBackend, SyncEngine, SyncState,
};
use anyhow::Result;
use chrono::{Local, Timelike};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};
use tokio::time::Interval;

#[cfg(unix)]
use tokio::signal::unix::{signal, SignalKind};

const DEFAULT_SYNC_INTERVAL_SECS: u64 = 300; // 5 minutes
const UPDATE_HOUR: u32 = 2; // 2am local time

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
        // Set env var so sync code knows we're in daemon mode (non-interactive)
        std::env::set_var("TETHER_DAEMON", "1");

        log::info!("Daemon starting (pid {})", std::process::id());
        log::info!("Sync interval: {} seconds", self.sync_interval.as_secs());

        #[cfg(unix)]
        {
            let mut sync_timer = self.sync_interval();
            let mut sigterm = signal(SignalKind::terminate())?;
            let mut sighup = signal(SignalKind::hangup())?;

            let ctrl_c = tokio::signal::ctrl_c();
            tokio::pin!(ctrl_c);

            // Skip first tick (immediate)
            sync_timer.tick().await;

            loop {
                tokio::select! {
                    _ = sync_timer.tick() => {
                        // Check for binary update before doing work
                        if self.binary_updated() {
                            log::info!("Binary updated, exiting for restart");
                            break;
                        }

                        log::info!("Running periodic sync...");
                        if let Err(e) = self.run_sync().await {
                            log::error!("Sync failed: {}", e);
                        }
                        // Check if we should run daily package updates
                        if self.should_run_update() {
                            log::info!("Running daily package update...");
                            if let Err(e) = self.run_package_updates().await {
                                log::error!("Package update failed: {}", e);
                            }
                            // Re-check after upgrades (tether itself may have been updated)
                            if self.binary_updated() {
                                log::info!("Binary updated during package upgrade, exiting for restart");
                                break;
                            }
                        }
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

            // Skip first tick (immediate)
            sync_timer.tick().await;

            loop {
                tokio::select! {
                    _ = sync_timer.tick() => {
                        // Check for binary update before doing work
                        if self.binary_updated() {
                            log::info!("Binary updated, exiting for restart");
                            break;
                        }

                        log::info!("Running periodic sync...");
                        if let Err(e) = self.run_sync().await {
                            log::error!("Sync failed: {}", e);
                        }
                        // Check if we should run daily package updates
                        if self.should_run_update() {
                            log::info!("Running daily package update...");
                            if let Err(e) = self.run_package_updates().await {
                                log::error!("Package update failed: {}", e);
                            }
                            // Re-check after upgrades (tether itself may have been updated)
                            if self.binary_updated() {
                                log::info!("Binary updated during package upgrade, exiting for restart");
                                break;
                            }
                        }
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

    async fn run_sync(&self) -> Result<()> {
        let config = Config::load()?;
        let sync_path = SyncEngine::sync_path()?;
        let home =
            home::home_dir().ok_or_else(|| anyhow::anyhow!("Could not find home directory"))?;

        // Pull latest changes
        log::debug!("Pulling latest changes...");
        let git = GitBackend::open(&sync_path)?;
        git.pull()?;

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
        let mut conflict_state = ConflictState::load().unwrap_or_default();
        let mut new_conflicts = Vec::new();

        // Apply remote changes first (with conflict detection)
        if config.security.encrypt_dotfiles {
            let key = crate::security::get_encryption_key()?;
            for entry in &config.dotfiles.files {
                let file = entry.path();
                let filename = file.trim_start_matches('.');
                let enc_file = dotfiles_dir.join(format!("{}.enc", filename));

                if enc_file.exists() {
                    if let Ok(encrypted_content) = std::fs::read(&enc_file) {
                        if let Ok(plaintext) =
                            crate::security::decrypt_file(&encrypted_content, &key)
                        {
                            let local_file = home.join(file);

                            // Skip if file doesn't exist and create_if_missing is false
                            if !local_file.exists() && !entry.create_if_missing() {
                                continue;
                            }

                            let last_synced_hash = state.files.get(file).map(|f| f.hash.as_str());

                            if let Some(conflict) =
                                detect_conflict(file, &local_file, &plaintext, last_synced_hash)
                            {
                                log::warn!("Conflict detected in {}", file);
                                new_conflicts.push((
                                    file.to_string(),
                                    conflict.local_hash,
                                    conflict.remote_hash,
                                ));
                            } else {
                                // No conflict, safe to apply remote (create parent dirs if needed)
                                if let Some(parent) = local_file.parent() {
                                    std::fs::create_dir_all(parent)?;
                                }
                                std::fs::write(&local_file, plaintext)?;
                                log::debug!("Applied remote changes to {}", file);
                            }
                        }
                    }
                }
            }
        }

        // Save conflicts and notify
        if !new_conflicts.is_empty() {
            for (file, local_hash, remote_hash) in &new_conflicts {
                conflict_state.add_conflict(file, local_hash, remote_hash);
            }
            conflict_state.save()?;
            notify_conflicts(new_conflicts.len()).ok();
            log::info!(
                "{} conflicts detected, user notification sent",
                new_conflicts.len()
            );
        }

        // Now sync local changes to remote
        let mut changes_made = false;

        for entry in &config.dotfiles.files {
            let file = entry.path();
            // Skip files with conflicts
            if conflict_state
                .conflicts
                .iter()
                .any(|c| c.file_path == file)
            {
                continue;
            }

            let source = home.join(file);
            if source.exists() {
                if let Ok(content) = std::fs::read(&source) {
                    let hash = format!("{:x}", Sha256::digest(&content));
                    let file_changed = state
                        .files
                        .get(file)
                        .map(|f| f.hash != hash)
                        .unwrap_or(true);

                    if file_changed {
                        log::info!("File changed: {}", file);
                        let filename = file.trim_start_matches('.');

                        if config.security.encrypt_dotfiles {
                            let key = crate::security::get_encryption_key()?;
                            let encrypted = crate::security::encrypt_file(&content, &key)?;
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

                        state.update_file(file, hash.clone());
                        changes_made = true;
                    }
                }
            }
        }

        // Sync packages
        changes_made |= self.sync_packages(&config, &mut state, &sync_path).await?;

        // Commit and push if changes made
        if changes_made {
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
                        use chrono::Utc;
                        state.packages.insert(
                            "brew".to_string(),
                            crate::sync::state::PackageState {
                                last_sync: Utc::now(),
                                hash,
                            },
                        );
                        changes_made = true;
                        log::info!("Brewfile updated");
                    }
                }
            }
        }

        // npm
        if config.packages.npm.enabled {
            changes_made |= self
                .sync_package_manager(&NpmManager::new(), "npm", "npm.txt", state, &manifests_dir)
                .await?;
        }

        // pnpm
        if config.packages.pnpm.enabled {
            changes_made |= self
                .sync_package_manager(
                    &PnpmManager::new(),
                    "pnpm",
                    "pnpm.txt",
                    state,
                    &manifests_dir,
                )
                .await?;
        }

        // bun
        if config.packages.bun.enabled {
            changes_made |= self
                .sync_package_manager(&BunManager::new(), "bun", "bun.txt", state, &manifests_dir)
                .await?;
        }

        // gem
        if config.packages.gem.enabled {
            changes_made |= self
                .sync_package_manager(&GemManager::new(), "gem", "gems.txt", state, &manifests_dir)
                .await?;
        }

        Ok(changes_made)
    }

    async fn sync_package_manager<P: PackageManager>(
        &self,
        manager: &P,
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
                use chrono::Utc;
                state.packages.insert(
                    name.to_string(),
                    crate::sync::state::PackageState {
                        last_sync: Utc::now(),
                        hash,
                    },
                );
                log::info!("{} manifest updated", name);
                return Ok(true);
            }
        }

        Ok(false)
    }

    /// Check if we should run daily package updates (2am local, once per day)
    fn should_run_update(&mut self) -> bool {
        let now = Local::now();
        let today = now.date_naive();
        let current_hour = now.hour();

        // Only run at the update hour (2am)
        if current_hour != UPDATE_HOUR {
            return false;
        }

        // Only run once per day
        if self.last_update_date == Some(today) {
            return false;
        }

        // Mark as run for today
        self.last_update_date = Some(today);
        true
    }

    /// Update all enabled package managers
    async fn run_package_updates(&self) -> Result<()> {
        let config = Config::load()?;
        let mut any_actual_updates = false;

        if config.packages.brew.enabled {
            let brew = BrewManager::new();
            if brew.is_available().await {
                log::info!("Updating Homebrew packages...");
                let hash_before = brew.compute_manifest_hash().await.ok();
                if let Err(e) = brew.update_all().await {
                    log::error!("Homebrew update failed: {}", e);
                } else {
                    let hash_after = brew.compute_manifest_hash().await.ok();
                    if hash_before != hash_after {
                        any_actual_updates = true;
                    }
                }
            }
        }

        if config.packages.npm.enabled {
            let npm = NpmManager::new();
            if npm.is_available().await {
                log::info!("Updating npm packages...");
                let hash_before = npm.compute_manifest_hash().await.ok();
                if let Err(e) = npm.update_all().await {
                    log::error!("npm update failed: {}", e);
                } else {
                    let hash_after = npm.compute_manifest_hash().await.ok();
                    if hash_before != hash_after {
                        any_actual_updates = true;
                    }
                }
            }
        }

        if config.packages.pnpm.enabled {
            let pnpm = PnpmManager::new();
            if pnpm.is_available().await {
                log::info!("Updating pnpm packages...");
                let hash_before = pnpm.compute_manifest_hash().await.ok();
                if let Err(e) = pnpm.update_all().await {
                    log::error!("pnpm update failed: {}", e);
                } else {
                    let hash_after = pnpm.compute_manifest_hash().await.ok();
                    if hash_before != hash_after {
                        any_actual_updates = true;
                    }
                }
            }
        }

        if config.packages.bun.enabled {
            let bun = BunManager::new();
            if bun.is_available().await {
                log::info!("Updating bun packages...");
                let hash_before = bun.compute_manifest_hash().await.ok();
                if let Err(e) = bun.update_all().await {
                    log::error!("bun update failed: {}", e);
                } else {
                    let hash_after = bun.compute_manifest_hash().await.ok();
                    if hash_before != hash_after {
                        any_actual_updates = true;
                    }
                }
            }
        }

        if config.packages.gem.enabled {
            let gem = GemManager::new();
            if gem.is_available().await {
                log::info!("Updating Ruby gems...");
                let hash_before = gem.compute_manifest_hash().await.ok();
                if let Err(e) = gem.update_all().await {
                    log::error!("gem update failed: {}", e);
                } else {
                    let hash_after = gem.compute_manifest_hash().await.ok();
                    if hash_before != hash_after {
                        any_actual_updates = true;
                    }
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

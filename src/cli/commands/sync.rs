use crate::cli::{Output, Progress, Prompt};
use crate::config::Config;
use crate::packages::{
    BrewManager, BunManager, GemManager, NpmManager, PackageManager, PnpmManager, UvManager,
};
use crate::sync::git::{find_git_repos, get_remote_url, normalize_remote_url};
use crate::sync::{
    import_packages, sync_packages, GitBackend, MachineState, SyncEngine, SyncState,
};
use anyhow::Result;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

/// Build a map of normalized project URLs to all local checkout paths
fn build_project_map(search_paths: &[PathBuf]) -> HashMap<String, Vec<PathBuf>> {
    let mut project_map: HashMap<String, Vec<PathBuf>> = HashMap::new();

    for search_path in search_paths {
        if !search_path.exists() {
            continue;
        }
        if let Ok(repos) = find_git_repos(search_path) {
            for repo in repos {
                if let Ok(url) = get_remote_url(&repo) {
                    let normalized = normalize_remote_url(&url);
                    project_map.entry(normalized).or_default().push(repo);
                }
            }
        }
    }

    project_map
}

pub async fn run(dry_run: bool, force: bool, rediscover: bool) -> Result<()> {
    // Acquire sync lock, waiting for any running sync to finish
    let _sync_lock = if !dry_run {
        Some(crate::sync::acquire_sync_lock(true)?)
    } else {
        None
    };
    run_locked(dry_run, force, rediscover).await
}

/// A sync for a caller that already holds the sync lock, or a dry run.
pub async fn run_locked(dry_run: bool, _force: bool, rediscover: bool) -> Result<()> {
    if dry_run {
        Output::info("Dry-run mode");
    }

    // Only an interactive shell has the user's PATH; cron or ssh -c would bake in a bare PATH for good.
    #[cfg(target_os = "macos")]
    if !dry_run
        && !crate::daemon::is_daemon_mode()
        && std::io::IsTerminal::is_terminal(&std::io::stdin())
    {
        if let Err(e) = super::daemon::refresh_stale_launchd_service().await {
            Output::warning(&format!("Could not update the daemon service: {}", e));
        }
    }

    // A dry run reads config.toml without saving a migration. Without one, a sync would take
    // the synced config; a dry run does not pull it, so it shows what the defaults do
    let config = if dry_run {
        match std::fs::read_to_string(Config::config_path()?) {
            Ok(text) => Config::parse(&text)?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Config::default(),
            Err(e) => return Err(e.into()),
        }
    } else {
        Config::load()?
    };

    // No personal features: skip personal sync, only sync teams
    if !config.has_personal_features() {
        return run_team_only_sync(&config, dry_run).await;
    }

    let mut config = config;

    // Ensure encryption key is unlocked if encryption is enabled
    if config.security.encrypt_dotfiles && !crate::security::is_unlocked() {
        if !crate::security::has_encryption_key() {
            return Err(anyhow::anyhow!(
                "No encryption key found. Run 'tether init' first."
            ));
        }

        Output::info("Enter passphrase:");
        let passphrase = Prompt::password("Passphrase")?;
        crate::security::unlock_with_passphrase(&passphrase)?;
    }
    let sync_path = SyncEngine::sync_path()?;
    let home = crate::home_dir()?;

    // Pull latest changes from personal repo
    let git = GitBackend::open(&sync_path)?;
    if !dry_run {
        Output::info("Pulling latest changes...");
        git.pull()?;
        crate::sync::check_sync_format_version(&sync_path)?;
    }

    // Pull from team repo if enabled
    if let Some(team) = &config.team {
        if team.enabled {
            let team_sync_dir = Config::team_sync_dir()?;

            if team_sync_dir.exists() {
                if !dry_run {
                    let team_git = GitBackend::open(&team_sync_dir)?;
                    team_git.pull()?;
                }
            } else {
                Output::warning("Team sync directory not found - run 'tether team add' again");
            }
        }
    }

    // Always sync tether config first (hardcoded, not dependent on config)
    // This ensures config changes from other machines are applied before using config
    let mut state = SyncState::load()?;
    if config.security.encrypt_dotfiles && !dry_run {
        if let Some(new_config) = sync_tether_config(&sync_path, &home, &mut state)? {
            warn_changed_profile(&config, &new_config, &state.machine_id);
            config = new_config;
        }
    }

    if !git.has_unpushed_commits() {
        state.discard_unpushed();
    }

    // Auto-assign machine to default profile on first run after v2 migration
    if !config.profiles.is_empty() && !config.machine_profiles.contains_key(&state.machine_id) {
        config.machine_profiles.insert(
            state.machine_id.clone(),
            crate::config::DEFAULT_PROFILE.to_string(),
        );
        if !dry_run {
            config.save()?;
        }
    }

    // Load machine state early to get ignored lists for decrypt phase
    let machine_state_for_decrypt =
        crate::sync::signing::own_record(&sync_path, &state.machine_id)?.unwrap_or_default();

    // Without a terminal, a sync runs as the daemon does: conflicts wait for 'tether resolve'
    // and casks that need a password wait for a sync in a terminal. -y still answers prompts,
    // but never a conflict: its merge tool or editor needs a terminal.
    let daemon_like = crate::daemon::is_daemon_mode() || !Prompt::is_interactive();
    let interactive =
        !crate::daemon::is_daemon_mode() && (Prompt::is_interactive() || Prompt::assume_yes());
    if config.security.encrypt_dotfiles && !dry_run {
        decrypt_from_repo(
            &config,
            &sync_path,
            &home,
            &mut state,
            &machine_state_for_decrypt,
            !daemon_like,
        )?;
    }

    // Interactive mode: offer files from other profiles
    if interactive && !dry_run && config.features.personal_dotfiles {
        if rediscover {
            state.dismissed_imports.clear();
        }
        let machine_id_for_prompt = state.machine_id.clone();
        if let Ok(true) =
            prompt_new_items(&mut config, &machine_id_for_prompt, &sync_path, &mut state)
        {
            // Config changed, dotfile list expanded — re-decrypt for newly added files
            if config.security.encrypt_dotfiles {
                decrypt_from_repo(
                    &config,
                    &sync_path,
                    &home,
                    &mut state,
                    &machine_state_for_decrypt,
                    !daemon_like,
                )?;
            }
        }
    }

    // A pending conflict keeps the remote file until 'tether resolve'
    let conflict_state = crate::sync::ConflictState::load().unwrap_or_default();

    // Sync dotfiles (local → Git) - only if personal dotfiles enabled
    if config.features.personal_dotfiles {
        let machine_id = state.machine_id.clone();
        let upload_profile = config.profile_name(&machine_id).to_string();

        // Sync individual dotfiles (with glob expansion)
        for entry in config.effective_dotfiles(&machine_id) {
            // Validate path before expansion to prevent traversal attacks
            if !entry.is_safe_path() {
                Output::warning(&format!("Skipping unsafe dotfile path: {}", entry.path()));
                continue;
            }

            let pattern = entry.path();
            let shared = config.is_dotfile_shared(&machine_id, pattern);
            let expanded = crate::sync::expand_dotfile_glob(pattern, &home);

            for file in expanded {
                if conflict_state.conflicts.iter().any(|c| c.file_path == file) {
                    continue;
                }
                if !dry_run {
                    crate::sync::migrate_dotfile_shared_change(
                        &sync_path,
                        &file,
                        config.security.encrypt_dotfiles,
                        &upload_profile,
                        shared,
                    )?;
                }

                let source = home.join(&file);

                if source.exists() {
                    if let Ok(content) = std::fs::read(&source) {
                        let hash = crate::sha256_hex(&content);

                        let file_changed = state
                            .files
                            .get(&file)
                            .map(|f| f.hash != hash)
                            .unwrap_or(true);

                        if file_changed && !dry_run {
                            if config.security.encrypt_dotfiles {
                                let key = crate::security::get_encryption_key()?;
                                let encrypted_data = crate::security::encrypt(&content, &key)?;
                                let repo_path = crate::sync::dotfile_to_repo_path_profiled(
                                    &file,
                                    true,
                                    &upload_profile,
                                    shared,
                                );
                                let dest = sync_path.join(&repo_path);
                                if let Some(parent) = dest.parent() {
                                    std::fs::create_dir_all(parent)?;
                                }
                                std::fs::write(&dest, encrypted_data)?;
                                #[cfg(unix)]
                                preserve_executable_bit(&source, &dest);
                            } else {
                                let repo_path = crate::sync::dotfile_to_repo_path_profiled(
                                    &file,
                                    false,
                                    &upload_profile,
                                    shared,
                                );
                                let dest = sync_path.join(&repo_path);
                                if let Some(parent) = dest.parent() {
                                    std::fs::create_dir_all(parent)?;
                                }
                                std::fs::write(&dest, &content)?;
                                #[cfg(unix)]
                                preserve_executable_bit(&source, &dest);
                            }

                            state.update_file(&file, hash.clone());
                        }
                    }
                }
            }
        }

        // Auto-discover directories sourced from shell configs and add to config
        if !dry_run {
            let effective = config.effective_dotfiles(&machine_id);
            let discovered = crate::sync::discover_sourced_dirs(&home, &effective);
            let mut config_changed = false;
            for dir in discovered {
                // Push to current profile's dirs (or global if no profile)
                let current_profile = config.profile_name(&machine_id).to_string();
                if let Some(profile) = config.profiles.get_mut(&current_profile) {
                    if !profile.dirs.contains(&dir) {
                        Output::info(&format!("Auto-discovered sourced directory: {}", dir));
                        profile.dirs.push(dir);
                        config_changed = true;
                    }
                } else if !config.dotfiles.dirs.contains(&dir) {
                    Output::info(&format!("Auto-discovered sourced directory: {}", dir));
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
        }

        // Sync global config directories
        let effective_dirs = config.effective_dirs(&machine_id);
        if !effective_dirs.is_empty() {
            sync_directories(&config, &machine_id, &mut state, &sync_path, &home, dry_run)?;
        }

        // Sync project-local configs (personal)
        if config.project_configs.enabled {
            sync_project_configs(&config, &mut state, &sync_path, &home, dry_run)?;
        }
    } // end personal dotfiles feature block

    // Sync team project secrets
    if !dry_run {
        sync_team_project_secrets(&config, &home, &mut state)?;
    }

    // Build machine state first (to know what's installed locally + respect removed_packages)
    let mut machine_state = build_machine_state(&config, &state, &sync_path).await?;

    // Import packages from manifests (install missing packages, respecting removed_packages)
    // In a terminal: install deferred casks from daemon syncs
    if config.features.personal_packages && !dry_run {
        let deferred_casks = state.deferred_casks.clone();

        let outcome = import_packages(
            &config,
            &sync_path,
            &mut state,
            &machine_state,
            daemon_like,
            &deferred_casks,
        )
        .await?;
        // A sync in a terminal installs casks now, so only a sync without one defers them
        if daemon_like {
            crate::sync::packages::defer_casks(&mut state, &outcome.deferred_casks)?;
        }

        if crate::cli::Prompt::is_interactive() && !crate::cli::Prompt::assume_yes() {
            // A cancelled prompt defers the review; the sync must still save and push
            if let Err(e) = super::packages::review_inbox().await {
                Output::warning(&format!("Inbox review stopped: {}", e));
            }
        }

        // Clear deferred casks after a sync in a terminal (user had their chance)
        if !daemon_like && !state.deferred_casks.is_empty() {
            state.deferred_casks.clear();
            state.deferred_casks_hash = None;
            state.save()?;
        }

        // Rebuild machine state after import to capture newly installed packages
        machine_state = build_machine_state(&config, &state, &sync_path).await?;
    }

    // Export package manifests using union of all machine states
    if config.features.personal_packages {
        sync_packages(&config, &mut state, &sync_path, &machine_state, dry_run).await?;
    }

    // Save machine state for cross-machine comparison
    if !dry_run {
        crate::sync::signing::save_record(&sync_path, &machine_state)?;
    }

    // Always export tether config (hardcoded, not dependent on feature flags)
    // This ensures config settings (including features) are synced across machines
    // even when personal features are disabled, allowing remote config changes
    if config.security.encrypt_dotfiles && !dry_run {
        export_tether_config(&sync_path, &home, &mut state)?;
    }

    // Commit and push changes
    if !dry_run {
        let has_changes = git.has_changes()?;

        if has_changes {
            git.commit("Sync dotfiles and packages", &crate::sync::local_hostname())?;
        }
        // Retry a commit left by a failed push, or mark_synced would record it as pushed
        if has_changes || git.has_unpushed_commits() {
            let pb = Progress::spinner("Pushing changes...");
            git.push()?;
            pb.finish_and_clear();
        }
        commit_config_base(&home)?;
    }

    // Check and push team repo changes (if write access enabled)
    if !dry_run {
        if let Some(team) = &config.team {
            if team.enabled && !team.read_only {
                let team_sync_dir = Config::team_sync_dir()?;
                if team_sync_dir.exists() {
                    let team_git = GitBackend::open(&team_sync_dir)?;

                    let has_changes = team_git.has_changes()?;
                    // Scan stranded commits too: they reach the team on this push
                    if has_changes || team_git.has_unpushed_commits() {
                        let dotfiles_dir = team_sync_dir.join("dotfiles");
                        if dotfiles_dir.exists() {
                            for entry in std::fs::read_dir(&dotfiles_dir)? {
                                let entry = entry?;
                                if entry.file_type()?.is_file() {
                                    if let Ok(findings) =
                                        crate::security::scan_for_secrets(&entry.path())
                                    {
                                        if !findings.is_empty() {
                                            anyhow::bail!(
                                                "Team push blocked: {} contains {} secret(s). Remove sensitive data first",
                                                entry.file_name().to_string_lossy(),
                                                findings.len()
                                            );
                                        }
                                    }
                                }
                            }
                        }

                        if has_changes {
                            team_git
                                .commit("Update team configs", &crate::sync::local_hostname())?;
                        }
                        team_git.push()?;
                    }
                }
            }
        }
    }

    // Sync collab secrets (only if feature enabled)
    if !dry_run && config.features.collab_secrets {
        sync_collab_secrets(&config, &home, &mut state)?;
    }

    // Prune old backups
    if let Ok(pruned) = crate::sync::prune_old_backups() {
        if pruned > 0 {
            log::debug!("Pruned {} old backup(s)", pruned);
        }
    }

    if dry_run {
        Output::info("Dry run: nothing changed");
        return Ok(());
    }
    state.mark_synced();
    state.save()?;
    Output::success("Synced");
    Ok(())
}

/// Sync secrets from collab repos to local projects
pub fn sync_collab_secrets(config: &Config, home: &Path, state: &mut SyncState) -> Result<()> {
    use crate::sync::{backup_file, create_backup_dir};

    let teams = match &config.teams {
        Some(t) if !t.collabs.is_empty() => t,
        _ => return Ok(()), // No collabs configured
    };

    // Discover local projects
    let project_paths = config.project_configs.search_paths.clone();
    let search_paths: Vec<PathBuf> = if project_paths.is_empty() {
        vec![
            home.join("Projects"),
            home.join("Code"),
            home.join("Developer"),
            home.join("repos"),
        ]
    } else {
        project_paths
            .iter()
            .map(|p: &String| {
                if p.starts_with("~/") {
                    home.join(p.strip_prefix("~/").unwrap())
                } else {
                    PathBuf::from(p)
                }
            })
            .collect()
    };

    // Build map of normalized_url -> list of local checkouts
    let project_map = build_project_map(&search_paths);

    // Load user's identity for decryption
    let identity = match crate::security::load_identity(None) {
        Ok(id) => id,
        Err(_) => return Ok(()), // No identity, can't decrypt
    };

    let mut backup_dir: Option<PathBuf> = None;

    // Process each collab
    for (collab_name, collab_config) in &teams.collabs {
        if !collab_config.enabled {
            continue;
        }

        let collab_dir = match Config::collab_repo_dir(collab_name) {
            Ok(d) if d.exists() => d,
            _ => continue,
        };

        // Pull latest
        if let Ok(git) = GitBackend::open(&collab_dir) {
            if let Err(e) = git.pull() {
                log::warn!("Failed to pull collab '{}': {}", collab_name, e);
            }
        }

        // Walk projects directory
        let projects_dir = collab_dir.join("projects");
        if !projects_dir.exists() {
            continue;
        }

        for entry in walkdir::WalkDir::new(&projects_dir) {
            let entry = match entry {
                Ok(e) => e,
                Err(_) => continue,
            };

            if !entry.file_type().is_file() {
                continue;
            }

            let path = entry.path();
            if !path.to_string_lossy().ends_with(".age") {
                continue;
            }

            // Extract project URL and filename from path
            // Path format: projects/github.com/owner/repo/path/to/file.age
            // The first 3 path components are the project URL (host/owner/repo)
            // The rest is the file path within the project
            let rel_path = match path.strip_prefix(&projects_dir) {
                Ok(p) => p,
                Err(_) => continue,
            };

            let components: Vec<_> = rel_path.components().collect();
            if components.len() < 4 {
                // Need at least: host, owner, repo, file
                continue;
            }

            // First 3 components = project URL (github.com/owner/repo)
            let project_url = format!(
                "{}/{}/{}",
                components[0].as_os_str().to_string_lossy(),
                components[1].as_os_str().to_string_lossy(),
                components[2].as_os_str().to_string_lossy()
            );

            // Rest = file path (may be nested: path/to/file.age)
            let file_path: PathBuf = components[3..].iter().map(|c| c.as_os_str()).collect();
            let file_path_str = file_path.to_string_lossy();
            let filename = file_path_str.trim_end_matches(".age");

            // Check if this project is in our collab's projects list
            if !collab_config.projects.iter().any(|p| p == &project_url) {
                continue;
            }

            // Find local checkouts for this project
            let checkouts = match project_map.get(&project_url) {
                Some(c) if !c.is_empty() => c,
                _ => continue,
            };

            // Decrypt and write
            let encrypted = match std::fs::read(path) {
                Ok(e) => e,
                Err(_) => continue,
            };

            match crate::security::decrypt_with_identity(&encrypted, &identity) {
                Ok(decrypted) => {
                    // Security: reject paths with traversal patterns
                    if filename.contains("..") || filename.starts_with('/') {
                        log::warn!("Path traversal attempt blocked: {}", filename);
                        continue;
                    }

                    let state_key =
                        format!("collab-secret:{}/{}/{}", collab_name, project_url, filename);
                    let last_synced_hash = state.files.get(&state_key).map(|f| f.hash.as_str());
                    let remote_hash = crate::sha256_hex(&decrypted);

                    // Write to all checkouts of this project
                    for local_project in checkouts {
                        let dest = local_project.join(filename);

                        // Validate destination stays within project (defense-in-depth)
                        let canonical_project = match local_project.canonicalize() {
                            Ok(p) => p,
                            Err(_) => continue, // Project doesn't exist, skip
                        };

                        // Create parent directories first so we can canonicalize
                        if let Some(parent) = dest.parent() {
                            if std::fs::create_dir_all(parent).is_err() {
                                continue;
                            }
                        }

                        // For new files, check that parent is within project
                        let check_path = if dest.exists() {
                            dest.canonicalize().ok()
                        } else {
                            dest.parent().and_then(|p| p.canonicalize().ok())
                        };

                        if let Some(canonical_check) = check_path {
                            if !canonical_check.starts_with(&canonical_project) {
                                log::warn!("Path traversal attempt blocked: {}", filename);
                                continue;
                            }
                        }

                        let should_write = if dest.exists() {
                            let existing = std::fs::read(&dest).unwrap_or_default();
                            let local_hash = crate::sha256_hex(&existing);
                            if local_hash == remote_hash {
                                false // Already in sync
                            } else {
                                match last_synced_hash {
                                    Some(h) => {
                                        if local_hash == h {
                                            true
                                        } else {
                                            log::info!(
                                                "Preserving local changes to collab secret: {}/{}",
                                                project_url,
                                                filename
                                            );
                                            false
                                        }
                                    }
                                    None => true,
                                }
                            }
                        } else {
                            true
                        };

                        if should_write {
                            if dest.exists() {
                                if backup_dir.is_none() {
                                    backup_dir = Some(create_backup_dir()?);
                                }
                                let backup_path = format!("{}/{}", project_url, filename);
                                backup_file(
                                    backup_dir.as_ref().unwrap(),
                                    "collab-secrets",
                                    &backup_path,
                                    &dest,
                                )?;
                            }
                            write_decrypted(&dest, &decrypted)?;
                            log::debug!(
                                "Synced collab secret: {} -> {}",
                                filename,
                                local_project.display()
                            );
                        }
                    }

                    state.record_remote_file(&state_key, remote_hash);
                }
                Err(e) => {
                    let err_str = e.to_string().to_lowercase();
                    if err_str.contains("not a recipient") || err_str.contains("no matching keys") {
                        log::debug!("Collab secret {}: not a recipient, skipping", filename);
                    } else {
                        log::warn!("Failed to decrypt collab secret {}: {}", filename, e);
                    }
                }
            }
        }
    }

    Ok(())
}

/// Copy owner executable bit from source to dest.
/// Git tracks this bit, so it travels across machines via the sync repo.
#[cfg(unix)]
fn preserve_executable_bit(source: &Path, dest: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let is_exec = std::fs::metadata(source)
        .map(|m| m.permissions().mode() & 0o100 != 0)
        .unwrap_or(false);
    if is_exec {
        if let Ok(meta) = std::fs::metadata(dest) {
            let mode = meta.permissions().mode() | 0o100;
            let _ = std::fs::set_permissions(dest, std::fs::Permissions::from_mode(mode));
        }
    }
}

/// Write decrypted content with secure permissions (0o600 on Unix)
fn write_decrypted(path: &Path, contents: &[u8]) -> Result<()> {
    crate::security::write_owner_only(path, contents)
}

/// Back up an existing dotfile (if present), ensure parent dir exists,
/// write the decrypted content, and preserve the executable bit from the
/// encrypted source file.
fn backup_and_write_dotfile(
    backup_dir: &mut Option<PathBuf>,
    file: &str,
    local_file: &Path,
    enc_file: &Path,
    plaintext: &[u8],
) -> Result<()> {
    use crate::sync::{backup_file, create_backup_dir};
    if local_file.exists() {
        if backup_dir.is_none() {
            *backup_dir = Some(create_backup_dir()?);
        }
        backup_file(backup_dir.as_ref().unwrap(), "dotfiles", file, local_file)?;
    }
    if let Some(parent) = local_file.parent() {
        std::fs::create_dir_all(parent)?;
    }
    write_decrypted(local_file, plaintext)?;
    #[cfg(unix)]
    preserve_executable_bit(enc_file, local_file);
    Ok(())
}

/// Path of the synced Tether config under `configs/`, without `.enc`.
const TETHER_CONFIG_REL: &str = "tether/config.toml";

pub fn decrypt_from_repo(
    config: &Config,
    sync_path: &Path,
    home: &Path,
    state: &mut SyncState,
    machine_state: &MachineState,
    interactive: bool,
) -> Result<()> {
    use crate::config::OnConflict;
    use crate::sync::{detect_conflict, ConflictResolution, ConflictState};

    let key = crate::security::get_encryption_key()?;
    let dotfiles_dir = sync_path.join("dotfiles");
    let mut conflict_state = ConflictState::load().unwrap_or_default();
    let tether = crate::config::TetherDir::new(home);
    let mut new_conflicts = Vec::new();

    // Create backup directory for this sync (lazily - only if needed)
    let mut backup_dir: Option<PathBuf> = None;

    let machine_id = &state.machine_id.clone();
    let profile_name = config.profile_name(machine_id).to_string();

    // Migrate flat repo to profiled layout on first sync after config v2 migration
    if let Err(e) = crate::sync::migrate_repo_to_profiled(sync_path, config, machine_id) {
        log::warn!("Repo migration failed: {}", e);
    }

    // Clean up legacy flat/old-profiled files once all machines are upgraded
    if let Err(e) = crate::sync::cleanup_legacy_dotfiles(sync_path) {
        log::warn!("Legacy cleanup failed: {}", e);
    }

    for entry in config.effective_dotfiles(machine_id) {
        // Validate path before expansion to prevent traversal attacks
        if !entry.is_safe_path() {
            Output::warning(&format!("Skipping unsafe dotfile path: {}", entry.path()));
            continue;
        }

        let pattern = entry.path();
        // Glob patterns default to create_if_missing = true (sync all matching files from other machines)
        let create_if_missing = entry.create_if_missing() || crate::sync::is_glob_pattern(pattern);
        let on_conflict = entry.on_conflict();

        let shared = config.is_dotfile_shared(machine_id, pattern);

        // Expand glob pattern by scanning sync repo for matching .enc files
        // Check both profiled and flat dirs for backwards compat
        let subdir = if shared { "shared" } else { &profile_name };
        let profiled_dir = sync_path.join("profiles").join(subdir);
        let mut expanded = if profiled_dir.exists() {
            crate::sync::expand_from_sync_repo(pattern, &profiled_dir)
        } else {
            vec![]
        };
        // Also check flat dir for un-migrated files (only if profiles/ doesn't exist yet)
        if expanded.is_empty()
            && dotfiles_dir.exists()
            && crate::sync::is_pre_migration_repo(sync_path)
        {
            expanded = crate::sync::expand_from_sync_repo(pattern, &dotfiles_dir);
        }

        for file in expanded {
            // Skip if this dotfile is ignored on this machine
            if machine_state.ignored_dotfiles.iter().any(|f| f == &file) {
                continue;
            }
            if tether.contains(&home.join(&file)) {
                continue;
            }

            // Resolve repo path: profile dir first, flat fallback
            let repo_path = crate::sync::resolve_dotfile_repo_path(
                sync_path,
                &file,
                true, // encrypted
                &profile_name,
                shared,
            );
            let enc_file = sync_path.join(&repo_path);

            if enc_file.exists() {
                let encrypted_content = std::fs::read(&enc_file)?;
                match crate::security::decrypt(&encrypted_content, &key) {
                    Ok(plaintext) => {
                        let local_file = home.join(&file);

                        // Skip if file doesn't exist and create_if_missing is false
                        if !local_file.exists() && !create_if_missing {
                            continue;
                        }

                        let last_synced_hash = state.files.get(&file).map(|f| f.hash.as_str());

                        // First-time sync for create_if_missing files: remote wins.
                        // Handles apps that create defaults (e.g. Claude Code writes
                        // `{}` to settings.json) before tether has a chance to sync.
                        let first_sync = last_synced_hash.is_none() && create_if_missing;

                        let local_content = std::fs::read(&local_file).ok();
                        let local_hash = local_content.as_ref().map(|c| crate::sha256_hex(c));
                        let remote_hash = crate::sha256_hex(&plaintext);

                        if !first_sync {
                            if let (Some(lc), Some(lh)) =
                                (local_content.as_ref(), local_hash.as_ref())
                            {
                                if let Some(conflict) = detect_conflict(
                                    &file,
                                    lc,
                                    lh,
                                    &plaintext,
                                    &remote_hash,
                                    last_synced_hash,
                                ) {
                                    let resolution = match on_conflict {
                                        OnConflict::Local => ConflictResolution::KeepLocal,
                                        OnConflict::Remote => ConflictResolution::UseRemote,
                                        OnConflict::Prompt if interactive => {
                                            conflict.show_diff()?;
                                            conflict.prompt_resolution()?
                                        }
                                        OnConflict::Prompt => {
                                            Output::warning(&format!(
                                                "  {} (conflict - skipped)",
                                                file
                                            ));
                                            ConflictResolution::Skip
                                        }
                                    };

                                    match resolution {
                                        ConflictResolution::Skip => {
                                            new_conflicts.push((
                                                file.to_string(),
                                                conflict.local_hash.clone(),
                                                conflict.remote_hash.clone(),
                                            ));
                                            continue;
                                        }
                                        ConflictResolution::UseRemote => {
                                            backup_and_write_dotfile(
                                                &mut backup_dir,
                                                &file,
                                                &local_file,
                                                &enc_file,
                                                &plaintext,
                                            )?;
                                        }
                                        ConflictResolution::Merged => {
                                            conflict
                                                .launch_merge_tool(&config.merge_tool()?, home)?;
                                        }
                                        ConflictResolution::KeepLocal => {}
                                    }
                                    // Baseline on the remote so the next export pushes the
                                    // local (kept or merged) file instead of re-detecting.
                                    state.record_remote_file(&file, remote_hash);
                                    conflict_state.remove_conflict(&file);
                                    // A glob and an exact entry can both match this file.
                                    new_conflicts.retain(|(f, _, _)| f != &file);
                                    continue;
                                }
                            }
                        }

                        let should_write = if first_sync {
                            local_hash.as_ref() != Some(&remote_hash)
                        } else {
                            let local_unchanged = local_hash.as_deref() == last_synced_hash;
                            local_unchanged && local_hash.as_ref() != Some(&remote_hash)
                        };

                        if should_write {
                            backup_and_write_dotfile(
                                &mut backup_dir,
                                &file,
                                &local_file,
                                &enc_file,
                                &plaintext,
                            )?;
                        }
                        conflict_state.remove_conflict(&file);
                    }
                    Err(e) => {
                        Output::warning(&format!("  {} (failed to decrypt: {})", file, e));
                    }
                }
            }
        }
    }

    // Notify only for files not already pending, so an unresolved conflict
    // does not toast on every daemon tick.
    let newly_conflicted: Vec<&str> = new_conflicts
        .iter()
        .map(|(file, _, _)| file.as_str())
        .filter(|file| {
            !conflict_state
                .conflicts
                .iter()
                .any(|c| c.file_path == *file)
        })
        .collect();
    if !interactive && !newly_conflicted.is_empty() {
        crate::sync::notify_conflicts(&newly_conflicted).ok();
    }

    for (file, local_hash, remote_hash) in &new_conflicts {
        conflict_state.add_conflict(file, local_hash, remote_hash);
    }
    conflict_state.save()?;

    // Decrypt global config directories
    let configs_dir = sync_path.join("configs");
    if configs_dir.exists() {
        use walkdir::WalkDir;
        for entry in WalkDir::new(&configs_dir).follow_links(false) {
            let entry = match entry {
                Ok(e) => e,
                Err(_) => continue,
            };

            if entry.file_type().is_file() {
                let file_path = entry.path();
                let file_name = file_path.to_string_lossy();

                if file_name.ends_with(".enc") {
                    let rel_path = file_path
                        .strip_prefix(&configs_dir)
                        .map_err(|e| anyhow::anyhow!("Failed to strip prefix: {}", e))?;
                    let rel_path_str = rel_path.to_string_lossy();
                    let rel_path_no_enc = rel_path_str.trim_end_matches(".enc");

                    // Tether's own synced config merges into ~/.tether/config.toml, never into ~/tether
                    if rel_path_no_enc == TETHER_CONFIG_REL {
                        continue;
                    }

                    // Validate path is safe (defense-in-depth)
                    if !crate::config::is_safe_dotfile_path(rel_path_no_enc)
                        || tether.contains(&home.join(rel_path_no_enc))
                    {
                        Output::warning(&format!("  {} (unsafe path, skipping)", rel_path_no_enc));
                        continue;
                    }

                    if let Ok(encrypted_content) = std::fs::read(file_path) {
                        match crate::security::decrypt(&encrypted_content, &key) {
                            Ok(plaintext) => {
                                let local_file = home.join(rel_path_no_enc);
                                if let Some(parent) = local_file.parent() {
                                    std::fs::create_dir_all(parent)?;
                                }
                                // Only write if local unchanged since last sync AND remote differs
                                let state_key = format!("~/{}", rel_path_no_enc);
                                let last_synced_hash =
                                    state.files.get(&state_key).map(|f| f.hash.as_str());
                                let remote_hash = crate::sha256_hex(&plaintext);
                                let local_hash = std::fs::read(&local_file)
                                    .ok()
                                    .map(|c| crate::sha256_hex(&c));
                                let local_unchanged = local_hash.as_deref() == last_synced_hash;
                                if local_unchanged && local_hash.as_ref() != Some(&remote_hash) {
                                    write_decrypted(&local_file, &plaintext)?;
                                    #[cfg(unix)]
                                    preserve_executable_bit(file_path, &local_file);
                                }
                            }
                            Err(e) => {
                                Output::warning(&format!(
                                    "  ~/{} (failed to decrypt: {})",
                                    rel_path_no_enc, e
                                ));
                            }
                        }
                    }
                }
            }
        }
    }

    // Decrypt project-local configs
    if config.project_configs.enabled {
        decrypt_project_configs(config, sync_path, home, machine_state, state, &key)?;
    }

    Ok(())
}

/// During interactive sync, scan other profiles for files not in the current profile.
/// Offers to add selected files to the current profile as profile-specific copies.
/// Returns true if config was modified.
pub fn prompt_new_items(
    config: &mut Config,
    machine_id: &str,
    sync_path: &Path,
    state: &mut crate::sync::state::SyncState,
) -> Result<bool> {
    let encrypted = config.security.encrypt_dotfiles;
    let current_profile = config.profile_name(machine_id).to_string();
    let profiles_dir = sync_path.join("profiles");

    // Gather current profile's dotfile paths
    let current_paths: std::collections::HashSet<String> = config
        .effective_dotfiles(machine_id)
        .iter()
        .map(|e| e.path().to_string())
        .collect();

    // Scan other profile directories for .enc files
    // Only consider directories that are known profile names (not flat-layout subdirs like config/)
    let known_profiles: std::collections::HashSet<&str> =
        config.profiles.keys().map(|s| s.as_str()).collect();

    let mut candidates: Vec<(String, String)> = Vec::new(); // (dotfile_path, source_profile)

    if let Ok(entries) = std::fs::read_dir(&profiles_dir) {
        for entry in entries.flatten() {
            if !entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                continue;
            }
            let dir_name = entry.file_name().to_string_lossy().to_string();
            // Skip current profile and shared (shared is already accessible)
            if dir_name == current_profile || dir_name == "shared" {
                continue;
            }
            // Only scan known profile directories
            if !known_profiles.contains(dir_name.as_str()) {
                continue;
            }

            // Walk this profile dir recursively for .enc files
            for file in walkdir::WalkDir::new(entry.path())
                .follow_links(false)
                .into_iter()
                .filter_map(Result::ok)
                .filter(|e| e.file_type().is_file())
            {
                let fname = file.path().to_string_lossy().to_string();
                if encrypted && fname.ends_with(".enc") {
                    if let Ok(rel) = file.path().strip_prefix(entry.path()) {
                        let rel_str = rel.to_string_lossy();
                        let name = rel_str.trim_end_matches(".enc");
                        let dotfile = format!(".{}", name);
                        if !current_paths.contains(&dotfile) {
                            candidates.push((dotfile, dir_name.clone()));
                        }
                    }
                }
            }
        }
    }

    candidates.sort();
    candidates.dedup_by(|a, b| a.0 == b.0);

    // Filter out previously dismissed files
    candidates.retain(|(path, _)| !state.dismissed_imports.contains(path));

    if candidates.is_empty() {
        return Ok(false);
    }

    let options: Vec<String> = candidates
        .iter()
        .map(|(path, profile)| format!("{} (from {})", path, profile))
        .collect();
    let options_ref: Vec<&str> = options.iter().map(|s| s.as_str()).collect();

    let selected = match Prompt::multi_select(
        "New files from other profiles — add to yours?",
        options_ref,
        &[],
    ) {
        Ok(sel) => sel,
        Err(_) => {
            // Cancelled — dismiss all candidates
            for (path, _) in &candidates {
                state.dismissed_imports.insert(path.clone());
            }
            return Ok(false);
        }
    };

    if selected.is_empty() {
        // Selected nothing — dismiss all candidates
        for (path, _) in &candidates {
            state.dismissed_imports.insert(path.clone());
        }
        return Ok(false);
    }

    // Dismiss non-selected files so we don't re-prompt
    let selected_set: std::collections::HashSet<usize> = selected.iter().copied().collect();
    for (i, (path, _)) in candidates.iter().enumerate() {
        if !selected_set.contains(&i) {
            state.dismissed_imports.insert(path.clone());
        }
    }

    // Add selected files to current profile
    let profile = config.profiles.entry(current_profile.clone()).or_default();

    for idx in selected {
        let (dotfile_path, source_profile) = &candidates[idx];
        profile
            .dotfiles
            .push(crate::config::ProfileDotfileEntry::Simple(
                dotfile_path.clone(),
            ));

        // Copy the file from source profile to current profile
        let src_repo_path = crate::sync::dotfile_to_repo_path_profiled(
            dotfile_path,
            encrypted,
            source_profile,
            false,
        );
        let dst_repo_path = crate::sync::dotfile_to_repo_path_profiled(
            dotfile_path,
            encrypted,
            &current_profile,
            false,
        );
        let src = sync_path.join(&src_repo_path);
        let dst = sync_path.join(&dst_repo_path);
        if src.exists() && !dst.exists() {
            if let Some(parent) = dst.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::copy(&src, &dst)?;
        }
    }

    config.save()?;
    Ok(true)
}

/// Ensure checkout_file is a symlink pointing to canonical_path.
/// Handles: missing, wrong symlink, real file (migrates to symlink).
fn ensure_symlink(checkout_file: &Path, canonical_path: &Path) -> Result<()> {
    use std::os::unix::fs::symlink;

    if let Some(parent) = checkout_file.parent() {
        std::fs::create_dir_all(parent)?;
    }

    let metadata = std::fs::symlink_metadata(checkout_file);

    match metadata {
        Ok(m) if m.file_type().is_symlink() => {
            // Already a symlink - check if correct target
            if let Ok(target) = std::fs::read_link(checkout_file) {
                if target == canonical_path {
                    return Ok(()); // Correct symlink exists
                }
            }
            // Wrong target - remove and recreate
            std::fs::remove_file(checkout_file)?;
        }
        Ok(m) if m.file_type().is_dir() => {
            anyhow::bail!(
                "Cannot create symlink: directory exists at {}",
                checkout_file.display()
            );
        }
        Ok(_) => {
            // Real file exists - migrate content to canonical if newer
            let checkout_content = std::fs::read(checkout_file)?;
            let canonical_content = std::fs::read(canonical_path).ok();

            if canonical_content.as_ref() != Some(&checkout_content) {
                let checkout_mtime = std::fs::metadata(checkout_file)?.modified()?;
                let canonical_mtime = std::fs::metadata(canonical_path)
                    .and_then(|m| m.modified())
                    .unwrap_or(std::time::SystemTime::UNIX_EPOCH);

                if checkout_mtime > canonical_mtime {
                    // Checkout is newer - write to canonical
                    if let Some(parent) = canonical_path.parent() {
                        std::fs::create_dir_all(parent)?;
                    }
                    crate::sync::atomic_write(canonical_path, &checkout_content)?;
                }
            }
            std::fs::remove_file(checkout_file)?;
        }
        Err(_) => {
            // Doesn't exist - will create symlink below
        }
    }

    // Ensure canonical file exists before creating symlink
    if !canonical_path.exists() {
        anyhow::bail!(
            "Cannot create symlink: canonical file does not exist at {}",
            canonical_path.display()
        );
    }

    symlink(canonical_path, checkout_file)?;
    Ok(())
}

fn decrypt_project_configs(
    config: &Config,
    sync_path: &Path,
    home: &Path,
    machine_state: &MachineState,
    state: &mut SyncState,
    key: &[u8],
) -> Result<()> {
    use crate::sync::{backup_file, create_backup_dir};
    use walkdir::WalkDir;

    let projects_dir = sync_path.join("projects");
    if !projects_dir.exists() {
        return Ok(());
    }

    // Lazy backup dir creation
    let mut backup_dir: Option<PathBuf> = None;

    // Build map of project URLs -> all local checkouts
    let search_paths: Vec<PathBuf> = config
        .project_configs
        .search_paths
        .iter()
        .map(|p| {
            if let Some(stripped) = p.strip_prefix("~/") {
                home.join(stripped)
            } else {
                PathBuf::from(p)
            }
        })
        .collect();

    let repo_map = build_project_map(&search_paths);

    // Find all unique project names from encrypted files
    let mut projects_in_sync: HashSet<String> = HashSet::new();

    for entry in WalkDir::new(&projects_dir).follow_links(false) {
        let entry = match entry {
            Ok(e) => e,
            Err(_) => continue,
        };

        if !entry.file_type().is_file() {
            continue;
        }

        let file_path = entry.path();
        if !file_path.to_string_lossy().ends_with(".enc") {
            continue;
        }

        // Extract project name from path: projects/host/user/repo/file.enc
        if let Ok(rel_to_projects) = file_path.strip_prefix(&projects_dir) {
            let components: Vec<_> = rel_to_projects.components().collect();
            if components.len() >= 4 {
                let project_name = format!(
                    "{}/{}/{}",
                    components[0].as_os_str().to_string_lossy(),
                    components[1].as_os_str().to_string_lossy(),
                    components[2].as_os_str().to_string_lossy()
                );
                projects_in_sync.insert(project_name);
            }
        }
    }

    // Process each project
    for project_name in &projects_in_sync {
        // Skip projects that belong to a team (team sync handles those)
        if let Some(teams) = &config.teams {
            if crate::sync::find_team_for_project(project_name, &teams.teams).is_some() {
                continue;
            }
        }

        let project_dir = projects_dir.join(project_name);

        let checkouts = match repo_map.get(project_name) {
            Some(c) if !c.is_empty() => c,
            _ => continue,
        };

        // Process files for this project
        for file_entry in WalkDir::new(&project_dir).follow_links(false) {
            let file_entry = match file_entry {
                Ok(e) => e,
                Err(_) => continue,
            };

            if !file_entry.file_type().is_file() {
                continue;
            }

            let enc_file = file_entry.path();
            let enc_file_name = enc_file.to_string_lossy();

            if enc_file_name.ends_with(".enc") {
                let rel_path = match enc_file.strip_prefix(&project_dir) {
                    Ok(p) => p,
                    Err(_) => continue,
                };
                let rel_path_str = rel_path.to_string_lossy();
                let rel_path_no_enc = rel_path_str.trim_end_matches(".enc");

                // Skip if this project config is ignored on this machine
                if let Some(ignored_paths) = machine_state.ignored_project_configs.get(project_name)
                {
                    if ignored_paths.contains(&rel_path_no_enc.to_string()) {
                        continue;
                    }
                }

                if let Ok(encrypted_content) = std::fs::read(enc_file) {
                    match crate::security::decrypt(&encrypted_content, key) {
                        Ok(plaintext) => {
                            let remote_hash = crate::sha256_hex(&plaintext);
                            let state_key = format!("project:{}/{}", project_name, rel_path_no_enc);
                            let canonical_path = crate::sync::canonical_project_file_path(
                                project_name,
                                rel_path_no_enc,
                            )?;

                            // Check if any checkout has local modifications
                            let last_synced_hash =
                                state.files.get(&state_key).map(|f| f.hash.clone());
                            let mut has_local_mods = false;

                            for local_repo_path in checkouts {
                                let local_file = local_repo_path.join(rel_path_no_enc);
                                // Read actual content (follows symlinks)
                                if let Ok(local_content) = std::fs::read(&local_file) {
                                    let local_hash = crate::sha256_hex(&local_content);
                                    if Some(&local_hash) != last_synced_hash.as_ref()
                                        && local_hash != remote_hash
                                    {
                                        has_local_mods = true;
                                        break;
                                    }
                                }
                            }

                            if has_local_mods {
                                Output::info(&format!(
                                    "{}: {} (local changes will be pushed)",
                                    project_name, rel_path_no_enc
                                ));
                            } else {
                                // Write decrypted content to canonical location
                                let canonical_content = std::fs::read(&canonical_path).ok();
                                let canonical_hash =
                                    canonical_content.as_ref().map(|c| crate::sha256_hex(c));

                                if canonical_hash.as_ref() != Some(&remote_hash) {
                                    // Backup canonical file if it exists and differs
                                    if canonical_path.exists() {
                                        if backup_dir.is_none() {
                                            backup_dir = Some(create_backup_dir()?);
                                        }
                                        let backup_path =
                                            format!("{}/{}", project_name, rel_path_no_enc);
                                        backup_file(
                                            backup_dir.as_ref().unwrap(),
                                            "projects",
                                            &backup_path,
                                            &canonical_path,
                                        )?;
                                    }

                                    crate::sync::atomic_write(&canonical_path, &plaintext)?;
                                    #[cfg(unix)]
                                    {
                                        use std::os::unix::fs::PermissionsExt;
                                        std::fs::set_permissions(
                                            &canonical_path,
                                            std::fs::Permissions::from_mode(0o600),
                                        )?;
                                    }
                                    #[cfg(unix)]
                                    preserve_executable_bit(enc_file, &canonical_path);
                                }
                                state.record_remote_file(&state_key, remote_hash);
                            }

                            // Create symlinks in all checkouts
                            for local_repo_path in checkouts {
                                let checkout_file = local_repo_path.join(rel_path_no_enc);
                                if let Err(e) = ensure_symlink(&checkout_file, &canonical_path) {
                                    log::warn!(
                                        "Failed to create symlink for {}/{}: {}",
                                        project_name,
                                        rel_path_no_enc,
                                        e
                                    );
                                }
                            }
                        }
                        Err(e) => {
                            Output::warning(&format!(
                                "  {}: {} (failed to decrypt: {})",
                                project_name, rel_path_no_enc, e
                            ));
                        }
                    }
                }
            }
        }
    }

    Ok(())
}

/// Merges the synced config.toml into the local one (always, independent of config file
/// list). Returns Some(config) if the local config changed.
pub fn sync_tether_config(
    sync_path: &Path,
    home: &Path,
    state: &mut SyncState,
) -> Result<Option<Config>> {
    Ok(
        if merge_tether_config(sync_path, home, state, crate::security::get_encryption_key)? {
            Some(Config::load()?)
        } else {
            None
        },
    )
}

/// The synced config.toml, at its legacy path in a repo that has not moved it yet.
fn synced_config_path(sync_path: &Path) -> PathBuf {
    let new_path = sync_path.join(format!("configs/{}.enc", TETHER_CONFIG_REL));
    let legacy_path = sync_path.join("dotfiles/tether/config.toml.enc");
    if new_path.exists() {
        new_path
    } else {
        legacy_path
    }
}

/// Merges the synced config.toml into the local one. Returns true when the local one changed.
fn merge_tether_config(
    sync_path: &Path,
    home: &Path,
    state: &mut SyncState,
    key: impl FnOnce() -> Result<Vec<u8>>,
) -> Result<bool> {
    // An export that a previous sync did not push is not in the repo, so it is no base
    match std::fs::remove_file(config_base_pending_path(home)) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => return Err(e.into()),
        _ => {}
    }
    let path = synced_config_path(sync_path);
    if !path.exists() {
        return Ok(false);
    }
    let key = &key()?;
    let encrypted_content = std::fs::read(path)?;

    let remote = match crate::security::decrypt(&encrypted_content, key) {
        Ok(plaintext) => plaintext,
        Err(e) => {
            let hash = crate::sha256_hex(&encrypted_content);
            warn_config_error(state, hash, &format!("does not decrypt: {}", e));
            return Ok(false);
        }
    };
    let text = |b: &[u8]| String::from_utf8_lossy(b).into_owned();
    if let Some(version) = crate::sync::config_merge::newer_version(&text(&remote)) {
        Output::warning(&format!(
            "The synced config.toml has config_version {}, which this Tether cannot read. \
             This machine keeps its config.toml until you upgrade Tether",
            version
        ));
        return Ok(false);
    }
    if let Some(e) = crate::sync::config_merge::read_error(&remote) {
        warn_config_error(
            state,
            crate::sha256_hex(&remote),
            &format!("does not read: {}", e),
        );
        return Ok(false);
    }
    state.config_error = None;
    let local_config_path = home.join(".tether/config.toml");
    let local = match std::fs::read(&local_config_path) {
        Ok(local) => local,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            let text = crate::sync::config_merge::without_legacy_keys(&text(&remote))?;
            crate::sync::atomic_write_private(&local_config_path, text.as_bytes())?;
            write_config_base(home, &remote)?;
            return Ok(true);
        }
        Err(e) => {
            return Err(anyhow::anyhow!(
                "Could not read {}: {}",
                local_config_path.display(),
                e
            ))
        }
    };

    let base = load_config_base(sync_path, home, state, &local, key)?;
    let merged = crate::sync::config_merge::merge(
        base.as_deref().map(text).as_deref(),
        &text(&local),
        &text(&remote),
        &state.machine_id,
    )
    .map_err(|e| anyhow::anyhow!("Could not merge the synced config.toml: {}", e))?;
    for path in &merged.conflicts {
        Output::warning(&format!(
            "config.toml: {} changed on this machine and on another; this machine's value stays",
            path
        ));
    }
    for profile in &merged.kept_profiles {
        Output::warning(&format!(
            "config.toml: another machine deleted profile {}, which a machine still uses; \
             the profile stays",
            profile
        ));
    }
    // The merged config holds every remote change, so this remote is the next merge's base.
    // The base follows the local write: a failed write must not mark remote changes as merged
    if merged.changed {
        crate::sync::atomic_write_private(&local_config_path, merged.text.as_bytes())?;
    }
    write_config_base(home, &remote)?;
    Ok(merged.changed)
}

/// A synced config.toml that does not decrypt or read stops only the config step: the merge
/// and the export skip it, so this machine never replaces a copy it cannot read, such as
/// one from a newer Tether. The warning shows once per copy; later syncs log it.
fn warn_config_error(state: &mut SyncState, hash: String, error: &str) {
    let message = format!(
        "The synced config.toml {}. This machine keeps its config.toml and does not export it \
         until the sync repo holds a copy it can read",
        error
    );
    if state.config_error.as_ref() == Some(&hash) {
        log::warn!("{}", message);
    } else {
        Output::warning(&message);
        state.config_error = Some(hash);
    }
}

/// The base of the next merge. A base file that does not read is replaced by one from the
/// sync history.
fn load_config_base(
    sync_path: &Path,
    home: &Path,
    state: &SyncState,
    local: &[u8],
    key: &[u8],
) -> Result<Option<Vec<u8>>> {
    let path = config_base_path(home);
    match std::fs::read(&path) {
        Ok(base) if crate::sync::config_merge::reads(&base) => return Ok(Some(base)),
        Ok(_) => Output::warning(&format!(
            "{} does not read; the merge takes its base from the sync history",
            path.display()
        )),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(anyhow::anyhow!("Could not read {}: {}", path.display(), e)),
    }
    Ok(recover_config_base(sync_path, state, local, key))
}

/// The base holds the synced config, so only the user may read it.
fn write_config_base(home: &Path, content: &[u8]) -> Result<()> {
    crate::sync::atomic_write_private(&config_base_path(home), content)
}

/// The remote config.toml of the last merge, kept on this machine only.
fn config_base_path(home: &Path) -> PathBuf {
    home.join(".tether/config.base.toml")
}

/// The copy this sync exported, which becomes the base once the push holds it.
fn config_base_pending_path(home: &Path) -> PathBuf {
    home.join(".tether/config.base.pending.toml")
}

/// Makes the exported copy the merge base. Call it after the push that holds the export.
pub fn commit_config_base(home: &Path) -> Result<()> {
    match std::fs::rename(config_base_pending_path(home), config_base_path(home)) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e.into()),
        _ => Ok(()),
    }
}

/// A base for a machine that has none yet, as after an upgrade. The local file when its hash
/// is the one the last export recorded; else the exported copy from the sync repo's history,
/// by the hash of the export (1.x and earlier betas recorded the exported bytes in `files`).
/// No match gives no base, so the merge keeps additions from both sides and reports
/// conflicts. A machine that never synced its config joins the fleet's: its local config is
/// the base, so every remote setting applies, and it keeps only its own profile assignment.
fn recover_config_base(
    sync_path: &Path,
    state: &SyncState,
    local: &[u8],
    key: &[u8],
) -> Option<Vec<u8>> {
    let Some(entry) = state.files.get(".tether/config.toml") else {
        return Some(local.to_vec());
    };
    if crate::sha256_hex(local) == entry.hash {
        return Some(local.to_vec());
    }
    let hashes = [Some(&entry.hash), state.config_export_hash.as_ref()];
    let git = GitBackend::open(sync_path).ok()?;
    for repo_path in [
        "configs/tether/config.toml.enc",
        "dotfiles/tether/config.toml.enc",
    ] {
        for entry in git.file_log(repo_path, 100).unwrap_or_default() {
            let Ok(enc) = git.show_at_commit(&entry.commit_hash, repo_path) else {
                continue;
            };
            if let Ok(plain) = crate::security::decrypt(&enc, key) {
                if hashes.contains(&Some(&crate::sha256_hex(&plain))) {
                    return Some(plain);
                }
            }
        }
    }
    None
}

/// A machine sets only its own profile: the merge keeps this machine's `machine_profiles`
/// entry, whatever another machine or a 1.x copy holds. So the profile changes only when
/// the sync copies the synced config over a missing config.toml, or when the local file
/// changed during the sync. The warning names the packages that change with it.
pub fn warn_changed_profile(old: &Config, new: &Config, machine_id: &str) {
    if let Some(message) = changed_profile_warning(old, new, machine_id) {
        Output::warning(&message);
    }
}

/// A missing assignment counts as the default profile, so only a different profile warns.
fn changed_profile_warning(old: &Config, new: &Config, machine_id: &str) -> Option<String> {
    let before = old.profile_name(machine_id);
    let after = new.profile_name(machine_id);
    if before == after {
        return None;
    }
    let restore = if old.machine_profiles.contains_key(machine_id) {
        format!("tether machines profile set {}", before)
    } else {
        "tether machines profile unset".to_string()
    };
    Some(format!(
        "The synced config changed this machine's profile from {} to {}, so it now installs \
         the packages of profile {}. If nobody meant to change it, run '{}'",
        before, after, after, restore
    ))
}

/// Export tether config to sync repo (always, independent of config file list)
pub fn export_tether_config(sync_path: &Path, home: &Path, state: &mut SyncState) -> Result<()> {
    let config_path = home.join(".tether/config.toml");

    if !config_path.exists() {
        return Ok(());
    }
    let key = crate::security::get_encryption_key()?;
    write_synced_config(sync_path, home, state, &key)
}

fn write_synced_config(
    sync_path: &Path,
    home: &Path,
    state: &mut SyncState,
    key: &[u8],
) -> Result<()> {
    use crate::sync::config_merge::{export_text, has_marker};
    let content = std::fs::read(home.join(".tether/config.toml"))?;
    let dest = sync_path.join(format!("configs/{}.enc", TETHER_CONFIG_REL));
    let repo = match std::fs::read(&dest) {
        Ok(enc) => match crate::security::decrypt(&enc, key) {
            Ok(plain) => Some(plain),
            // The merge warned (warn_config_error)
            Err(_) => return Ok(()),
        },
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => return Err(e.into()),
    };
    if let Some(repo) = &repo {
        // A newer Tether wrote the repo config; this machine leaves it alone until it upgrades
        if crate::sync::config_merge::newer_version(&String::from_utf8_lossy(repo)).is_some()
            || !crate::sync::config_merge::reads(repo)
        {
            return Ok(());
        }
        // A format-only difference is no change: exporting it would make 1.x machines see
        // one. A copy without the marker is replaced, so that 1.x copies get the marker
        if has_marker(repo) && crate::sync::config_merge::same_settings(repo, &content) {
            return Ok(());
        }
    }
    let exported = export_text(&String::from_utf8_lossy(&content))?.into_bytes();
    crate::sync::atomic_write(&dest, &crate::security::encrypt(&exported, key)?)?;
    // Once pushed, the repo holds this config, so it is the next merge's base. With the
    // pulled remote as base, a kept conflict reads as a local edit forever and two machines
    // export their values in turn. Until the push, the base stays: a pull that discards
    // the commit must not leave a base that holds local edits the repo never got
    crate::sync::atomic_write_private(&config_base_pending_path(home), &exported)?;
    state.config_export_hash = Some(crate::sha256_hex(&exported));
    state.update_file(".tether/config.toml", crate::sha256_hex(&content));
    Ok(())
}

pub fn sync_directories(
    config: &Config,
    machine_id: &str,
    state: &mut SyncState,
    sync_path: &Path,
    home: &Path,
    dry_run: bool,
) -> Result<()> {
    use walkdir::WalkDir;

    let configs_dir = sync_path.join("configs");
    std::fs::create_dir_all(&configs_dir)?;
    let tether = crate::config::TetherDir::new(home);

    for dir_path in &config.effective_dirs(machine_id) {
        // Validate path is safe (security: prevents path traversal via synced config)
        if !crate::config::is_safe_dotfile_path(dir_path) {
            Output::warning(&format!("  {} (unsafe path, skipping)", dir_path));
            continue;
        }

        let expanded_path = if let Some(stripped) = dir_path.strip_prefix("~/") {
            home.join(stripped)
        } else {
            PathBuf::from(dir_path)
        };

        if !expanded_path.exists() {
            Output::warning(&format!("  {} (not found, skipping)", dir_path));
            continue;
        }
        if tether.contains(&expanded_path) {
            Output::warning(&format!("  {} (in ~/.tether, skipping)", dir_path));
            continue;
        }

        if expanded_path.is_file() {
            if let Ok(content) = std::fs::read(&expanded_path) {
                let hash = crate::sha256_hex(&content);
                let file_changed = state
                    .files
                    .get(dir_path)
                    .map(|f| f.hash != hash)
                    .unwrap_or(true);

                if file_changed && !dry_run {
                    let rel_path = expanded_path.strip_prefix(home).unwrap_or(&expanded_path);
                    let dest = configs_dir.join(rel_path);

                    if let Some(parent) = dest.parent() {
                        std::fs::create_dir_all(parent)?;
                    }

                    if config.security.encrypt_dotfiles {
                        let key = crate::security::get_encryption_key()?;
                        let encrypted = crate::security::encrypt(&content, &key)?;
                        let enc_dest = PathBuf::from(format!("{}.enc", dest.display()));
                        std::fs::write(&enc_dest, encrypted)?;
                        #[cfg(unix)]
                        preserve_executable_bit(&expanded_path, &enc_dest);
                    } else {
                        std::fs::write(&dest, &content)?;
                        #[cfg(unix)]
                        preserve_executable_bit(&expanded_path, &dest);
                    }

                    state.update_file(dir_path, hash);
                }
            }
        } else if expanded_path.is_dir() {
            // The walk follows no symlink below the root, so a file is in ~/.tether only
            // when its directory is
            for entry in WalkDir::new(&expanded_path)
                .follow_links(false)
                .into_iter()
                .filter_entry(|e| !(e.file_type().is_dir() && tether.contains(e.path())))
            {
                let entry = match entry {
                    Ok(e) => e,
                    Err(_) => continue,
                };

                if entry.file_type().is_file() {
                    let file_path = entry.path();
                    let rel_to_home = file_path.strip_prefix(home).unwrap_or(file_path);
                    let state_key = format!("~/{}", rel_to_home.display());

                    if let Ok(content) = std::fs::read(file_path) {
                        let hash = crate::sha256_hex(&content);
                        let file_changed = state
                            .files
                            .get(&state_key)
                            .map(|f| f.hash != hash)
                            .unwrap_or(true);

                        if file_changed && !dry_run {
                            let dest = configs_dir.join(rel_to_home);

                            if let Some(parent) = dest.parent() {
                                std::fs::create_dir_all(parent)?;
                            }

                            if config.security.encrypt_dotfiles {
                                let key = crate::security::get_encryption_key()?;
                                let encrypted = crate::security::encrypt(&content, &key)?;
                                let enc_dest = PathBuf::from(format!("{}.enc", dest.display()));
                                std::fs::write(&enc_dest, encrypted)?;
                                #[cfg(unix)]
                                preserve_executable_bit(file_path, &enc_dest);
                            } else {
                                std::fs::write(&dest, &content)?;
                                #[cfg(unix)]
                                preserve_executable_bit(file_path, &dest);
                            }

                            state.update_file(&state_key, hash);
                        }
                    }
                }
            }
        }
    }

    Ok(())
}

pub fn sync_project_configs(
    config: &Config,
    state: &mut SyncState,
    sync_path: &Path,
    home: &Path,
    dry_run: bool,
) -> Result<()> {
    use crate::sync::git::{
        find_git_repos, get_remote_url, is_gitignored, normalize_remote_url,
        should_skip_dir_for_project_configs,
    };
    use walkdir::WalkDir;

    let projects_dir = sync_path.join("projects");
    std::fs::create_dir_all(&projects_dir)?;

    for search_path_str in &config.project_configs.search_paths {
        let search_path = if let Some(stripped) = search_path_str.strip_prefix("~/") {
            home.join(stripped)
        } else {
            PathBuf::from(search_path_str)
        };

        if !search_path.exists() {
            continue;
        }

        let repos = match find_git_repos(&search_path) {
            Ok(r) => r,
            Err(_) => continue,
        };

        for repo_path in repos {
            let remote_url = match get_remote_url(&repo_path) {
                Ok(url) => url,
                Err(_) => continue,
            };

            let normalized_url = normalize_remote_url(&remote_url);

            // Skip projects that belong to a team (team sync handles those)
            if let Some(teams) = &config.teams {
                if crate::sync::find_team_for_project(&normalized_url, &teams.teams).is_some() {
                    continue;
                }
            }

            for pattern in &config.project_configs.patterns {
                let walker = WalkDir::new(&repo_path)
                    .follow_links(true)
                    .max_depth(5)
                    .into_iter()
                    .filter_entry(|e| {
                        e.file_type().is_file()
                            || e.file_name()
                                .to_str()
                                .map(|n| !should_skip_dir_for_project_configs(n))
                                .unwrap_or(true)
                    });
                for entry in walker {
                    let entry = match entry {
                        Ok(e) => e,
                        Err(_) => continue,
                    };

                    if !entry.file_type().is_file() {
                        continue;
                    }

                    let file_path = entry.path();
                    let file_name = match file_path.file_name() {
                        Some(name) => name.to_string_lossy(),
                        None => continue,
                    };

                    // Handle ** for directory patterns (e.g., ".idea/**")
                    let matches = if pattern.contains("**") {
                        // For ** patterns, match against full relative path
                        if let Ok(rel_path) = file_path.strip_prefix(&repo_path) {
                            let rel_str = rel_path.to_string_lossy();
                            // Convert ** to match any path
                            let pattern_for_path = pattern.replace("**", "*");
                            crate::sync::glob_match(&pattern_for_path, &rel_str)
                        } else {
                            false
                        }
                    } else {
                        // For single * patterns, match filename only
                        crate::sync::glob_match(pattern, &file_name)
                    };

                    if !matches {
                        continue;
                    }

                    if config.project_configs.only_if_gitignored {
                        match is_gitignored(file_path) {
                            Ok(true) => {}
                            _ => continue,
                        }
                    }

                    if let Ok(content) = std::fs::read(file_path) {
                        let hash = crate::sha256_hex(&content);

                        let rel_to_repo = file_path
                            .strip_prefix(&repo_path)
                            .map_err(|e| anyhow::anyhow!("Failed to strip prefix: {}", e))?;
                        let state_key =
                            format!("project:{}/{}", normalized_url, rel_to_repo.display());

                        let file_changed = state
                            .files
                            .get(&state_key)
                            .map(|f| f.hash != hash)
                            .unwrap_or(true);

                        if file_changed && !dry_run {
                            let dest = projects_dir.join(&normalized_url).join(rel_to_repo);

                            if let Some(parent) = dest.parent() {
                                std::fs::create_dir_all(parent)?;
                            }

                            if config.security.encrypt_dotfiles {
                                let key = crate::security::get_encryption_key()?;
                                let encrypted = crate::security::encrypt(&content, &key)?;
                                let enc_dest = PathBuf::from(format!("{}.enc", dest.display()));
                                std::fs::write(&enc_dest, encrypted)?;
                                #[cfg(unix)]
                                preserve_executable_bit(file_path, &enc_dest);
                            } else {
                                std::fs::write(&dest, &content)?;
                                #[cfg(unix)]
                                preserve_executable_bit(file_path, &dest);
                            }

                            state.update_file(&state_key, hash);
                        }
                    }
                }
            }
        }
    }

    Ok(())
}

/// Build machine state for cross-machine comparison
pub async fn build_machine_state(
    config: &Config,
    state: &SyncState,
    sync_path: &Path,
) -> Result<MachineState> {
    // This machine's last record keeps removals and ignores; the repo copy is not trusted
    let mut machine_state = crate::sync::signing::own_record(sync_path, &state.machine_id)?
        .unwrap_or_else(|| MachineState::new(&state.machine_id));

    // Update last_sync time, hostname, CLI and OS version, and profile
    machine_state.last_sync = chrono::Utc::now();
    machine_state.hostname = crate::sync::local_hostname();
    machine_state.cli_version = env!("CARGO_PKG_VERSION").to_string();
    machine_state.os_version = crate::sync::state::local_os_version();
    machine_state.os = std::env::consts::OS.to_string();
    machine_state.profile = config.machine_profiles.get(&state.machine_id).cloned();

    // Collect file hashes
    machine_state.files.clear();
    for (path, file_state) in &state.files {
        machine_state
            .files
            .insert(path.clone(), file_state.hash.clone());
    }

    // Populate packages from local system
    let previous_packages = machine_state.packages.clone();
    machine_state.packages.clear();

    let mid = &state.machine_id;
    // Homebrew
    if config.is_manager_enabled(mid, "brew") {
        let brew = BrewManager::new();
        if brew.is_available().await {
            // Get formulae
            if let Ok(formulae) = brew.list_installed().await {
                machine_state.packages.insert(
                    "brew_formulae".to_string(),
                    formulae.iter().map(|p| p.name.clone()).collect(),
                );
            }
            // Get casks
            if let Ok(casks) = brew.list_installed_casks().await {
                machine_state
                    .packages
                    .insert("brew_casks".to_string(), casks);
            }
            // Get taps
            if let Ok(taps) = brew.list_taps().await {
                machine_state.packages.insert("brew_taps".to_string(), taps);
            }
        }
    }

    // Standard managers (same pattern: check enabled, check available, list installed)
    let managers: Vec<(bool, Box<dyn PackageManager>)> = vec![
        (
            config.is_manager_enabled(mid, "npm"),
            Box::new(NpmManager::new()),
        ),
        (
            config.is_manager_enabled(mid, "pnpm"),
            Box::new(PnpmManager::new()),
        ),
        (
            config.is_manager_enabled(mid, "bun"),
            Box::new(BunManager::new()),
        ),
        (
            config.is_manager_enabled(mid, "gem"),
            Box::new(GemManager::new()),
        ),
        (
            config.is_manager_enabled(mid, "uv"),
            Box::new(UvManager::new()),
        ),
    ];

    machine_state.package_versions.clear();
    for (enabled, manager) in managers {
        if enabled && manager.is_available().await {
            if let Ok(packages) = manager.list_installed().await {
                machine_state.packages.insert(
                    manager.name().to_string(),
                    packages.iter().map(|p| p.name.clone()).collect(),
                );
                machine_state.package_versions.insert(
                    manager.name().to_string(),
                    packages
                        .into_iter()
                        .filter_map(|p| Some((p.name, p.version?)))
                        .collect(),
                );
            }
        }
    }

    // Detect removed packages: packages that were in previous state but not installed now
    detect_removed_packages(&mut machine_state, &previous_packages);

    // Populate dotfiles list from config (files that exist locally, with glob expansion)
    let home = crate::home_dir()?;
    machine_state.dotfiles.clear();
    for entry in config.effective_dotfiles(&state.machine_id) {
        if !entry.is_safe_path() {
            continue;
        }
        let pattern = entry.path();
        let expanded = crate::sync::expand_dotfile_glob(pattern, &home);
        for file in expanded {
            if home.join(&file).exists() {
                machine_state.dotfiles.push(file);
            }
        }
    }
    machine_state.dotfiles.sort();

    // Populate project_configs from state (tracked project files)
    // State keys are formatted as "project:host/org/repo/rel/path"
    // The project key is the first 3 path components (host/org/repo)
    machine_state.project_configs.clear();
    for key in state.files.keys() {
        if let Some(rest) = key.strip_prefix("project:") {
            let parts: Vec<&str> = rest.splitn(4, '/').collect();
            if parts.len() == 4 {
                let project_key = format!("{}/{}/{}", parts[0], parts[1], parts[2]);
                machine_state
                    .project_configs
                    .entry(project_key)
                    .or_default()
                    .push(parts[3].to_string());
            }
        }
    }
    // Sort for deterministic output
    for paths in machine_state.project_configs.values_mut() {
        paths.sort();
        paths.dedup();
    }

    // Track all checkouts of projects on this machine
    machine_state.checkouts.clear();
    let search_paths: Vec<PathBuf> = config
        .project_configs
        .search_paths
        .iter()
        .map(|p| {
            if let Some(stripped) = p.strip_prefix("~/") {
                home.join(stripped)
            } else {
                PathBuf::from(p)
            }
        })
        .collect();

    let project_map = build_project_map(&search_paths);
    for (normalized_url, checkouts) in project_map {
        use crate::sync::git::checkout_id_from_path;
        use crate::sync::CheckoutInfo;

        let checkout_infos: Vec<CheckoutInfo> = checkouts
            .into_iter()
            .map(|path| {
                let checkout_id = checkout_id_from_path(&path);
                CheckoutInfo { path, checkout_id }
            })
            .collect();

        if !checkout_infos.is_empty() {
            machine_state
                .checkouts
                .insert(normalized_url, checkout_infos);
        }
    }

    Ok(machine_state)
}

/// Detect packages that were removed since the last sync and track them
fn detect_removed_packages(
    machine_state: &mut MachineState,
    previous_packages: &std::collections::HashMap<String, Vec<String>>,
) {
    for (manager, prev_pkgs) in previous_packages {
        let current_pkgs: HashSet<_> = machine_state
            .packages
            .get(manager)
            .map(|v| v.iter().collect())
            .unwrap_or_default();

        let removed_set = machine_state
            .removed_packages
            .entry(manager.clone())
            .or_default();

        for pkg in prev_pkgs {
            if !current_pkgs.contains(pkg) {
                // Package was in previous state but not installed now - track as removed
                if !removed_set.contains(pkg) {
                    removed_set.push(pkg.clone());
                }
            }
        }

        // Clean up: if a package is now installed, remove it from removed_packages
        removed_set.retain(|pkg| !current_pkgs.contains(pkg));
    }
}

/// Sync project secrets from team repos to local projects
pub fn sync_team_project_secrets(
    config: &Config,
    home: &Path,
    state: &mut SyncState,
) -> Result<()> {
    use crate::sync::{backup_file, create_backup_dir};
    use walkdir::WalkDir;

    let teams = match &config.teams {
        Some(t) => t,
        None => return Ok(()),
    };

    // Build map of local projects: normalized_url -> all local checkout paths
    let search_paths: Vec<PathBuf> = config
        .project_configs
        .search_paths
        .iter()
        .map(|p| {
            if let Some(stripped) = p.strip_prefix("~/") {
                home.join(stripped)
            } else {
                PathBuf::from(p)
            }
        })
        .collect();

    let local_projects = build_project_map(&search_paths);

    // Try to load user's identity for decryption
    let identity = match crate::security::load_identity(None) {
        Ok(id) => id,
        Err(_) => {
            // Identity not unlocked - skip team project secrets
            return Ok(());
        }
    };

    // Track secrets we couldn't decrypt (not a recipient)
    let mut skipped_secrets: Vec<String> = vec![];

    // Backup directory (lazy init)
    let mut backup_dir: Option<PathBuf> = None;

    // For each active team with configured orgs
    for team_name in &teams.active {
        let team_config = match teams.teams.get(team_name) {
            Some(c) if c.enabled && !c.orgs.is_empty() => c,
            _ => continue,
        };

        let team_repo_dir = Config::team_repo_dir(team_name)?;
        let projects_dir = team_repo_dir.join("projects");

        if !projects_dir.exists() {
            continue;
        }

        // Walk the team's projects directory
        for entry in WalkDir::new(&projects_dir).follow_links(false).min_depth(4) {
            let entry = match entry {
                Ok(e) => e,
                Err(_) => continue,
            };

            if !entry.file_type().is_file() {
                continue;
            }

            let file_path = entry.path();

            // Only process .age encrypted files
            if !file_path.to_string_lossy().ends_with(".age") {
                continue;
            }

            // Extract project path: projects/github.com/org/repo/file.age
            let rel_to_projects = match file_path.strip_prefix(&projects_dir) {
                Ok(p) => p,
                Err(_) => continue,
            };

            let components: Vec<_> = rel_to_projects.components().collect();
            if components.len() < 4 {
                continue;
            }

            // Reconstruct normalized URL: github.com/org/repo
            let normalized_url = format!(
                "{}/{}/{}",
                components[0].as_os_str().to_string_lossy(),
                components[1].as_os_str().to_string_lossy(),
                components[2].as_os_str().to_string_lossy()
            );

            // Check if this project belongs to this team's orgs
            let project_org = crate::sync::extract_org_from_normalized_url(&normalized_url);
            let belongs_to_team = project_org
                .as_ref()
                .map(|org| team_config.orgs.iter().any(|t| t.eq_ignore_ascii_case(org)))
                .unwrap_or(false);

            if !belongs_to_team {
                continue;
            }

            // Check if we have this project locally
            let checkouts = match local_projects.get(&normalized_url) {
                Some(c) if !c.is_empty() => c,
                _ => continue,
            };

            // Get relative file path (remove .age extension)
            let rel_file_path: PathBuf = components[3..].iter().map(|c| c.as_os_str()).collect();
            let rel_file_str = rel_file_path.to_string_lossy();
            let rel_file_no_age = rel_file_str.trim_end_matches(".age");

            // Decrypt and write to all checkouts
            match std::fs::read(file_path) {
                Ok(encrypted) => {
                    match crate::security::decrypt_with_identity(&encrypted, &identity) {
                        Ok(decrypted) => {
                            let state_key =
                                format!("team-secret:{}/{}", normalized_url, rel_file_no_age);
                            let last_synced_hash =
                                state.files.get(&state_key).map(|f| f.hash.as_str());
                            let remote_hash = crate::sha256_hex(&decrypted);

                            for local_project in checkouts {
                                let local_file = local_project.join(rel_file_no_age);

                                let should_write = if local_file.exists() {
                                    let existing = std::fs::read(&local_file).unwrap_or_default();
                                    let local_hash = crate::sha256_hex(&existing);
                                    if local_hash == remote_hash {
                                        false // Already in sync
                                    } else {
                                        match last_synced_hash {
                                            Some(h) => {
                                                if local_hash == h {
                                                    true
                                                } else {
                                                    log::info!(
                                                        "Preserving local changes to team secret: {}/{}",
                                                        normalized_url,
                                                        rel_file_no_age
                                                    );
                                                    false
                                                }
                                            }
                                            None => true,
                                        }
                                    }
                                } else {
                                    true
                                };

                                if should_write {
                                    // Backup before overwriting
                                    if local_file.exists() {
                                        if backup_dir.is_none() {
                                            backup_dir = Some(create_backup_dir()?);
                                        }
                                        let backup_path =
                                            format!("{}/{}", normalized_url, rel_file_no_age);
                                        backup_file(
                                            backup_dir.as_ref().unwrap(),
                                            "team-projects",
                                            &backup_path,
                                            &local_file,
                                        )?;
                                    }
                                    if let Some(parent) = local_file.parent() {
                                        std::fs::create_dir_all(parent)?;
                                    }
                                    write_decrypted(&local_file, &decrypted)?;
                                    Output::success(&format!(
                                        "Team secret: {} → {}",
                                        rel_file_no_age,
                                        local_project.file_name().unwrap().to_string_lossy()
                                    ));
                                }
                            }

                            state.record_remote_file(&state_key, remote_hash);
                        }
                        Err(e) => {
                            let err_str = e.to_string().to_lowercase();
                            if err_str.contains("not a recipient")
                                || err_str.contains("no matching keys")
                            {
                                skipped_secrets
                                    .push(format!("{}/{}", normalized_url, rel_file_no_age));
                            } else {
                                Output::warning(&format!(
                                    "Failed to decrypt {}/{}: {}",
                                    normalized_url, rel_file_no_age, e
                                ));
                            }
                        }
                    }
                }
                Err(_) => continue,
            }
        }
    }

    if !skipped_secrets.is_empty() {
        Output::warning(&format!(
            "Skipped {} team secret(s) (not a recipient)",
            skipped_secrets.len()
        ));
    }

    Ok(())
}

/// Team-only sync: skip personal dotfiles/packages, only sync team repos
async fn run_team_only_sync(config: &Config, dry_run: bool) -> Result<()> {
    let home = crate::home_dir()?;

    let teams = match &config.teams {
        Some(t) if !t.active.is_empty() => t,
        _ => {
            Output::warning("Team-only mode with no teams configured");
            Output::info("Run 'tether team setup' to add a team");
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
            Output::warning(&format!("Team '{}' repo not found", team_name));
            continue;
        }

        if !dry_run {
            let team_git = GitBackend::open(&team_repo_dir)?;
            team_git.pull()?;

            Output::success(&format!("Team '{}' synced", team_name));

            // Push changes if we have write access
            if !team_config.read_only {
                if team_git.has_changes()? {
                    team_git.commit("Update team configs", &crate::sync::local_hostname())?;
                }
                if team_git.has_unpushed_commits() {
                    team_git.push()?;
                }
            }
        } else {
            Output::success(&format!("Team '{}' synced", team_name));
        }
    }

    if dry_run {
        Output::info("Dry run: nothing changed");
        return Ok(());
    }
    // Sync team project secrets to local projects
    let mut state = SyncState::load()?;
    sync_team_project_secrets(config, &home, &mut state)?;
    state.save()?;
    Output::success("Team sync complete");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sync::config_merge::has_marker;
    use tempfile::TempDir;

    const KEY: [u8; 32] = [7u8; 32];

    fn new_state(id: &str) -> SyncState {
        serde_json::from_value(serde_json::json!({
            "machine_id": id,
            "last_sync": "2026-01-01T00:00:00Z",
            "files": {},
            "packages": {},
        }))
        .unwrap()
    }

    /// A machine with its own home, syncing config.toml through a repo directory
    struct Peer {
        home: TempDir,
        state: SyncState,
    }

    impl Peer {
        fn new(id: &str, config: Option<&str>) -> Peer {
            let home = TempDir::new().unwrap();
            std::fs::create_dir_all(home.path().join(".tether")).unwrap();
            let peer = Peer {
                home,
                state: new_state(id),
            };
            if let Some(config) = config {
                peer.write(config);
            }
            peer
        }
        fn config_path(&self) -> PathBuf {
            self.home.path().join(".tether/config.toml")
        }
        fn read(&self) -> String {
            std::fs::read_to_string(self.config_path()).unwrap()
        }
        fn write(&self, text: &str) {
            std::fs::write(self.config_path(), text).unwrap();
        }
        fn edit(&self, f: impl FnOnce(&mut Config)) {
            let text = self.read();
            let mut c = Config::parse(&text).unwrap();
            f(&mut c);
            self.write(&crate::sync::config_merge::save_text(Some(&text), &c).unwrap());
        }
        fn config(&self) -> Config {
            Config::parse(&self.read()).unwrap()
        }
        /// Merges the repo copy, as a sync does first
        fn pull(&mut self, repo: &Path) -> bool {
            merge_tether_config(repo, self.home.path(), &mut self.state, || Ok(KEY.to_vec()))
                .unwrap()
        }
        /// Exports, as a sync does last, and pushes
        fn push(&mut self, repo: &Path) {
            self.export(repo);
            commit_config_base(self.home.path()).unwrap();
        }
        /// Exports, and the push fails
        fn export(&mut self, repo: &Path) {
            write_synced_config(repo, self.home.path(), &mut self.state, &KEY).unwrap();
        }
        fn sync(&mut self, repo: &Path) {
            self.pull(repo);
            self.push(repo);
        }
    }

    fn repo_copy(repo: &Path) -> Vec<u8> {
        let enc = std::fs::read(repo.join("configs/tether/config.toml.enc")).unwrap();
        crate::security::decrypt(&enc, &KEY).unwrap()
    }

    fn set_repo_copy(repo: &Path, plain: &[u8]) {
        let path = repo.join("configs/tether/config.toml.enc");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, crate::security::encrypt(plain, &KEY).unwrap()).unwrap();
    }

    /// The text 1.13.1 saves: no marker, none of the keys only 2.0 knows
    fn saved_by_1x(text: &[u8], f: impl FnOnce(&mut toml::Table)) -> Vec<u8> {
        let mut t: toml::Table = toml::from_str(std::str::from_utf8(text).unwrap()).unwrap();
        for key in ["config_writer", "dashboard"] {
            t.remove(key);
        }
        let packages = t["packages"].as_table_mut().unwrap();
        for key in [
            "allow_scripts",
            "min_release_age_days",
            "auto_install_from_trusted",
        ] {
            packages.remove(key);
        }
        f(&mut t);
        toml::to_string_pretty(&t).unwrap().into_bytes()
    }

    fn days(c: &Config) -> u32 {
        c.packages.min_release_age_days
    }

    /// An unpatched 1.x machine pushes its copy of an earlier export again verbatim. The copy
    /// has the marker, so it merges as a 2.0 copy and reverts the change made since: the
    /// documented limit. No machine exports to restore the change, so the fleet settles.
    #[test]
    fn a_replayed_copy_reverts_the_change_and_settles() {
        let repo = TempDir::new().unwrap();
        let repo = repo.path();
        let start = toml::to_string_pretty(&Config::default()).unwrap();
        let mut a = Peer::new("a", Some(&start));
        let mut b = Peer::new("b", Some(&start));
        a.sync(repo);
        b.sync(repo);

        a.edit(|c| c.packages.min_release_age_days = 3);
        a.sync(repo);
        let r1 = repo_copy(repo);
        b.sync(repo);
        a.edit(|c| c.packages.min_release_age_days = 14);
        a.sync(repo);
        b.sync(repo);
        assert_eq!(days(&b.config()), 14);

        set_repo_copy(repo, &r1);
        for p in [&mut b, &mut a] {
            p.sync(repo);
            assert_eq!(days(&p.config()), 3);
            assert_eq!(repo_copy(repo), r1);
        }
    }

    /// A 1.x user who sets a value back saves a copy without the marker: it merges
    #[test]
    fn a_value_a_1x_user_sets_back_merges() {
        let repo = TempDir::new().unwrap();
        let repo = repo.path();
        let start = toml::to_string_pretty(&Config::default()).unwrap();
        let mut a = Peer::new("a", Some(&start));
        let mut b = Peer::new("b", Some(&start));
        a.sync(repo);
        b.sync(repo);
        a.edit(|c| {
            c.packages.brew.sync_casks = false;
            c.packages.min_release_age_days = 14;
        });
        a.sync(repo);
        b.sync(repo);
        assert!(!b.config().packages.brew.sync_casks);

        let r = repo_copy(repo);
        let from_1x = saved_by_1x(&r, |t| {
            t["packages"]["brew"]
                .as_table_mut()
                .unwrap()
                .insert("sync_casks".into(), true.into());
        });
        set_repo_copy(repo, &from_1x);
        assert!(b.pull(repo));
        let c = b.config();
        assert!(c.packages.brew.sync_casks);
        assert_eq!(days(&c), 14);
        // The export puts the marker and the 2.0 settings back
        b.push(repo);
        let r = repo_copy(repo);
        assert!(has_marker(&r));
        assert_eq!(
            days(&Config::parse(&String::from_utf8_lossy(&r)).unwrap()),
            14
        );
        a.sync(repo);
        assert!(a.config().packages.brew.sync_casks);
    }

    /// An export becomes the base only once pushed. A pull that discards the commit merges
    /// against the base before the export, so the local edit is not taken as merged.
    #[test]
    fn a_discarded_export_does_not_advance_the_base() {
        let repo = TempDir::new().unwrap();
        let repo = repo.path();
        let start = toml::to_string_pretty(&Config::default()).unwrap();
        let mut a = Peer::new("a", Some(&start));
        let mut b = Peer::new("b", Some(&start));
        a.sync(repo);
        b.sync(repo);
        let base = std::fs::read(config_base_path(a.home.path())).unwrap();

        a.edit(|c| c.packages.min_release_age_days = 3);
        a.pull(repo);
        a.export(repo);
        assert_eq!(
            std::fs::read(config_base_path(a.home.path())).unwrap(),
            base
        );
        // The push fails, and the pull resets the repo to b's export
        b.edit(|c| {
            c.packages.min_release_age_days = 14;
            c.dashboard.theme = Some("mocha".into());
        });
        b.sync(repo);
        a.pull(repo);
        assert!(!config_base_pending_path(a.home.path()).exists());
        let c = a.config();
        assert_eq!(
            c.packages.min_release_age_days, 3,
            "the local edit was lost"
        );
        assert_eq!(c.dashboard.theme.as_deref(), Some("mocha"));

        // A pushed export is the base
        a.push(repo);
        assert_eq!(
            std::fs::read(config_base_path(a.home.path())).unwrap(),
            repo_copy(repo)
        );
    }

    /// A synced copy that does not read stops neither this sync nor the next: the merge and
    /// the export skip it, and the warning shows once per copy
    #[test]
    fn an_unreadable_synced_config_skips_the_config_step() {
        let repo = TempDir::new().unwrap();
        let repo = repo.path();
        let start = toml::to_string_pretty(&Config::default()).unwrap();
        let mut a = Peer::new("a", Some(&start));
        a.sync(repo);
        a.edit(|c| c.packages.min_release_age_days = 3);
        for corrupt in [&b"not = [toml"[..], b"\xff\xfe", b"packages = 1\n"] {
            set_repo_copy(repo, corrupt);
            for _ in 0..2 {
                assert!(!a.pull(repo));
                assert_eq!(
                    a.state.config_error.as_deref(),
                    Some(crate::sha256_hex(corrupt).as_str())
                );
                a.push(repo);
                assert_eq!(repo_copy(repo), corrupt);
            }
        }
        // A copy that does not decrypt
        let path = repo.join("configs/tether/config.toml.enc");
        std::fs::write(&path, b"garbage").unwrap();
        assert!(!a.pull(repo));
        a.push(repo);
        assert_eq!(std::fs::read(&path).unwrap(), b"garbage");
        assert!(a.read().contains("min_release_age_days = 3"));

        // A readable copy merges again, and the export follows
        set_repo_copy(repo, start.as_bytes());
        a.sync(repo);
        assert_eq!(a.state.config_error, None);
        assert_eq!(
            Config::parse(&String::from_utf8_lossy(&repo_copy(repo)))
                .unwrap()
                .packages
                .min_release_age_days,
            3
        );
    }

    /// A synced directory that resolves into ~/.tether, or holds it, never syncs its files
    #[cfg(unix)]
    #[test]
    fn synced_dirs_leave_out_the_tether_dir() {
        let temp = TempDir::new().unwrap();
        let home = temp.path().join("home");
        let repo = temp.path().join("repo");
        std::fs::create_dir_all(home.join(".tether")).unwrap();
        std::fs::write(home.join(".tether/signing_key"), b"secret").unwrap();
        std::fs::create_dir_all(home.join(".config/app")).unwrap();
        std::fs::write(home.join(".config/app/a.conf"), b"a").unwrap();
        std::os::unix::fs::symlink(home.join(".tether"), home.join("tlink")).unwrap();
        let mut config = Config::default();
        config.security.encrypt_dotfiles = false;
        config.dotfiles.dirs = vec![
            "~/tlink".into(),
            "~/.config".into(),
            "~/./.tether".into(),
            "~".into(),
        ];
        let mut state = new_state("me");
        sync_directories(&config, "me", &mut state, &repo, &home, false).unwrap();
        let synced: Vec<String> = walkdir::WalkDir::new(repo.join("configs"))
            .into_iter()
            .filter_map(Result::ok)
            .filter(|e| e.file_type().is_file())
            .map(|e| e.path().to_string_lossy().into_owned())
            .collect();
        assert!(synced.iter().any(|p| p.ends_with(".config/app/a.conf")));
        assert!(
            !synced
                .iter()
                .any(|p| std::fs::read(p).unwrap() == b"secret"),
            "{synced:?}"
        );
    }

    /// A merge or a first copy writes config.toml 0600, also over a file that was 0644
    #[cfg(unix)]
    #[test]
    fn a_synced_config_write_is_private() {
        use std::os::unix::fs::PermissionsExt;
        let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
        let repo = TempDir::new().unwrap();
        let repo = repo.path();
        let start = toml::to_string_pretty(&Config::default()).unwrap();
        let mut a = Peer::new("a", Some(&start));
        let mut b = Peer::new("b", Some(&start));
        a.sync(repo);
        b.sync(repo);
        a.edit(|c| c.packages.min_release_age_days = 3);
        a.sync(repo);
        std::fs::set_permissions(b.config_path(), std::fs::Permissions::from_mode(0o644)).unwrap();
        assert!(b.pull(repo));
        assert_eq!(mode(&b.config_path()), 0o600);
        let mut c = Peer::new("c", None);
        assert!(c.pull(repo));
        assert_eq!(mode(&c.config_path()), 0o600);
    }

    /// state.json records the local file; the hash of the exported copy is kept apart
    #[test]
    fn an_export_records_the_local_file_hash() {
        let repo = TempDir::new().unwrap();
        let repo = repo.path();
        // Without the marker, so the export differs from the local file
        let start = format!(
            "# mine\n{}",
            toml::to_string_pretty(&Config::default())
                .unwrap()
                .replace("config_writer = 2\n", "")
        );
        let mut a = Peer::new("a", Some(&start));
        a.push(repo);
        let exported = repo_copy(repo);
        assert_ne!(exported, start.as_bytes());
        assert_eq!(
            a.state.files[".tether/config.toml"].hash,
            crate::sha256_hex(start.as_bytes())
        );
        assert_eq!(
            a.state.config_export_hash.as_deref(),
            Some(crate::sha256_hex(&exported).as_str())
        );
        // Without a base file, the unchanged local file is the base
        std::fs::remove_file(config_base_path(a.home.path())).unwrap();
        let base = load_config_base(repo, a.home.path(), &a.state, start.as_bytes(), &KEY);
        assert_eq!(base.unwrap().as_deref(), Some(start.as_bytes()));
    }

    /// A recorded hash that matches nothing gives no base: additions from both sides stay and
    /// a setting both changed is a conflict. Only a machine with no recorded hash takes the
    /// remote values.
    #[test]
    fn a_base_that_cannot_be_recovered_merges_without_one() {
        let repo = TempDir::new().unwrap();
        let repo = repo.path();
        let start = toml::to_string_pretty(&Config::default()).unwrap();
        let mut remote = Config::parse(&start).unwrap();
        remote.dashboard.theme = Some("mocha".into());
        remote.packages.min_release_age_days = 14;
        remote.packages.brew.sync_casks = false;
        let remote = crate::sync::config_merge::save_text(Some(&start), &remote).unwrap();
        set_repo_copy(repo, remote.as_bytes());

        let mut a = Peer::new("a", Some(&start));
        a.edit(|c| {
            c.packages.allow_scripts = vec!["esbuild".into()];
            c.packages.min_release_age_days = 3;
        });
        a.state
            .update_file(".tether/config.toml", crate::sha256_hex(b"lost"));
        assert!(a.pull(repo));
        let c = a.config();
        assert_eq!(c.packages.allow_scripts, vec!["esbuild"]);
        assert_eq!(c.dashboard.theme.as_deref(), Some("mocha"));
        assert_eq!(c.packages.min_release_age_days, 3);
        // Without a base, a setting that differs is a change on both sides
        assert!(c.packages.brew.sync_casks);

        // A new machine joins the fleet's config
        let mut b = Peer::new("b", Some(&start));
        assert!(b.pull(repo));
        let c = b.config();
        assert_eq!(c.packages.min_release_age_days, 14);
        assert!(!c.packages.brew.sync_casks);
    }

    /// A new machine copies a 1.x remote without the marker, and its export has the marker
    #[test]
    fn a_copied_1x_config_exports_with_the_marker() {
        let repo = TempDir::new().unwrap();
        let repo = repo.path();
        let start = toml::to_string_pretty(&Config::default()).unwrap();
        let from_1x = saved_by_1x(start.as_bytes(), |_| {});
        set_repo_copy(repo, &from_1x);
        let mut c = Peer::new("c", None);
        assert!(c.pull(repo));
        assert_eq!(c.read().as_bytes(), &from_1x[..]);
        c.push(repo);
        let r = repo_copy(repo);
        assert!(has_marker(&r), "{}", String::from_utf8_lossy(&r));
        // Exported once: the next sync has nothing to export
        c.sync(repo);
        assert_eq!(repo_copy(repo), r);
    }

    #[test]
    fn test_package_uninstalled_since_the_last_record_is_tombstoned() {
        // The last record, signed by this machine, still lists evilpkg; the user has since
        // uninstalled it, so a manifest line for it must not install it again this sync
        let previous = HashMap::from([(
            "npm".to_string(),
            vec!["evilpkg".to_string(), "kept".to_string()],
        )]);
        let mut machine = MachineState::new("me");
        machine
            .packages
            .insert("npm".to_string(), vec!["kept".to_string()]);
        detect_removed_packages(&mut machine, &previous);
        assert_eq!(machine.removed_packages["npm"], vec!["evilpkg"]);
    }

    #[test]
    fn test_write_decrypted_creates_file_with_content() {
        let temp = TempDir::new().unwrap();
        let path = temp.path().join("secret.env");
        let content = b"API_KEY=hunter2";

        write_decrypted(&path, content).unwrap();

        assert_eq!(std::fs::read(&path).unwrap(), content);
    }

    #[cfg(unix)]
    #[test]
    fn test_write_decrypted_sets_secure_permissions() {
        use std::os::unix::fs::PermissionsExt;

        let temp = TempDir::new().unwrap();
        let path = temp.path().join("secret.env");

        write_decrypted(&path, b"secret").unwrap();

        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
    }

    #[cfg(unix)]
    #[test]
    fn test_write_decrypted_overwrites_existing_and_fixes_permissions() {
        use std::os::unix::fs::PermissionsExt;

        let temp = TempDir::new().unwrap();
        let path = temp.path().join("secret.env");

        // Create file with permissive permissions
        std::fs::write(&path, b"old").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();

        write_decrypted(&path, b"new secret").unwrap();

        assert_eq!(std::fs::read(&path).unwrap(), b"new secret");
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
    }

    #[test]
    fn test_write_decrypted_empty_content() {
        let temp = TempDir::new().unwrap();
        let path = temp.path().join("empty");

        write_decrypted(&path, b"").unwrap();

        assert_eq!(std::fs::read(&path).unwrap(), b"");
    }

    #[test]
    fn test_write_decrypted_fails_missing_parent() {
        let temp = TempDir::new().unwrap();
        let path = temp.path().join("nonexistent_dir").join("file");

        assert!(write_decrypted(&path, b"data").is_err());
    }

    #[test]
    fn test_write_decrypted_binary_content() {
        let temp = TempDir::new().unwrap();
        let path = temp.path().join("binary");
        let content: Vec<u8> = (0..=255).collect();

        write_decrypted(&path, &content).unwrap();

        assert_eq!(std::fs::read(&path).unwrap(), content);
    }

    #[cfg(unix)]
    #[test]
    fn test_preserve_executable_bit() {
        use std::os::unix::fs::PermissionsExt;

        let temp = TempDir::new().unwrap();
        let source = temp.path().join("script.sh");
        let dest = temp.path().join("script.sh.enc");

        std::fs::write(&source, b"#!/bin/sh").unwrap();
        std::fs::set_permissions(&source, std::fs::Permissions::from_mode(0o755)).unwrap();
        std::fs::write(&dest, b"encrypted").unwrap();
        std::fs::set_permissions(&dest, std::fs::Permissions::from_mode(0o644)).unwrap();

        preserve_executable_bit(&source, &dest);

        let mode = std::fs::metadata(&dest).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o744);
    }

    #[cfg(unix)]
    #[test]
    fn test_preserve_executable_bit_not_set() {
        use std::os::unix::fs::PermissionsExt;

        let temp = TempDir::new().unwrap();
        let source = temp.path().join("config");
        let dest = temp.path().join("config.enc");

        std::fs::write(&source, b"key=value").unwrap();
        std::fs::set_permissions(&source, std::fs::Permissions::from_mode(0o644)).unwrap();
        std::fs::write(&dest, b"encrypted").unwrap();
        std::fs::set_permissions(&dest, std::fs::Permissions::from_mode(0o644)).unwrap();

        preserve_executable_bit(&source, &dest);

        let mode = std::fs::metadata(&dest).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o644);
    }

    #[test]
    fn config_base_recovers_from_repo_history() {
        let temp = tempfile::tempdir().unwrap();
        let dir = temp.path();
        let git = |args: &[&str]| {
            let status = crate::sync::git::git_command()
                .args(args)
                .current_dir(dir)
                .status()
                .unwrap();
            assert!(status.success());
        };
        git(&["init", "-q", "-b", "main"]);
        let key = [7u8; 32];
        let enc = dir.join("configs/tether/config.toml.enc");
        std::fs::create_dir_all(enc.parent().unwrap()).unwrap();
        for text in ["synced = 1\n", "remote = 2\n"] {
            let data = crate::security::encrypt(text.as_bytes(), &key).unwrap();
            std::fs::write(&enc, data).unwrap();
            git(&["add", "-A"]);
            git(&["commit", "-qm", "config"]);
        }
        let mut state: SyncState = serde_json::from_value(serde_json::json!({
            "machine_id": "me",
            "last_sync": "2026-01-01T00:00:00Z",
            "files": {},
            "packages": {},
        }))
        .unwrap();
        // A machine that never synced its config takes the fleet's
        assert_eq!(
            recover_config_base(dir, &state, b"local", &key).as_deref(),
            Some(&b"local"[..])
        );
        state.update_file(".tether/config.toml", crate::sha256_hex(b"unknown"));
        assert_eq!(recover_config_base(dir, &state, b"local", &key), None);
        // The hash of the export finds the exported copy
        state.config_export_hash = Some(crate::sha256_hex(b"remote = 2\n"));
        assert_eq!(
            recover_config_base(dir, &state, b"local", &key).as_deref(),
            Some(&b"remote = 2\n"[..])
        );
        state.config_export_hash = None;
        state.update_file(".tether/config.toml", crate::sha256_hex(b"synced = 1\n"));
        // The local file was rewritten since, so the base comes from history
        assert_eq!(
            recover_config_base(dir, &state, b"# rewritten\nsynced = 1\n", &key).as_deref(),
            Some(&b"synced = 1\n"[..])
        );
        assert_eq!(
            recover_config_base(dir, &state, b"synced = 1\n", &key).as_deref(),
            Some(&b"synced = 1\n"[..])
        );
    }

    #[test]
    fn config_base_that_does_not_read_comes_from_history() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path();
        let state: SyncState = serde_json::from_value(serde_json::json!({
            "machine_id": "me",
            "last_sync": "2026-01-01T00:00:00Z",
            "files": {},
            "packages": {},
        }))
        .unwrap();
        let local = toml::to_string_pretty(&Config::default()).unwrap();
        write_config_base(home, b"not = [toml").unwrap();
        let base = load_config_base(home, home, &state, local.as_bytes(), &[7u8; 32]).unwrap();
        assert_eq!(base.as_deref(), Some(local.as_bytes()));
        write_config_base(home, local.as_bytes()).unwrap();
        let base = load_config_base(home, home, &state, b"other", &[7u8; 32]).unwrap();
        assert_eq!(base.as_deref(), Some(local.as_bytes()));
    }

    #[cfg(unix)]
    #[test]
    fn config_base_is_private() {
        use std::os::unix::fs::PermissionsExt;
        let temp = tempfile::tempdir().unwrap();
        let path = config_base_path(temp.path());
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, b"old").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        write_config_base(temp.path(), b"new").unwrap();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
    }

    #[test]
    fn profile_warning_only_for_a_different_profile() {
        let assigned = |profile: Option<&str>| {
            let mut c = Config::default();
            if let Some(p) = profile {
                c.machine_profiles.insert("me".into(), p.into());
            }
            c
        };
        // An explicit dev and a missing assignment are the same profile
        assert_eq!(
            changed_profile_warning(&assigned(Some("dev")), &assigned(None), "me"),
            None
        );
        assert_eq!(
            changed_profile_warning(&assigned(None), &assigned(Some("dev")), "me"),
            None
        );
        let warning =
            changed_profile_warning(&assigned(Some("server")), &assigned(None), "me").unwrap();
        assert!(warning.contains("from server to dev"), "{warning}");
        assert!(warning.contains("'tether machines profile set server'"));
        let warning =
            changed_profile_warning(&assigned(None), &assigned(Some("server")), "me").unwrap();
        assert!(warning.contains("from dev to server"), "{warning}");
        assert!(warning.contains("'tether machines profile unset'"));
    }
}

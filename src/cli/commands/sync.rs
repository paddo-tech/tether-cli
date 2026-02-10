use crate::cli::{Output, Progress, Prompt};
use crate::config::Config;
use crate::packages::{
    BrewManager, BunManager, GemManager, NpmManager, PackageManager, PnpmManager, UvManager,
    WingetManager,
};
use crate::sync::git::{find_git_repos, get_remote_url, normalize_remote_url};
use crate::sync::{
    import_packages, sync_packages, GitBackend, MachineState, SyncEngine, SyncState,
};
use anyhow::Result;
use sha2::{Digest, Sha256};
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

pub async fn run(dry_run: bool, _force: bool) -> Result<()> {
    if dry_run {
        Output::info("Dry-run mode");
    }

    // Acquire sync lock (wait up to 2s for other syncs to finish)
    let _sync_lock = if !dry_run {
        Some(crate::sync::acquire_sync_lock(true)?)
    } else {
        None
    };

    let config = Config::load()?;

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

    let dotfiles_dir = sync_path.join("dotfiles");
    std::fs::create_dir_all(&dotfiles_dir)?;

    // Always sync tether config first (hardcoded, not dependent on config)
    // This ensures config changes from other machines are applied before using config
    if config.security.encrypt_dotfiles && !dry_run {
        if let Some(new_config) = sync_tether_config(&sync_path, &home)? {
            config = new_config;
        }
    }

    let mut state = SyncState::load()?;

    // Load machine state early to get ignored lists for decrypt phase
    let machine_state_for_decrypt =
        MachineState::load_from_repo(&sync_path, &state.machine_id)?.unwrap_or_default();

    // Apply dotfiles from sync repo (if encrypted) - with conflict detection
    // Interactive mode when run manually, non-interactive when run by daemon
    let interactive = !crate::daemon::is_daemon_mode();
    if config.security.encrypt_dotfiles && !dry_run {
        decrypt_from_repo(
            &config,
            &sync_path,
            &home,
            &state,
            &machine_state_for_decrypt,
            interactive,
        )?;
    }

    // Sync dotfiles (local → Git) - only if personal dotfiles enabled
    if config.features.personal_dotfiles {
        // Sync individual dotfiles (with glob expansion)
        for entry in &config.dotfiles.files {
            // Validate path before expansion to prevent traversal attacks
            if !entry.is_safe_path() {
                Output::warning(&format!("Skipping unsafe dotfile path: {}", entry.path()));
                continue;
            }

            let pattern = entry.path();
            let expanded = crate::sync::expand_dotfile_glob(pattern, &home);

            for file in expanded {
                let source = home.join(&file);

                if source.exists() {
                    if let Ok(content) = std::fs::read(&source) {
                        let hash = format!("{:x}", Sha256::digest(&content));

                        let file_changed = state
                            .files
                            .get(&file)
                            .map(|f| f.hash != hash)
                            .unwrap_or(true);

                        if file_changed && !dry_run {
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
                        }
                    }
                }
            }
        }

        // Auto-discover directories sourced from shell configs and add to config
        if !dry_run {
            let discovered = crate::sync::discover_sourced_dirs(&home, &config.dotfiles.files);
            let mut config_changed = false;
            for dir in discovered {
                if !config.dotfiles.dirs.contains(&dir) {
                    Output::info(&format!("Auto-discovered sourced directory: {}", dir));
                    config.dotfiles.dirs.push(dir);
                    config_changed = true;
                }
            }
            if config_changed {
                config.dotfiles.dirs.sort();
                config.save()?;
            }
        }

        // Sync global config directories
        if !config.dotfiles.dirs.is_empty() {
            sync_directories(&config, &mut state, &sync_path, &home, dry_run)?;
        }

        // Sync project-local configs (personal)
        if config.project_configs.enabled {
            sync_project_configs(&config, &mut state, &sync_path, &home, dry_run)?;
        }
    } // end personal dotfiles feature block

    // Sync team project secrets
    if !dry_run {
        sync_team_project_secrets(&config, &home)?;
    }

    // Build machine state first (to know what's installed locally + respect removed_packages)
    let mut machine_state = build_machine_state(&config, &state, &sync_path).await?;

    // Import packages from manifests (install missing packages, respecting removed_packages)
    // Interactive mode: install deferred casks from daemon syncs
    if config.features.personal_packages && !dry_run {
        let deferred_casks = state.deferred_casks.clone();

        import_packages(
            &config,
            &sync_path,
            &mut state,
            &machine_state,
            false, // interactive mode
            &deferred_casks,
        )
        .await?;

        // Clear deferred casks after interactive sync (user had their chance)
        if !state.deferred_casks.is_empty() {
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
        machine_state.save_to_repo(&sync_path)?;
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
            let pb = Progress::spinner("Pushing changes...");
            git.commit("Sync dotfiles and packages", &state.machine_id)?;
            git.push()?;
            pb.finish_and_clear();
        }

        state.mark_synced();
        state.save()?;
    }

    // Check and push team repo changes (if write access enabled)
    if !dry_run {
        if let Some(team) = &config.team {
            if team.enabled && !team.read_only {
                let team_sync_dir = Config::team_sync_dir()?;
                if team_sync_dir.exists() {
                    let team_git = GitBackend::open(&team_sync_dir)?;

                    if team_git.has_changes()? {
                        // Scan for secrets before pushing to team repo
                        let dotfiles_dir = team_sync_dir.join("dotfiles");
                        if dotfiles_dir.exists() {
                            for entry in std::fs::read_dir(&dotfiles_dir)? {
                                let entry = entry?;
                                if entry.file_type()?.is_file() {
                                    if let Ok(findings) =
                                        crate::security::scan_for_secrets(&entry.path())
                                    {
                                        if !findings.is_empty() {
                                            Output::error(&format!(
                                                "Team push blocked: {} contains {} secret(s)",
                                                entry.file_name().to_string_lossy(),
                                                findings.len()
                                            ));
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
    }

    // Sync collab secrets (only if feature enabled)
    if !dry_run && config.features.collab_secrets {
        sync_collab_secrets(&config, &home)?;
    }

    // Prune old backups
    if let Ok(pruned) = crate::sync::prune_old_backups() {
        if pruned > 0 {
            log::debug!("Pruned {} old backup(s)", pruned);
        }
    }

    Output::success("Synced");
    Ok(())
}

/// Sync secrets from collab repos to local projects
fn sync_collab_secrets(config: &Config, home: &Path) -> Result<()> {
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

                        // Only write if content differs
                        let should_write = if dest.exists() {
                            std::fs::read(&dest).map(|c| c != decrypted).unwrap_or(true)
                        } else {
                            true
                        };

                        if should_write {
                            std::fs::write(&dest, &decrypted)?;
                            log::debug!(
                                "Synced collab secret: {} -> {}",
                                filename,
                                local_project.display()
                            );
                        }
                    }
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

fn decrypt_from_repo(
    config: &Config,
    sync_path: &Path,
    home: &Path,
    state: &SyncState,
    machine_state: &MachineState,
    interactive: bool,
) -> Result<()> {
    use crate::sync::{
        backup_file, create_backup_dir, detect_conflict, ConflictResolution, ConflictState,
    };

    let key = crate::security::get_encryption_key()?;
    let dotfiles_dir = sync_path.join("dotfiles");
    let mut conflict_state = ConflictState::load().unwrap_or_default();
    let mut new_conflicts = Vec::new();

    // Create backup directory for this sync (lazily - only if needed)
    let mut backup_dir: Option<PathBuf> = None;

    for entry in &config.dotfiles.files {
        // Validate path before expansion to prevent traversal attacks
        if !entry.is_safe_path() {
            Output::warning(&format!("Skipping unsafe dotfile path: {}", entry.path()));
            continue;
        }

        let pattern = entry.path();
        // Glob patterns default to create_if_missing = true (sync all matching files from other machines)
        let create_if_missing = entry.create_if_missing() || crate::sync::is_glob_pattern(pattern);

        // Expand glob pattern by scanning sync repo for matching .enc files
        let expanded = crate::sync::expand_from_sync_repo(pattern, &dotfiles_dir);

        for file in expanded {
            // Skip if this dotfile is ignored on this machine
            if machine_state.ignored_dotfiles.iter().any(|f| f == &file) {
                continue;
            }

            let filename = file.trim_start_matches('.');
            let enc_file = dotfiles_dir.join(format!("{}.enc", filename));

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

                        // Check for conflict
                        if let Some(conflict) =
                            detect_conflict(&file, &local_file, &plaintext, last_synced_hash)
                        {
                            if interactive {
                                // Interactive mode: prompt user
                                conflict.show_diff()?;
                                let resolution = conflict.prompt_resolution()?;

                                match resolution {
                                    ConflictResolution::KeepLocal => {}
                                    ConflictResolution::UseRemote => {
                                        // Backup before overwriting
                                        if local_file.exists() {
                                            if backup_dir.is_none() {
                                                backup_dir = Some(create_backup_dir()?);
                                            }
                                            backup_file(
                                                backup_dir.as_ref().unwrap(),
                                                "dotfiles",
                                                &file,
                                                &local_file,
                                            )?;
                                        }
                                        std::fs::write(&local_file, &plaintext)?;
                                        conflict_state.remove_conflict(&file);
                                    }
                                    ConflictResolution::Merged => {
                                        conflict.launch_merge_tool(&config.merge, home)?;
                                        conflict_state.remove_conflict(&file);
                                    }
                                    ConflictResolution::Skip => {
                                        new_conflicts.push((
                                            file.to_string(),
                                            conflict.local_hash.clone(),
                                            conflict.remote_hash.clone(),
                                        ));
                                    }
                                }
                            } else {
                                // Non-interactive (daemon): save conflict for later
                                Output::warning(&format!("  {} (conflict - skipped)", file));
                                new_conflicts.push((
                                    file.to_string(),
                                    conflict.local_hash.clone(),
                                    conflict.remote_hash.clone(),
                                ));
                            }
                        } else {
                            // No true conflict - but preserve local-only changes
                            let remote_hash = format!("{:x}", Sha256::digest(&plaintext));
                            let local_hash = std::fs::read(&local_file)
                                .ok()
                                .map(|c| format!("{:x}", Sha256::digest(&c)));

                            // Only write if local unchanged from last sync AND remote differs
                            let local_unchanged = local_hash.as_deref() == last_synced_hash;
                            if local_unchanged && local_hash.as_ref() != Some(&remote_hash) {
                                // Backup before overwriting
                                if local_file.exists() {
                                    if backup_dir.is_none() {
                                        backup_dir = Some(create_backup_dir()?);
                                    }
                                    backup_file(
                                        backup_dir.as_ref().unwrap(),
                                        "dotfiles",
                                        &file,
                                        &local_file,
                                    )?;
                                }
                                std::fs::write(&local_file, plaintext)?;
                            }
                            conflict_state.remove_conflict(&file);
                        }
                    }
                    Err(e) => {
                        Output::warning(&format!("  {} (failed to decrypt: {})", file, e));
                    }
                }
            }
        }
    }

    // Save any new conflicts
    for (file, local_hash, remote_hash) in &new_conflicts {
        conflict_state.add_conflict(file, local_hash, remote_hash);
    }

    if !new_conflicts.is_empty() {
        conflict_state.save()?;
        if !interactive {
            // Send notification for daemon mode
            crate::sync::notify_conflicts(new_conflicts.len()).ok();
        }
    } else {
        conflict_state.save()?;
    }

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

                    // Validate path is safe (defense-in-depth)
                    if !crate::config::is_safe_dotfile_path(rel_path_no_enc) {
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
                                let remote_hash = format!("{:x}", Sha256::digest(&plaintext));
                                let local_hash = std::fs::read(&local_file)
                                    .ok()
                                    .map(|c| format!("{:x}", Sha256::digest(&c)));
                                let local_unchanged = local_hash.as_deref() == last_synced_hash;
                                if local_unchanged && local_hash.as_ref() != Some(&remote_hash) {
                                    std::fs::write(&local_file, plaintext)?;
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

/// Ensure checkout_file is a symlink pointing to canonical_path.
/// Handles: missing, wrong symlink, real file (migrates to symlink).
fn ensure_symlink(checkout_file: &Path, canonical_path: &Path) -> Result<()> {
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

    crate::sync::create_symlink(canonical_path, checkout_file)?;
    Ok(())
}

fn decrypt_project_configs(
    config: &Config,
    sync_path: &Path,
    home: &Path,
    machine_state: &MachineState,
    state: &SyncState,
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
                            let remote_hash = format!("{:x}", Sha256::digest(&plaintext));
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
                                    let local_hash =
                                        format!("{:x}", Sha256::digest(&local_content));
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
                                let canonical_hash = canonical_content
                                    .as_ref()
                                    .map(|c| format!("{:x}", Sha256::digest(c)));

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
                                }
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

/// Sync tether config from remote (always, independent of config file list)
/// Only applies remote if local config hasn't changed since last sync (to avoid overwriting local edits)
/// Returns Some(config) if remote config was applied, None otherwise
fn sync_tether_config(sync_path: &Path, home: &Path) -> Result<Option<Config>> {
    let enc_file = sync_path.join("dotfiles/tether/config.toml.enc");

    if !enc_file.exists() {
        return Ok(None);
    }

    let key = crate::security::get_encryption_key()?;
    let encrypted_content = std::fs::read(&enc_file)?;

    match crate::security::decrypt(&encrypted_content, &key) {
        Ok(plaintext) => {
            let local_config_path = home.join(".tether/config.toml");
            let local_content = std::fs::read(&local_config_path).ok();

            let remote_hash = format!("{:x}", Sha256::digest(&plaintext));
            let local_hash = local_content
                .as_ref()
                .map(|c| format!("{:x}", Sha256::digest(c)));

            // Check if local has changed since last sync
            let state = SyncState::load().ok();
            let last_synced_hash = state
                .as_ref()
                .and_then(|s| s.files.get(".tether/config.toml"))
                .map(|f| f.hash.as_str());

            let local_changed = local_hash.as_deref() != last_synced_hash;
            let remote_changed = Some(remote_hash.as_str()) != last_synced_hash;

            // Only apply remote if:
            // - Local hasn't changed (safe to overwrite) OR local doesn't exist yet
            // - AND remote has changed OR local doesn't exist yet
            let should_apply = (!local_changed || local_content.is_none())
                && (remote_changed || local_content.is_none());

            if should_apply {
                // Apply remote config
                if let Some(parent) = local_config_path.parent() {
                    std::fs::create_dir_all(parent)?;
                }
                std::fs::write(&local_config_path, &plaintext)?;

                // Reload config
                let new_config = Config::load()?;
                return Ok(Some(new_config));
            }
            // If local changed, local wins - it will be exported later
        }
        Err(e) => {
            Output::warning(&format!("Failed to decrypt tether config: {}", e));
        }
    }

    Ok(None)
}

/// Export tether config to sync repo (always, independent of config file list)
fn export_tether_config(sync_path: &Path, home: &Path, state: &mut SyncState) -> Result<()> {
    let config_path = home.join(".tether/config.toml");

    if !config_path.exists() {
        return Ok(());
    }

    let content = std::fs::read(&config_path)?;
    let hash = format!("{:x}", Sha256::digest(&content));

    let dest_dir = sync_path.join("dotfiles/tether");
    std::fs::create_dir_all(&dest_dir)?;

    let dest = dest_dir.join("config.toml.enc");

    // Check if file on disk differs
    let file_hash = std::fs::read(&dest).ok().and_then(|enc| {
        let key = crate::security::get_encryption_key().ok()?;
        crate::security::decrypt(&enc, &key)
            .ok()
            .map(|plain| format!("{:x}", Sha256::digest(&plain)))
    });

    if file_hash.as_ref() != Some(&hash) {
        let key = crate::security::get_encryption_key()?;
        let encrypted = crate::security::encrypt(&content, &key)?;
        std::fs::write(&dest, encrypted)?;
        state.update_file(".tether/config.toml", hash);
    }

    Ok(())
}

fn sync_directories(
    config: &Config,
    state: &mut SyncState,
    sync_path: &Path,
    home: &Path,
    dry_run: bool,
) -> Result<()> {
    use walkdir::WalkDir;

    let configs_dir = sync_path.join("configs");
    std::fs::create_dir_all(&configs_dir)?;

    for dir_path in &config.dotfiles.dirs {
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

        if expanded_path.is_file() {
            if let Ok(content) = std::fs::read(&expanded_path) {
                let hash = format!("{:x}", Sha256::digest(&content));
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
                        std::fs::write(format!("{}.enc", dest.display()), encrypted)?;
                    } else {
                        std::fs::write(&dest, &content)?;
                    }

                    state.update_file(dir_path, hash);
                }
            }
        } else if expanded_path.is_dir() {
            for entry in WalkDir::new(&expanded_path).follow_links(false) {
                let entry = match entry {
                    Ok(e) => e,
                    Err(_) => continue,
                };

                if entry.file_type().is_file() {
                    let file_path = entry.path();
                    let rel_to_home = file_path.strip_prefix(home).unwrap_or(file_path);
                    let state_key =
                        format!("~/{}", rel_to_home.to_string_lossy().replace('\\', "/"));

                    if let Ok(content) = std::fs::read(file_path) {
                        let hash = format!("{:x}", Sha256::digest(&content));
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
                                std::fs::write(format!("{}.enc", dest.display()), encrypted)?;
                            } else {
                                std::fs::write(&dest, &content)?;
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

fn sync_project_configs(
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
                    .follow_links(false)
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
                        let hash = format!("{:x}", Sha256::digest(&content));

                        let rel_to_repo = file_path
                            .strip_prefix(&repo_path)
                            .map_err(|e| anyhow::anyhow!("Failed to strip prefix: {}", e))?;
                        let state_key = format!(
                            "project:{}/{}",
                            normalized_url,
                            rel_to_repo.to_string_lossy().replace('\\', "/")
                        );

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
                                std::fs::write(format!("{}.enc", dest.display()), encrypted)?;
                            } else {
                                std::fs::write(&dest, &content)?;
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
async fn build_machine_state(
    config: &Config,
    state: &SyncState,
    sync_path: &Path,
) -> Result<MachineState> {
    // Load existing machine state to preserve removed_packages
    let mut machine_state = MachineState::load_from_repo(sync_path, &state.machine_id)?
        .unwrap_or_else(|| MachineState::new(&state.machine_id));

    // Update last_sync time and CLI version
    machine_state.last_sync = chrono::Utc::now();
    machine_state.cli_version = env!("CARGO_PKG_VERSION").to_string();

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

    // Homebrew
    if config.packages.brew.enabled {
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
        (config.packages.npm.enabled, Box::new(NpmManager::new())),
        (config.packages.pnpm.enabled, Box::new(PnpmManager::new())),
        (config.packages.bun.enabled, Box::new(BunManager::new())),
        (config.packages.gem.enabled, Box::new(GemManager::new())),
        (config.packages.uv.enabled, Box::new(UvManager::new())),
    ];

    for (enabled, manager) in managers {
        if enabled && manager.is_available().await {
            if let Ok(packages) = manager.list_installed().await {
                machine_state.packages.insert(
                    manager.name().to_string(),
                    packages.iter().map(|p| p.name.clone()).collect(),
                );
            }
        }
    }

    // winget
    if config.packages.winget.enabled {
        let winget = WingetManager::new();
        if winget.is_available().await {
            if let Ok(packages) = winget.list_installed().await {
                machine_state.packages.insert(
                    "winget".to_string(),
                    packages.iter().map(|p| p.name.clone()).collect(),
                );
            }
        }
    }

    // Detect removed packages: packages that were in previous state but not installed now
    detect_removed_packages(&mut machine_state, &previous_packages);

    // Populate dotfiles list from config (files that exist locally, with glob expansion)
    let home = crate::home_dir()?;
    machine_state.dotfiles.clear();
    for entry in &config.dotfiles.files {
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
pub fn sync_team_project_secrets(config: &Config, home: &Path) -> Result<()> {
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
                            for local_project in checkouts {
                                let local_file = local_project.join(rel_file_no_age);

                                // Only write if different or doesn't exist
                                let should_write = if local_file.exists() {
                                    std::fs::read(&local_file)
                                        .map(|existing| existing != decrypted)
                                        .unwrap_or(true)
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
                                    std::fs::write(&local_file, &decrypted)?;
                                    Output::success(&format!(
                                        "Team secret: {} → {}",
                                        rel_file_no_age,
                                        local_project.file_name().unwrap().to_string_lossy()
                                    ));
                                }
                            }
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
            if !team_config.read_only && team_git.has_changes()? {
                let state = SyncState::load()?;
                team_git.commit("Update team configs", &state.machine_id)?;
                team_git.push()?;
            }
        } else {
            Output::success(&format!("Team '{}' synced", team_name));
        }
    }

    // Sync team project secrets to local projects
    if !dry_run {
        sync_team_project_secrets(config, &home)?;
    }

    Output::success("Team sync complete");
    Ok(())
}

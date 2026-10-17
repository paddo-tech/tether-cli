use crate::cli::{Output, Progress, Prompt};
use crate::config::Config;
use crate::sync::steps::{
    adopt_remote, build_machine_state, decrypt_from_repo, export_tether_config,
    preserve_executable_bit, promote_config_base, sync_collab_secrets, sync_directories,
    sync_project_configs, sync_team_project_secrets,
};
use crate::sync::{import_packages, sync_packages, GitBackend, SyncEngine, SyncState};
use anyhow::Result;
use std::path::Path;

pub async fn run(dry_run: bool, rediscover: bool) -> Result<()> {
    // Acquire sync lock, waiting for any running sync to finish
    let _sync_lock = if !dry_run {
        Some(crate::sync::acquire_sync_lock(true)?)
    } else {
        None
    };
    run_locked(dry_run, rediscover).await
}

/// A sync for a caller that already holds the sync lock, or a dry run.
pub async fn run_locked(dry_run: bool, rediscover: bool) -> Result<()> {
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

    let mut state = SyncState::load()?;
    for result in adopt_remote(&mut config, &git, &sync_path, &home, &mut state, dry_run).await? {
        let line = format!("Fleet command {}: {}", result.status.label(), result.action);
        match &result.error {
            Some(error) => Output::warning(&format!("{}: {}", line, error)),
            None => Output::info(&line),
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
        export_tether_config(&sync_path, &home)?;
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
        promote_config_base(
            &sync_path,
            &home,
            &mut state,
            crate::security::get_encryption_key,
        )?;
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
                            let ignored = crate::security::ignored_patterns(&config)?;
                            for entry in std::fs::read_dir(&dotfiles_dir)? {
                                let entry = entry?;
                                if entry.file_type()?.is_file() {
                                    if let Ok(findings) = crate::security::scan_team_file(
                                        &config,
                                        &ignored,
                                        &entry.path(),
                                    ) {
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

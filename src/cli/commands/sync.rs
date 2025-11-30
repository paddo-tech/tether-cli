use crate::cli::{Output, Prompt};
use crate::config::Config;
use crate::packages::{
    BrewManager, BunManager, GemManager, NpmManager, PackageManager, PnpmManager,
};
use crate::sync::{
    import_packages, sync_packages, GitBackend, MachineState, SyncEngine, SyncState,
};
use anyhow::Result;
use sha2::{Digest, Sha256};
use std::collections::HashSet;
use std::path::{Path, PathBuf};

pub async fn run(dry_run: bool, _force: bool) -> Result<()> {
    if dry_run {
        Output::info("Dry-run mode");
    }

    let mut config = Config::load()?;

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
    let home = home::home_dir().ok_or_else(|| anyhow::anyhow!("Could not find home directory"))?;

    // Pull latest changes from personal repo
    let git = GitBackend::open(&sync_path)?;
    if !dry_run {
        git.pull()?;
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
    let interactive = std::env::var("TETHER_DAEMON").is_err();
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

    // Sync dotfiles (local → Git)

    // Sync individual dotfiles
    for entry in &config.dotfiles.files {
        let file = entry.path();
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
                    scan_and_warn_secrets(&config, &source, file);

                    if !dry_run {
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

    // Sync project-local configs
    if config.project_configs.enabled {
        sync_project_configs(&config, &mut state, &sync_path, &home, dry_run)?;
    }

    // Build machine state first (to know what's installed locally + respect removed_packages)
    let mut machine_state = build_machine_state(&config, &state, &sync_path).await?;

    // Import packages from manifests (install missing packages, respecting removed_packages)
    if !dry_run {
        import_packages(&config, &sync_path, &machine_state).await?;

        // Rebuild machine state after import to capture newly installed packages
        machine_state = build_machine_state(&config, &state, &sync_path).await?;
    }

    // Export package manifests using union of all machine states
    sync_packages(&config, &mut state, &sync_path, &machine_state, dry_run).await?;

    // Save machine state for cross-machine comparison
    if !dry_run {
        machine_state.save_to_repo(&sync_path)?;
    }

    // Always export tether config (hardcoded, not dependent on config file list)
    if config.security.encrypt_dotfiles && !dry_run {
        export_tether_config(&sync_path, &home, &mut state)?;
    }

    // Commit and push changes
    if !dry_run {
        // Check if there are any changes (including machine state update)
        let has_changes = git.has_changes()?;

        if has_changes {
            git.commit("Sync dotfiles and packages", &state.machine_id)?;
            git.push()?;
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
                        team_git.commit("Update team configs", &state.machine_id)?;
                        team_git.push()?;
                    }
                }
            }
        }
    }

    Output::success("Synced");
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
    use crate::sync::{detect_conflict, ConflictResolution, ConflictState};

    let key = crate::security::get_encryption_key()?;
    let dotfiles_dir = sync_path.join("dotfiles");
    let mut conflict_state = ConflictState::load().unwrap_or_default();
    let mut new_conflicts = Vec::new();

    for entry in &config.dotfiles.files {
        let file = entry.path();
        // Skip if this dotfile is ignored on this machine
        if machine_state.ignored_dotfiles.iter().any(|f| f == file) {
            continue;
        }

        let filename = file.trim_start_matches('.');
        let enc_file = dotfiles_dir.join(format!("{}.enc", filename));

        if enc_file.exists() {
            let encrypted_content = std::fs::read(&enc_file)?;
            match crate::security::decrypt_file(&encrypted_content, &key) {
                Ok(plaintext) => {
                    let local_file = home.join(file);

                    // Skip if file doesn't exist and create_if_missing is false
                    if !local_file.exists() && !entry.create_if_missing() {
                        continue;
                    }

                    let last_synced_hash = state.files.get(file).map(|f| f.hash.as_str());

                    // Check for conflict
                    if let Some(conflict) =
                        detect_conflict(file, &local_file, &plaintext, last_synced_hash)
                    {
                        if interactive {
                            // Interactive mode: prompt user
                            conflict.show_diff()?;
                            let resolution = conflict.prompt_resolution()?;

                            match resolution {
                                ConflictResolution::KeepLocal => {}
                                ConflictResolution::UseRemote => {
                                    std::fs::write(&local_file, &plaintext)?;
                                    conflict_state.remove_conflict(file);
                                }
                                ConflictResolution::Merged => {
                                    conflict.launch_merge_tool(&config.merge, home)?;
                                    conflict_state.remove_conflict(file);
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
                        // No conflict - check if file actually changed before writing
                        let remote_hash = format!("{:x}", Sha256::digest(&plaintext));
                        let local_hash = std::fs::read(&local_file)
                            .ok()
                            .map(|c| format!("{:x}", Sha256::digest(&c)));

                        if local_hash.as_ref() != Some(&remote_hash) {
                            std::fs::write(&local_file, plaintext)?;
                        }
                        conflict_state.remove_conflict(file);
                    }
                }
                Err(e) => {
                    Output::warning(&format!("  {} (failed to decrypt: {})", file, e));
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

                    if let Ok(encrypted_content) = std::fs::read(file_path) {
                        match crate::security::decrypt_file(&encrypted_content, &key) {
                            Ok(plaintext) => {
                                let local_file = home.join(rel_path_no_enc);
                                if let Some(parent) = local_file.parent() {
                                    std::fs::create_dir_all(parent)?;
                                }
                                // Skip write if content is unchanged
                                let remote_hash = format!("{:x}", Sha256::digest(&plaintext));
                                let local_hash = std::fs::read(&local_file)
                                    .ok()
                                    .map(|c| format!("{:x}", Sha256::digest(&c)));
                                if local_hash.as_ref() != Some(&remote_hash) {
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
        decrypt_project_configs(config, sync_path, home, machine_state, &key)?;
    }

    Ok(())
}

fn decrypt_project_configs(
    config: &Config,
    sync_path: &Path,
    home: &Path,
    machine_state: &MachineState,
    key: &[u8],
) -> Result<()> {
    use crate::sync::git::{find_git_repos, get_remote_url, normalize_remote_url};
    use walkdir::WalkDir;

    let projects_dir = sync_path.join("projects");
    if !projects_dir.exists() {
        return Ok(());
    }

    let mut repo_map = std::collections::HashMap::new();
    for search_path_str in &config.project_configs.search_paths {
        let search_path = if let Some(stripped) = search_path_str.strip_prefix("~/") {
            home.join(stripped)
        } else {
            PathBuf::from(search_path_str)
        };

        if let Ok(repos) = find_git_repos(&search_path) {
            for repo_path in repos {
                if let Ok(remote_url) = get_remote_url(&repo_path) {
                    let normalized = normalize_remote_url(&remote_url);
                    repo_map.insert(normalized, repo_path);
                }
            }
        }
    }

    // Find all unique project names from encrypted files
    let mut projects_in_sync: std::collections::HashSet<String> = std::collections::HashSet::new();

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

        if let Some(local_repo_path) = repo_map.get(project_name) {
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
                    if let Some(ignored_paths) =
                        machine_state.ignored_project_configs.get(project_name)
                    {
                        if ignored_paths.contains(&rel_path_no_enc.to_string()) {
                            continue;
                        }
                    }

                    if let Ok(encrypted_content) = std::fs::read(enc_file) {
                        match crate::security::decrypt_file(&encrypted_content, key) {
                            Ok(plaintext) => {
                                let local_file = local_repo_path.join(rel_path_no_enc);
                                if let Some(parent) = local_file.parent() {
                                    std::fs::create_dir_all(parent)?;
                                }
                                // Skip write if content is unchanged
                                let remote_hash = format!("{:x}", Sha256::digest(&plaintext));
                                let local_hash = std::fs::read(&local_file)
                                    .ok()
                                    .map(|c| format!("{:x}", Sha256::digest(&c)));
                                if local_hash.as_ref() != Some(&remote_hash) {
                                    std::fs::write(&local_file, plaintext)?;
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
        } else {
            Output::warning(&format!(
                "Project config skipped: {} (repo not found in search paths)",
                project_name
            ));
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

    match crate::security::decrypt_file(&encrypted_content, &key) {
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
    let file_hash = std::fs::read(&dest)
        .ok()
        .and_then(|enc| {
            let key = crate::security::get_encryption_key().ok()?;
            crate::security::decrypt_file(&enc, &key)
                .ok()
                .map(|plain| format!("{:x}", Sha256::digest(&plain)))
        });

    if file_hash.as_ref() != Some(&hash) {
        let key = crate::security::get_encryption_key()?;
        let encrypted = crate::security::encrypt_file(&content, &key)?;
        std::fs::write(&dest, encrypted)?;
        state.update_file(".tether/config.toml", hash);
    }

    Ok(())
}

fn scan_and_warn_secrets(config: &Config, source: &Path, file: &str) {
    if config.security.scan_secrets {
        if let Ok(findings) = crate::security::scan_for_secrets(source) {
            if !findings.is_empty() {
                Output::warning(&format!(
                    "{} has {} secret(s) - will be encrypted",
                    file,
                    findings.len()
                ));
            }
        }
    }
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
                        let encrypted = crate::security::encrypt_file(&content, &key)?;
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
                    let state_key = format!("~/{}", rel_to_home.display());

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
                                let encrypted = crate::security::encrypt_file(&content, &key)?;
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
        find_git_repos, get_remote_url, is_gitignored, normalize_remote_url, should_skip_dir,
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

            for pattern in &config.project_configs.patterns {
                let walker = WalkDir::new(&repo_path)
                    .follow_links(false)
                    .max_depth(5)
                    .into_iter()
                    .filter_entry(|e| {
                        e.file_type().is_file()
                            || e.file_name()
                                .to_str()
                                .map(|n| !should_skip_dir(n))
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

                    let matches = if pattern.contains('*') {
                        let pattern_parts: Vec<&str> = pattern.split('*').collect();
                        if pattern_parts.len() == 2 {
                            file_name.starts_with(pattern_parts[0])
                                && file_name.ends_with(pattern_parts[1])
                        } else {
                            false
                        }
                    } else {
                        file_name == pattern.as_str()
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
                                let encrypted = crate::security::encrypt_file(&content, &key)?;
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

    // Update last_sync time
    machine_state.last_sync = chrono::Utc::now();

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

    // npm
    if config.packages.npm.enabled {
        let npm = NpmManager::new();
        if npm.is_available().await {
            if let Ok(packages) = npm.list_installed().await {
                machine_state.packages.insert(
                    "npm".to_string(),
                    packages.iter().map(|p| p.name.clone()).collect(),
                );
            }
        }
    }

    // pnpm
    if config.packages.pnpm.enabled {
        let pnpm = PnpmManager::new();
        if pnpm.is_available().await {
            if let Ok(packages) = pnpm.list_installed().await {
                machine_state.packages.insert(
                    "pnpm".to_string(),
                    packages.iter().map(|p| p.name.clone()).collect(),
                );
            }
        }
    }

    // bun
    if config.packages.bun.enabled {
        let bun = BunManager::new();
        if bun.is_available().await {
            if let Ok(packages) = bun.list_installed().await {
                machine_state.packages.insert(
                    "bun".to_string(),
                    packages.iter().map(|p| p.name.clone()).collect(),
                );
            }
        }
    }

    // gem
    if config.packages.gem.enabled {
        let gem = GemManager::new();
        if gem.is_available().await {
            if let Ok(packages) = gem.list_installed().await {
                machine_state.packages.insert(
                    "gem".to_string(),
                    packages.iter().map(|p| p.name.clone()).collect(),
                );
            }
        }
    }

    // Detect removed packages: packages that were in previous state but not installed now
    detect_removed_packages(&mut machine_state, &previous_packages);

    // Populate dotfiles list from config (files that exist locally)
    let home = home::home_dir().ok_or_else(|| anyhow::anyhow!("Could not find home directory"))?;
    machine_state.dotfiles.clear();
    for entry in &config.dotfiles.files {
        let file = entry.path();
        if home.join(file).exists() {
            machine_state.dotfiles.push(file.to_string());
        }
    }
    machine_state.dotfiles.sort();

    // Populate project_configs from state (tracked project files)
    machine_state.project_configs.clear();
    for key in state.files.keys() {
        if let Some(rest) = key.strip_prefix("project:") {
            if let Some((project_key, rel_path)) = rest.split_once('/') {
                machine_state
                    .project_configs
                    .entry(project_key.to_string())
                    .or_default()
                    .push(rel_path.to_string());
            }
        }
    }
    // Sort for deterministic output
    for paths in machine_state.project_configs.values_mut() {
        paths.sort();
        paths.dedup();
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

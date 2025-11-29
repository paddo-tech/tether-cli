use crate::cli::Output;
use crate::config::Config;
use crate::packages::{
    BrewManager, BunManager, GemManager, NpmManager, PackageManager, PnpmManager,
};
use crate::sync::{GitBackend, MachineState, SyncEngine, SyncState};
use anyhow::Result;
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

pub async fn run(dry_run: bool, _force: bool) -> Result<()> {
    if dry_run {
        Output::info("Running in dry-run mode...");
    } else {
        Output::info("Starting sync...");
    }

    let config = Config::load()?;
    let sync_path = SyncEngine::sync_path()?;
    let home = home::home_dir().ok_or_else(|| anyhow::anyhow!("Could not find home directory"))?;

    // Pull latest changes from personal repo
    Output::info("Pulling latest changes...");
    let git = GitBackend::open(&sync_path)?;
    if !dry_run {
        git.pull()?;
    }
    Output::success("Pulled latest changes");

    // Pull from team repo if enabled
    if let Some(team) = &config.team {
        if team.enabled {
            Output::info("Pulling team configs...");
            let team_sync_dir = Config::team_sync_dir()?;

            if team_sync_dir.exists() {
                if !dry_run {
                    let team_git = GitBackend::open(&team_sync_dir)?;
                    team_git.pull()?;
                    Output::success("Team configs updated");
                }
            } else {
                Output::warning("Team sync directory not found - run 'tether team add' again");
            }
        }
    }

    let dotfiles_dir = sync_path.join("dotfiles");
    std::fs::create_dir_all(&dotfiles_dir)?;

    let mut state = SyncState::load()?;

    // Apply dotfiles from sync repo (if encrypted) - with conflict detection
    // Interactive mode when run manually, non-interactive when run by daemon
    let interactive = std::env::var("TETHER_DAEMON").is_err();
    if config.security.encrypt_dotfiles && !dry_run {
        decrypt_from_repo(&config, &sync_path, &home, &state, interactive)?;
    }

    // Sync dotfiles (local → Git)
    Output::info("Syncing dotfiles...");

    // Sync individual dotfiles
    for file in &config.dotfiles.files {
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
                    Output::info(&format!("  {} (changed)", file));

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

                        state.update_file(file, hash);
                    }
                } else {
                    Output::info(&format!("  {} (unchanged)", file));
                }
            }
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

    // Sync package manifests
    sync_packages(&config, &mut state, &sync_path, dry_run).await?;

    // Save machine state for cross-machine comparison
    if !dry_run {
        let machine_state = build_machine_state(&config, &state, &sync_path).await?;
        machine_state.save_to_repo(&sync_path)?;
    }

    // Commit and push changes
    if !dry_run {
        // Check if there are any changes (including machine state update)
        let has_changes = git.has_changes()?;

        if has_changes {
            Output::info("Committing changes...");
            git.commit("Sync dotfiles and packages", &state.machine_id)?;
            Output::success("Changes committed");

            Output::info("Pushing to remote...");
            git.push()?;
            Output::success("Changes pushed");
        } else {
            Output::info("No changes to sync");
        }

        state.mark_synced();
        state.save()?;
    } else {
        Output::info("Dry-run complete - no changes made");
    }

    // Check and push team repo changes (if write access enabled)
    if !dry_run {
        if let Some(team) = &config.team {
            if team.enabled && !team.read_only {
                let team_sync_dir = Config::team_sync_dir()?;
                if team_sync_dir.exists() {
                    let team_git = GitBackend::open(&team_sync_dir)?;

                    if team_git.has_changes()? {
                        println!();
                        Output::info("Detected changes in team repository");

                        Output::info("Committing team config changes...");
                        team_git.commit("Update team configs", &state.machine_id)?;
                        Output::success("Team changes committed");

                        Output::info("Pushing team changes...");
                        team_git.push()?;
                        Output::success("Team changes pushed");
                    }
                }
            }
        }
    }

    Output::success("Sync complete!");
    Ok(())
}

fn decrypt_from_repo(
    config: &Config,
    sync_path: &Path,
    home: &Path,
    state: &SyncState,
    interactive: bool,
) -> Result<()> {
    use crate::sync::{detect_conflict, ConflictResolution, ConflictState};

    Output::info("Applying dotfiles from sync repository...");

    let key = crate::security::get_encryption_key()?;
    let dotfiles_dir = sync_path.join("dotfiles");
    let mut conflict_state = ConflictState::load().unwrap_or_default();
    let mut new_conflicts = Vec::new();

    for file in &config.dotfiles.files {
        let filename = file.trim_start_matches('.');
        let enc_file = dotfiles_dir.join(format!("{}.enc", filename));

        if enc_file.exists() {
            let encrypted_content = std::fs::read(&enc_file)?;
            match crate::security::decrypt_file(&encrypted_content, &key) {
                Ok(plaintext) => {
                    let local_file = home.join(file);
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
                                ConflictResolution::KeepLocal => {
                                    Output::info(&format!("  {} (kept local)", file));
                                }
                                ConflictResolution::UseRemote => {
                                    std::fs::write(&local_file, &plaintext)?;
                                    Output::info(&format!("  {} (used remote)", file));
                                    conflict_state.remove_conflict(file);
                                }
                                ConflictResolution::Merged => {
                                    conflict.launch_merge_tool(&config.merge, home)?;
                                    conflict_state.remove_conflict(file);
                                }
                                ConflictResolution::Skip => {
                                    Output::info(&format!("  {} (skipped)", file));
                                    new_conflicts.push((
                                        file.clone(),
                                        conflict.local_hash.clone(),
                                        conflict.remote_hash.clone(),
                                    ));
                                }
                            }
                        } else {
                            // Non-interactive (daemon): save conflict for later
                            Output::warning(&format!("  {} (conflict - skipped)", file));
                            new_conflicts.push((
                                file.clone(),
                                conflict.local_hash.clone(),
                                conflict.remote_hash.clone(),
                            ));
                        }
                    } else {
                        // No conflict - safe to overwrite
                        std::fs::write(&local_file, plaintext)?;
                        Output::info(&format!("  {} (applied)", file));
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
        Output::info("Decrypting global configs from sync repository...");

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
                                std::fs::write(&local_file, plaintext)?;
                                Output::info(&format!("  ~/{} (decrypted)", rel_path_no_enc));
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
        decrypt_project_configs(config, sync_path, home, &key)?;
    }

    Ok(())
}

fn decrypt_project_configs(
    config: &Config,
    sync_path: &Path,
    home: &Path,
    key: &[u8],
) -> Result<()> {
    use crate::sync::git::{find_git_repos, get_remote_url, normalize_remote_url};
    use walkdir::WalkDir;

    let projects_dir = sync_path.join("projects");
    if !projects_dir.exists() {
        return Ok(());
    }

    Output::info("Decrypting project configs from sync repository...");

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

    for entry in WalkDir::new(&projects_dir)
        .follow_links(false)
        .min_depth(1)
        .max_depth(1)
    {
        let entry = match entry {
            Ok(e) => e,
            Err(_) => continue,
        };

        if !entry.file_type().is_dir() {
            continue;
        }

        let project_dir = entry.path();
        let project_name = match project_dir.file_name() {
            Some(name) => name.to_string_lossy().to_string(),
            None => continue,
        };

        if let Some(local_repo_path) = repo_map.get(&project_name) {
            for file_entry in WalkDir::new(project_dir).follow_links(false) {
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
                    let rel_path = enc_file
                        .strip_prefix(project_dir)
                        .map_err(|e| anyhow::anyhow!("Failed to strip prefix: {}", e))?;
                    let rel_path_str = rel_path.to_string_lossy();
                    let rel_path_no_enc = rel_path_str.trim_end_matches(".enc");

                    if let Ok(encrypted_content) = std::fs::read(enc_file) {
                        match crate::security::decrypt_file(&encrypted_content, key) {
                            Ok(plaintext) => {
                                let local_file = local_repo_path.join(rel_path_no_enc);
                                if let Some(parent) = local_file.parent() {
                                    std::fs::create_dir_all(parent)?;
                                }
                                std::fs::write(&local_file, plaintext)?;
                                Output::info(&format!(
                                    "  {}: {} (decrypted)",
                                    project_name, rel_path_no_enc
                                ));
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
                "  {} (no matching local repo found - skipping)",
                project_name
            ));
        }
    }

    Ok(())
}

fn scan_and_warn_secrets(config: &Config, source: &Path, file: &str) {
    if config.security.scan_secrets {
        if let Ok(findings) = crate::security::scan_for_secrets(source) {
            if !findings.is_empty() {
                Output::warning(&format!(
                    "  {} - Found {} potential secret(s)",
                    file,
                    findings.len()
                ));
                for finding in findings.iter().take(3) {
                    Output::warning(&format!(
                        "    Line {}: {}",
                        finding.line_number,
                        finding.secret_type.description()
                    ));
                }
                if findings.len() > 3 {
                    Output::warning(&format!("    ... and {} more", findings.len() - 3));
                }
                Output::info("  Secrets will be encrypted before syncing");
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

    Output::info("Syncing global config directories...");

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

                if file_changed {
                    Output::info(&format!("  {} (changed)", dir_path));

                    if !dry_run {
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
                } else {
                    Output::info(&format!("  {} (unchanged)", dir_path));
                }
            }
        } else if expanded_path.is_dir() {
            Output::info(&format!("  {} (directory)", dir_path));

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
    use crate::sync::git::{find_git_repos, get_remote_url, is_gitignored, normalize_remote_url};
    use walkdir::WalkDir;

    Output::info("Syncing project-local configs...");

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
                for entry in WalkDir::new(&repo_path).follow_links(false).max_depth(5) {
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

                        if file_changed {
                            if config.security.scan_secrets {
                                if let Ok(findings) = crate::security::scan_for_secrets(file_path) {
                                    if !findings.is_empty() {
                                        Output::warning(&format!(
                                            "  {}: {} - Found {} potential secret(s)",
                                            normalized_url,
                                            rel_to_repo.display(),
                                            findings.len()
                                        ));
                                        for finding in findings.iter().take(2) {
                                            Output::warning(&format!(
                                                "    Line {}: {}",
                                                finding.line_number,
                                                finding.secret_type.description()
                                            ));
                                        }
                                        Output::info("  Secrets will be encrypted before syncing");
                                    }
                                }
                            }

                            Output::info(&format!(
                                "  {}: {} (changed)",
                                normalized_url,
                                rel_to_repo.display()
                            ));

                            if !dry_run {
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
    }

    Ok(())
}

async fn sync_packages(
    config: &Config,
    state: &mut SyncState,
    sync_path: &Path,
    dry_run: bool,
) -> Result<()> {
    Output::info("Syncing package manifests...");
    let manifests_dir = sync_path.join("manifests");
    std::fs::create_dir_all(&manifests_dir)?;

    // Homebrew
    if config.packages.brew.enabled {
        let brew = BrewManager::new();
        if brew.is_available().await {
            Output::info("  Syncing Homebrew packages (Brewfile)...");

            match brew.export_manifest().await {
                Ok(manifest) => {
                    let hash = format!("{:x}", Sha256::digest(manifest.as_bytes()));

                    if state
                        .packages
                        .get("brew")
                        .map(|p| p.hash != hash)
                        .unwrap_or(true)
                    {
                        let lines = manifest.lines().count();
                        Output::info(&format!("    {} entries in Brewfile", lines));
                        if !dry_run {
                            std::fs::write(manifests_dir.join("Brewfile"), manifest)?;
                            use chrono::Utc;
                            state.packages.insert(
                                "brew".to_string(),
                                crate::sync::state::PackageState {
                                    last_sync: Utc::now(),
                                    hash,
                                },
                            );
                        }
                    } else {
                        Output::info("    No changes");
                    }
                }
                Err(e) => {
                    Output::warning(&format!("Failed to export Homebrew manifest: {}", e));
                }
            }
        }
    }

    // npm
    if config.packages.npm.enabled {
        sync_package_manager(
            &NpmManager::new(),
            "npm",
            "npm.txt",
            state,
            &manifests_dir,
            dry_run,
        )
        .await?;
    }

    // pnpm
    if config.packages.pnpm.enabled {
        sync_package_manager(
            &PnpmManager::new(),
            "pnpm",
            "pnpm.txt",
            state,
            &manifests_dir,
            dry_run,
        )
        .await?;
    }

    // bun
    if config.packages.bun.enabled {
        sync_package_manager(
            &BunManager::new(),
            "bun",
            "bun.txt",
            state,
            &manifests_dir,
            dry_run,
        )
        .await?;
    }

    // gem
    if config.packages.gem.enabled {
        sync_package_manager(
            &GemManager::new(),
            "gem",
            "gems.txt",
            state,
            &manifests_dir,
            dry_run,
        )
        .await?;
    }

    Ok(())
}

async fn sync_package_manager<P: PackageManager>(
    manager: &P,
    name: &str,
    filename: &str,
    state: &mut SyncState,
    manifests_dir: &Path,
    dry_run: bool,
) -> Result<()> {
    if !manager.is_available().await {
        return Ok(());
    }

    Output::info(&format!("  Syncing {} packages...", name));

    match manager.export_manifest().await {
        Ok(manifest) => {
            let hash = format!("{:x}", Sha256::digest(manifest.as_bytes()));

            if state
                .packages
                .get(name)
                .map(|p| p.hash != hash)
                .unwrap_or(true)
            {
                let count = manifest.lines().filter(|l| !l.trim().is_empty()).count();
                Output::info(&format!("    {} packages", count));
                if !dry_run {
                    std::fs::write(manifests_dir.join(filename), manifest)?;
                    use chrono::Utc;
                    state.packages.insert(
                        name.to_string(),
                        crate::sync::state::PackageState {
                            last_sync: Utc::now(),
                            hash,
                        },
                    );
                }
            } else {
                Output::info("    No changes");
            }
        }
        Err(e) => {
            Output::warning(&format!("Failed to export {} manifest: {}", name, e));
        }
    }

    Ok(())
}

/// Build machine state for cross-machine comparison
async fn build_machine_state(
    _config: &Config,
    state: &SyncState,
    _sync_path: &Path,
) -> Result<MachineState> {
    let mut machine_state = MachineState::new(&state.machine_id);

    // Collect file hashes (packages are in manifest files, no need to duplicate)
    for (path, file_state) in &state.files {
        machine_state
            .files
            .insert(path.clone(), file_state.hash.clone());
    }

    Ok(machine_state)
}

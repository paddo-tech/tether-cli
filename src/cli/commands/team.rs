use crate::cli::{Output, Progress, Prompt};
use crate::config::{Config, TeamConfig};
use crate::sync::GitBackend;
use anyhow::Result;
use comfy_table::{Attribute, Cell, Color};

/// Validate team name contains only safe characters for filesystem paths
fn is_valid_team_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
        && !name.starts_with('-')
        && !name.starts_with('_')
}

/// Sanitize a team name by replacing unsafe characters
fn sanitize_team_name(name: &str) -> String {
    name.chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
        .take(64)
        .collect::<String>()
        .trim_start_matches(['-', '_'])
        .to_string()
}

/// Validate that a URL belongs to an allowed organization
fn validate_org_restriction(url: &str, allowed_orgs: &[String]) -> Result<()> {
    if allowed_orgs.is_empty() {
        return Ok(()); // No restrictions configured
    }

    let org = crate::sync::extract_team_name_from_url(url)
        .ok_or_else(|| anyhow::anyhow!("Could not extract organization from URL: {}", url))?;

    if !allowed_orgs
        .iter()
        .any(|allowed| allowed.eq_ignore_ascii_case(&org))
    {
        anyhow::bail!(
            "Team repository must belong to an allowed organization.\n\
             Allowed orgs: {}\n\
             Found org: {}",
            allowed_orgs.join(", "),
            org
        );
    }

    Ok(())
}

pub async fn add(url: &str, name: Option<&str>, _no_auto_inject: bool) -> Result<()> {
    let mut config = Config::load()?;

    // Check org restriction before cloning
    if let Some(teams) = &config.teams {
        validate_org_restriction(url, &teams.allowed_orgs)?;
    }

    // Determine team name (custom or auto-extracted)
    let raw_name = name.map(|s| s.to_string()).unwrap_or_else(|| {
        crate::sync::extract_team_name_from_url(url).unwrap_or_else(|| "team".to_string())
    });

    // Validate and sanitize team name
    let team_name = if is_valid_team_name(&raw_name) {
        raw_name
    } else {
        let sanitized = sanitize_team_name(&raw_name);
        if sanitized.is_empty() {
            anyhow::bail!("Invalid team name: must contain alphanumeric characters");
        }
        Output::warning(&format!(
            "Team name '{}' sanitized to '{}'",
            raw_name, sanitized
        ));
        sanitized
    };

    // Initialize teams config if needed
    if config.teams.is_none() {
        config.teams = Some(crate::config::TeamsConfig::default());
    }

    let teams = config.teams.as_mut().unwrap();

    // Check if team already exists
    if teams.teams.contains_key(&team_name) {
        Output::warning(&format!("Team '{}' already exists", team_name));
        if !Prompt::confirm("Replace existing team configuration?", false)? {
            return Ok(());
        }
    }

    Output::info(&format!("Adding team: {} ({})", team_name, url));

    // Clone team repository to team-specific directory
    let team_repo_dir = Config::team_repo_dir(&team_name)?;
    if team_repo_dir.exists() {
        std::fs::remove_dir_all(&team_repo_dir)?;
    }

    // Ensure parent directory exists
    if let Some(parent) = team_repo_dir.parent() {
        std::fs::create_dir_all(parent)?;
    }

    let pb = Progress::spinner("Cloning team repository...");
    GitBackend::clone(url, &team_repo_dir)?;
    Progress::finish_success(&pb, "Team repository cloned");

    // Security check: Scan for secrets in team repo
    Output::info("Scanning team configs for secrets...");
    let dotfiles_dir = team_repo_dir.join("dotfiles");
    let mut team_files = Vec::new();
    let mut secrets_found = false;

    if dotfiles_dir.exists() {
        for entry in std::fs::read_dir(&dotfiles_dir)? {
            let entry = entry?;
            if entry.file_type()?.is_file() {
                if let Some(filename) = entry.file_name().to_str() {
                    team_files.push(filename.to_string());

                    // Scan for secrets
                    let file_path = entry.path();
                    if let Ok(findings) = crate::security::scan_for_secrets(&file_path) {
                        if !findings.is_empty() {
                            secrets_found = true;
                            Output::warning(&format!(
                                "  {} - Found {} potential secret(s)",
                                filename,
                                findings.len()
                            ));
                            for finding in findings.iter().take(2) {
                                Output::warning(&format!(
                                    "    Line {}: {}",
                                    finding.line_number,
                                    finding.secret_type.description()
                                ));
                            }
                        }
                    }
                }
            }
        }
    }

    // Warn if secrets found
    if secrets_found {
        println!();
        Output::warning("Potential secrets detected in team repository!");
        Output::dim("  Team repositories should only contain non-sensitive shared configs.");
        Output::dim("  For sensitive data, use a secrets manager (1Password, Vault, etc.)");
        println!();

        if !Prompt::confirm("Continue anyway?", false)? {
            std::fs::remove_dir_all(&team_repo_dir)?;
            return Ok(());
        }
    }

    if !team_files.is_empty() {
        println!();
        Output::info("Found team configs:");
        for file in &team_files {
            println!("  • {}", file);
        }
        println!();
    }

    // Detect write access to team repository
    Output::info("Checking repository permissions...");
    let team_git = GitBackend::open(&team_repo_dir)?;
    let has_write = team_git.has_write_access().unwrap_or(false);

    let read_only = if has_write {
        println!();
        Output::success("You have write access to this repository!");
        Output::info("As a team admin/contributor, you can push updates to team configs.");
        println!();

        !Prompt::confirm(
            "Enable write access? (No = read-only mode for regular team members)",
            true,
        )?
    } else {
        println!();
        Output::info("Read-only access detected (regular team member mode)");
        println!();
        true
    };

    if read_only {
        Output::info("Team sync configured in read-only mode");
    } else {
        Output::info("Team sync configured with write access - you can push updates");
    }

    // Set up layer-based sync for dotfiles
    let use_layers = if !team_files.is_empty() {
        println!();
        Output::info("Team dotfiles will be merged with your personal configs.");
        Output::dim("  Personal settings always override team defaults.");
        println!();

        // Show preview of team config contents
        Output::info("Team dotfile contents:");
        for file in &team_files {
            let team_file_path = team_repo_dir.join("dotfiles").join(file);
            if let Ok(content) = std::fs::read_to_string(&team_file_path) {
                println!();
                println!("  {}:", file);
                println!("  {}", "─".repeat(50));
                // Show first 20 lines or all if shorter
                let lines: Vec<&str> = content.lines().take(20).collect();
                for line in &lines {
                    println!("  {}", line);
                }
                if content.lines().count() > 20 {
                    println!("  ... ({} more lines)", content.lines().count() - 20);
                }
                println!("  {}", "─".repeat(50));
            }
        }
        println!();
        Prompt::confirm("Merge team dotfiles with your personal configs?", true)?
    } else {
        false
    };

    // Perform layer-based merge if confirmed
    if use_layers && !team_files.is_empty() {
        apply_layer_sync(&team_name, &team_repo_dir, &team_files).await?;
    }

    // Discover and create symlinks for config directories
    println!();
    Output::info("Setting up symlinks for team configs...");
    let symlinkable_dirs = crate::sync::discover_symlinkable_dirs(&team_repo_dir)?;

    if symlinkable_dirs.is_empty() {
        Output::info("No symlinkable directories found (e.g., .claude, .config)");
    } else {
        let mut manifest = crate::sync::TeamManifest::load()?;

        for dir in &symlinkable_dirs {
            Output::info(&format!(
                "Symlinking items from {} to {}",
                dir.team_path.display(),
                dir.target_base.display()
            ));

            let results = dir.create_symlinks(&team_name, &mut manifest, false)?;

            for result in results {
                match result {
                    crate::sync::team::SymlinkResult::Created(target) => {
                        Output::success(&format!("  ✓ {}", target.display()));
                    }
                    crate::sync::team::SymlinkResult::Conflict(target) => {
                        let team_source = dir.team_path.join(target.file_name().unwrap());
                        let resolution = crate::sync::resolve_conflict(&target, &team_source)?;
                        manifest.add_conflict(&team_name, target.clone(), resolution);
                        Output::success(&format!("  ✓ {} (conflict resolved)", target.display()));
                    }
                    crate::sync::team::SymlinkResult::Skipped(target) => {
                        Output::info(&format!("  ⊘ {} (skipped)", target.display()));
                    }
                }
            }
        }

        manifest.save()?;
        Output::success("Symlinks created successfully");
    }

    // Add team to config
    let should_set_active = {
        let teams = config.teams.as_mut().unwrap();
        teams.teams.insert(
            team_name.clone(),
            TeamConfig {
                enabled: true,
                url: url.to_string(),
                auto_inject: use_layers, // Now means "use layer-based merge"
                read_only,
            },
        );

        // Set as active team if it's the first or user confirms
        if teams.active.is_none()
            || Prompt::confirm(&format!("Set '{}' as active team?", team_name), true)?
        {
            teams.active = Some(team_name.clone());
            true
        } else {
            false
        }
    };

    config.save()?;

    println!();
    Output::success(&format!("Team '{}' added successfully!", team_name));
    if should_set_active {
        Output::info("This team is now active");
    }
    Ok(())
}

pub async fn switch(name: &str) -> Result<()> {
    let mut config = Config::load()?;

    let teams = match config.teams.as_mut() {
        Some(t) => t,
        None => {
            Output::error("No teams configured. Run 'tether team add' first.");
            return Ok(());
        }
    };

    if !teams.teams.contains_key(name) {
        Output::error(&format!("Team '{}' not found", name));
        Output::info("Available teams:");
        for team_name in teams.teams.keys() {
            println!("  • {}", team_name);
        }
        return Ok(());
    }

    if teams.active.as_ref() == Some(&name.to_string()) {
        Output::info(&format!("Team '{}' is already active", name));
        return Ok(());
    }

    Output::info(&format!("Switching to team '{}'...", name));

    // Deactivate current team (remove symlinks)
    if let Some(current) = &teams.active {
        Output::info(&format!("Deactivating team '{}'...", current));
        let mut manifest = crate::sync::TeamManifest::load()?;
        manifest.cleanup_team(Some(current))?;
        Output::success("Current team deactivated");
    }

    // Activate new team (create symlinks)
    Output::info(&format!("Activating team '{}'...", name));
    let team_repo_dir = Config::team_repo_dir(name)?;

    if !team_repo_dir.exists() {
        Output::error(&format!(
            "Team repository not found at {}",
            team_repo_dir.display()
        ));
        Output::info("The team may need to be re-added.");
        return Ok(());
    }

    let symlinkable_dirs = crate::sync::discover_symlinkable_dirs(&team_repo_dir)?;
    if !symlinkable_dirs.is_empty() {
        let mut manifest = crate::sync::TeamManifest::load()?;
        for dir in &symlinkable_dirs {
            let results = dir.create_symlinks(name, &mut manifest, false)?;
            for result in results {
                if let crate::sync::team::SymlinkResult::Created(target) = result {
                    Output::success(&format!("  ✓ {}", target.display()));
                }
            }
        }
        manifest.save()?;
    }

    teams.active = Some(name.to_string());
    config.save()?;

    println!();
    Output::success(&format!("Switched to team '{}'", name));
    Ok(())
}

pub async fn list() -> Result<()> {
    let config = Config::load()?;

    let teams = match &config.teams {
        Some(t) => t,
        None => {
            Output::info("No teams configured. Run 'tether team add' to add a team.");
            return Ok(());
        }
    };

    if teams.teams.is_empty() {
        Output::info("No teams configured. Run 'tether team add' to add a team.");
        return Ok(());
    }

    println!();
    println!("Teams:");
    for (name, team) in &teams.teams {
        let active_marker = if teams.active.as_ref() == Some(name) {
            " (active)"
        } else {
            ""
        };
        let status = if team.enabled { "enabled" } else { "disabled" };
        let access = if team.read_only {
            "read-only"
        } else {
            "read-write"
        };

        println!("  • {}{}", name, active_marker);
        println!("    URL: {}", team.url);
        println!("    Status: {}, Access: {}", status, access);
    }
    println!();
    Ok(())
}

pub async fn remove(_name: Option<&str>) -> Result<()> {
    let mut config = Config::load()?;

    if config.team.is_none() {
        Output::warning("Team sync is not configured");
        return Ok(());
    }

    if !Prompt::confirm("Remove team sync configuration?", false)? {
        return Ok(());
    }

    // Clean up symlinks first
    Output::info("Removing symlinks...");
    let mut manifest = crate::sync::TeamManifest::load()?;
    manifest.cleanup()?;
    Output::success("Symlinks removed");

    // Remove team sync directory
    let team_sync_dir = Config::team_sync_dir()?;
    if team_sync_dir.exists() {
        std::fs::remove_dir_all(&team_sync_dir)?;
    }

    config.team = None;
    config.save()?;

    Output::success("Team sync removed");
    Output::info("Note: Source lines in your dotfiles were not removed automatically");
    Ok(())
}

pub async fn enable() -> Result<()> {
    let mut config = Config::load()?;

    match config.team.as_mut() {
        Some(team) => {
            if team.enabled {
                Output::info("Team sync is already enabled");
            } else {
                team.enabled = true;
                config.save()?;
                Output::success("Team sync enabled");
            }
        }
        None => {
            Output::error("Team sync is not configured. Run 'tether team add' first.");
        }
    }
    Ok(())
}

pub async fn disable() -> Result<()> {
    let mut config = Config::load()?;

    match config.team.as_mut() {
        Some(team) => {
            if !team.enabled {
                Output::info("Team sync is already disabled");
            } else {
                team.enabled = false;
                config.save()?;
                Output::success("Team sync disabled");
            }
        }
        None => {
            Output::error("Team sync is not configured");
        }
    }
    Ok(())
}

pub async fn status() -> Result<()> {
    let config = Config::load()?;

    println!();
    match &config.team {
        Some(team) => {
            let mut table = Output::table_full();
            table
                .set_header(vec![
                    Cell::new("Team Sync")
                        .add_attribute(Attribute::Bold)
                        .fg(Color::Cyan),
                    Cell::new(""),
                ])
                .add_row(vec![
                    Cell::new("Status"),
                    if team.enabled {
                        Cell::new(format!("{} Enabled", Output::DOT)).fg(Color::Green)
                    } else {
                        Cell::new(format!("{} Disabled", Output::DOT)).fg(Color::Yellow)
                    },
                ])
                .add_row(vec![Cell::new("Repository"), Cell::new(&team.url)])
                .add_row(vec![
                    Cell::new("Access Mode"),
                    if team.read_only {
                        Cell::new("Read-only").fg(Color::Yellow)
                    } else {
                        Cell::new("Read-write (Admin)").fg(Color::Green)
                    },
                ])
                .add_row(vec![
                    Cell::new("Auto-inject"),
                    Cell::new(if team.auto_inject { "Yes" } else { "No" }),
                ]);

            // Show team files
            let team_sync_dir = Config::team_sync_dir()?;
            let dotfiles_dir = team_sync_dir.join("dotfiles");

            if dotfiles_dir.exists() {
                let mut count = 0;
                for entry in std::fs::read_dir(&dotfiles_dir)? {
                    if entry?.file_type()?.is_file() {
                        count += 1;
                    }
                }
                table.add_row(vec![
                    Cell::new("Team files"),
                    Cell::new(format!("{} files", count)),
                ]);
            }

            println!("{table}");
        }
        None => {
            Output::info("Team sync is not configured");
            Output::info("Run 'tether team add <url>' to add team sync");
        }
    }
    println!();
    Ok(())
}

/// Apply layer-based sync for team dotfiles
/// 1. Copy team dotfiles to team layer
/// 2. Capture personal dotfiles to personal layer (first time)
/// 3. Merge and apply to home directory
async fn apply_layer_sync(
    team_name: &str,
    team_repo_dir: &std::path::Path,
    team_files: &[String],
) -> Result<()> {
    use crate::sync::layers::map_team_to_personal_name;
    use crate::sync::{init_layers, sync_dotfile_with_layers, sync_team_to_layer, FileType};

    Output::info("Setting up layer-based dotfile sync...");

    // Initialize layer directories
    init_layers(team_name)?;

    // Copy team dotfiles to team layer (renames team.* to .*)
    let dotfiles_dir = team_repo_dir.join("dotfiles");
    sync_team_to_layer(team_name, &dotfiles_dir)?;

    // Process each team dotfile
    for file in team_files {
        // Map team.* files to personal dotfile names
        let personal_name = map_team_to_personal_name(file);

        match sync_dotfile_with_layers(team_name, &personal_name) {
            Ok(crate::sync::LayerSyncResult::Merged { file_type }) => {
                let merge_type = match file_type {
                    FileType::Toml => "TOML merge",
                    FileType::Json => "JSON merge",
                    FileType::Ini => "INI merge",
                    FileType::Plain => "concatenated",
                };
                Output::success(&format!("  {} → {} ({})", file, personal_name, merge_type));
            }
            Ok(crate::sync::LayerSyncResult::TeamOnly) => {
                Output::success(&format!("  {} → {} (team only)", file, personal_name));
            }
            Ok(crate::sync::LayerSyncResult::Skipped) => {
                Output::dim(&format!("  {} skipped", file));
            }
            Err(e) => {
                Output::warning(&format!("  {} failed: {}", file, e));
            }
        }
    }

    Output::success("Layer-based sync configured");
    Ok(())
}

#[allow(dead_code)]
async fn inject_team_sources(team_files: &[String]) -> Result<()> {
    let home = home::home_dir().ok_or_else(|| anyhow::anyhow!("Could not find home directory"))?;
    let team_sync_dir = Config::team_sync_dir()?;

    for file in team_files {
        let (personal_file, source_line) = if file == "team.zshrc" {
            (
                home.join(".zshrc"),
                format!(
                    "[ -f {}/dotfiles/team.zshrc ] && source {}/dotfiles/team.zshrc",
                    team_sync_dir.display(),
                    team_sync_dir.display()
                ),
            )
        } else if file == "team.gitconfig" {
            let include_line = format!(
                "[include]\n    path = {}/dotfiles/team.gitconfig",
                team_sync_dir.display()
            );
            (home.join(".gitconfig"), include_line)
        } else if file.starts_with("team.") && (file.ends_with("rc") || file.ends_with("profile")) {
            (
                home.join(file.replace("team.", ".")),
                format!(
                    "[ -f {}/dotfiles/{} ] && source {}/dotfiles/{}",
                    team_sync_dir.display(),
                    file,
                    team_sync_dir.display(),
                    file
                ),
            )
        } else {
            continue;
        };

        if !personal_file.exists() {
            Output::warning(&format!(
                "  {} not found, skipping",
                personal_file.display()
            ));
            continue;
        }

        let content = std::fs::read_to_string(&personal_file)?;

        if content.contains(&source_line)
            || content.contains(&format!(
                "source {}/dotfiles/{}",
                team_sync_dir.display(),
                file
            ))
        {
            Output::info(&format!(
                "  {} already sources team config",
                file.replace("team.", ".")
            ));
            continue;
        }

        let new_content = if file == "team.gitconfig" {
            format!("{}\n\n{}", source_line, content)
        } else if content.starts_with("#!") {
            let mut lines: Vec<&str> = content.lines().collect();
            lines.insert(1, "");
            lines.insert(2, &source_line);
            lines.join("\n")
        } else {
            format!("{}\n\n{}", source_line, content)
        };

        std::fs::write(&personal_file, new_content)?;
        Output::success(&format!(
            "  Added source line to {}",
            file.replace("team.", ".")
        ));
    }

    Ok(())
}

#[allow(dead_code)]
fn show_injection_instructions(team_files: &[String]) {
    let team_sync_dir = Config::team_sync_dir().unwrap();

    for file in team_files {
        if file == "team.zshrc" {
            println!("  Add to ~/.zshrc:");
            println!(
                "    [ -f {}/dotfiles/team.zshrc ] && source {}/dotfiles/team.zshrc",
                team_sync_dir.display(),
                team_sync_dir.display()
            );
        } else if file == "team.gitconfig" {
            println!("  Add to ~/.gitconfig:");
            println!("    [include]");
            println!(
                "        path = {}/dotfiles/team.gitconfig",
                team_sync_dir.display()
            );
        }
    }
    println!();
}

// --- Org restriction management ---

pub async fn orgs_add(org: &str) -> Result<()> {
    let mut config = Config::load()?;

    // Initialize teams config if needed
    if config.teams.is_none() {
        config.teams = Some(crate::config::TeamsConfig::default());
    }

    let teams = config.teams.as_mut().unwrap();

    // Check if already exists (case-insensitive)
    if teams
        .allowed_orgs
        .iter()
        .any(|o| o.eq_ignore_ascii_case(org))
    {
        Output::info(&format!("Organization '{}' is already allowed", org));
        return Ok(());
    }

    teams.allowed_orgs.push(org.to_string());
    config.save()?;

    Output::success(&format!("Added '{}' to allowed organizations", org));
    Ok(())
}

pub async fn orgs_list() -> Result<()> {
    let config = Config::load()?;

    let allowed_orgs = config
        .teams
        .as_ref()
        .map(|t| &t.allowed_orgs)
        .filter(|o| !o.is_empty());

    match allowed_orgs {
        Some(orgs) => {
            println!();
            println!("Allowed organizations:");
            for org in orgs {
                println!("  • {}", org);
            }
            println!();
        }
        None => {
            Output::info("No organization restrictions configured");
            Output::dim("  Any team repository URL is allowed");
        }
    }
    Ok(())
}

pub async fn orgs_remove(org: &str) -> Result<()> {
    let mut config = Config::load()?;

    let teams = match config.teams.as_mut() {
        Some(t) => t,
        None => {
            Output::info("No organization restrictions configured");
            return Ok(());
        }
    };

    let original_len = teams.allowed_orgs.len();
    teams
        .allowed_orgs
        .retain(|o| !o.eq_ignore_ascii_case(org));

    if teams.allowed_orgs.len() == original_len {
        Output::warning(&format!("Organization '{}' not found in allowed list", org));
        return Ok(());
    }

    config.save()?;
    Output::success(&format!("Removed '{}' from allowed organizations", org));
    Ok(())
}

// --- Team secrets management ---

/// Get active team's repo directory or error
fn get_active_team_repo() -> Result<(String, std::path::PathBuf)> {
    let config = Config::load()?;
    let teams = config
        .teams
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("No teams configured. Run 'tether team add' first."))?;
    let active = teams
        .active
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("No active team. Run 'tether team switch <name>' first."))?;
    let repo_dir = Config::team_repo_dir(active)?;
    if !repo_dir.exists() {
        anyhow::bail!("Team repository not found. Re-add the team.");
    }
    Ok((active.clone(), repo_dir))
}

pub async fn secrets_add_recipient(key: &str, name: Option<&str>) -> Result<()> {
    let (team_name, repo_dir) = get_active_team_repo()?;
    let recipients_dir = repo_dir.join("recipients");
    std::fs::create_dir_all(&recipients_dir)?;

    // Validate key format
    let pubkey = if std::path::Path::new(key).exists() {
        std::fs::read_to_string(key)?
    } else {
        key.to_string()
    };
    crate::security::validate_pubkey(&pubkey)?;

    // Determine recipient name
    let recipient_name = name
        .map(|s| s.to_string())
        .unwrap_or_else(|| std::env::var("USER").unwrap_or_else(|_| "unknown".to_string()));

    let pubkey_file = recipients_dir.join(format!("{}.pub", recipient_name));
    std::fs::write(&pubkey_file, pubkey.trim())?;

    // Commit to team repo
    let git = GitBackend::open(&repo_dir)?;
    git.commit(&format!("Add recipient: {}", recipient_name), "tether")?;

    Output::success(&format!(
        "Added recipient '{}' to team '{}'",
        recipient_name, team_name
    ));
    Output::info("Run 'tether sync' to push changes to team repo");
    Ok(())
}

pub async fn secrets_list_recipients() -> Result<()> {
    let (team_name, repo_dir) = get_active_team_repo()?;
    let recipients_dir = repo_dir.join("recipients");

    if !recipients_dir.exists() {
        Output::info(&format!("No recipients configured for team '{}'", team_name));
        return Ok(());
    }

    println!();
    println!("Recipients for team '{}':", team_name);

    for entry in std::fs::read_dir(&recipients_dir)? {
        let entry = entry?;
        if entry.path().extension().is_some_and(|e| e == "pub") {
            if let Some(name) = entry.path().file_stem().and_then(|s| s.to_str()) {
                let pubkey = std::fs::read_to_string(entry.path())?;
                let short_key = if pubkey.len() > 20 {
                    format!("{}...", &pubkey.trim()[..20])
                } else {
                    pubkey.trim().to_string()
                };
                println!("  • {} ({})", name, short_key);
            }
        }
    }
    println!();
    Ok(())
}

pub async fn secrets_remove_recipient(name: &str) -> Result<()> {
    let (team_name, repo_dir) = get_active_team_repo()?;
    let pubkey_file = repo_dir.join("recipients").join(format!("{}.pub", name));

    if !pubkey_file.exists() {
        Output::error(&format!("Recipient '{}' not found", name));
        return Ok(());
    }

    std::fs::remove_file(&pubkey_file)?;

    // Commit to team repo
    let git = GitBackend::open(&repo_dir)?;
    git.commit(&format!("Remove recipient: {}", name), "tether")?;

    Output::success(&format!(
        "Removed recipient '{}' from team '{}'",
        name, team_name
    ));
    Output::warning("Existing secrets should be re-encrypted without this recipient");
    Ok(())
}

pub async fn secrets_set(name: &str, value: Option<&str>) -> Result<()> {
    let (team_name, repo_dir) = get_active_team_repo()?;
    let secrets_dir = repo_dir.join("secrets");
    std::fs::create_dir_all(&secrets_dir)?;

    // Get secret value
    let secret_value = match value {
        Some(v) => v.to_string(),
        None => Prompt::password(&format!("Enter value for '{}':", name))?,
    };

    // Load recipients
    let recipients_dir = repo_dir.join("recipients");
    let recipients = crate::security::load_recipients(&recipients_dir)?;
    if recipients.is_empty() {
        Output::error("No recipients configured. Add recipients first.");
        Output::info("Run: tether team secrets add-recipient <pubkey>");
        return Ok(());
    }

    // Encrypt to all recipients
    let encrypted = crate::security::encrypt_to_recipients(secret_value.as_bytes(), &recipients)?;
    let secret_file = secrets_dir.join(format!("{}.age", name));
    std::fs::write(&secret_file, &encrypted)?;

    // Commit to team repo
    let git = GitBackend::open(&repo_dir)?;
    git.commit(&format!("Set secret: {}", name), "tether")?;

    Output::success(&format!("Secret '{}' set for team '{}'", name, team_name));
    Output::info(&format!("Encrypted to {} recipient(s)", recipients.len()));
    Ok(())
}

pub async fn secrets_get(name: &str) -> Result<()> {
    let (_team_name, repo_dir) = get_active_team_repo()?;
    let secret_file = repo_dir.join("secrets").join(format!("{}.age", name));

    if !secret_file.exists() {
        Output::error(&format!("Secret '{}' not found", name));
        return Ok(());
    }

    // Load user's identity
    let identity = crate::security::load_identity(None).map_err(|_| {
        anyhow::anyhow!("Identity not unlocked. Run 'tether identity unlock' first.")
    })?;

    // Decrypt
    let encrypted = std::fs::read(&secret_file)?;
    let decrypted = crate::security::decrypt_with_identity(&encrypted, &identity)?;
    let value = String::from_utf8(decrypted)?;

    println!("{}", value);
    Ok(())
}

pub async fn secrets_list() -> Result<()> {
    let (team_name, repo_dir) = get_active_team_repo()?;
    let secrets_dir = repo_dir.join("secrets");

    if !secrets_dir.exists() {
        Output::info(&format!("No secrets configured for team '{}'", team_name));
        return Ok(());
    }

    println!();
    println!("Secrets for team '{}':", team_name);

    for entry in std::fs::read_dir(&secrets_dir)? {
        let entry = entry?;
        if entry.path().extension().is_some_and(|e| e == "age") {
            if let Some(name) = entry.path().file_stem().and_then(|s| s.to_str()) {
                println!("  • {}", name);
            }
        }
    }
    println!();
    Ok(())
}

pub async fn secrets_remove(name: &str) -> Result<()> {
    let (team_name, repo_dir) = get_active_team_repo()?;
    let secret_file = repo_dir.join("secrets").join(format!("{}.age", name));

    if !secret_file.exists() {
        Output::error(&format!("Secret '{}' not found", name));
        return Ok(());
    }

    std::fs::remove_file(&secret_file)?;

    // Commit to team repo
    let git = GitBackend::open(&repo_dir)?;
    git.commit(&format!("Remove secret: {}", name), "tether")?;

    Output::success(&format!(
        "Removed secret '{}' from team '{}'",
        name, team_name
    ));
    Ok(())
}

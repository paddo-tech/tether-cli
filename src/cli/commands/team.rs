use crate::cli::{Output, Prompt};
use crate::config::{Config, TeamConfig};
use crate::sync::GitBackend;
use anyhow::Result;
use comfy_table::{presets::UTF8_FULL, Attribute, Cell, Color, ContentArrangement, Table};

pub async fn add(url: &str, name: Option<&str>, no_auto_inject: bool) -> Result<()> {
    let mut config = Config::load()?;

    // Determine team name (custom or auto-extracted)
    let team_name = name.map(|s| s.to_string()).unwrap_or_else(|| {
        crate::sync::extract_team_name_from_url(url).unwrap_or_else(|| "team".to_string())
    });

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

    Output::info("Cloning team repository...");
    GitBackend::clone(url, &team_repo_dir)?;
    Output::success("Team repository cloned successfully");

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
        Output::warning("⚠️  Potential secrets detected in team repository!");
        Output::warning("Team repositories should only contain non-sensitive shared configs.");
        Output::info("For sensitive data, use a secrets manager (1Password, Vault, etc.)");
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

    // Ask about auto-injection
    let auto_inject = if no_auto_inject {
        false
    } else if !team_files.is_empty() {
        Prompt::confirm("Auto-inject source lines to your personal configs?", true)?
    } else {
        false
    };

    // Perform auto-injection if requested
    if auto_inject {
        inject_team_sources(&team_files).await?;
    } else if !team_files.is_empty() {
        println!();
        Output::info("To use team configs, add these lines to your dotfiles:");
        show_injection_instructions(&team_files);
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
                auto_inject,
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
            let mut table = Table::new();
            table
                .load_preset(UTF8_FULL)
                .set_content_arrangement(ContentArrangement::Dynamic)
                .set_header(vec![
                    Cell::new("Team Sync")
                        .add_attribute(Attribute::Bold)
                        .fg(Color::Cyan),
                    Cell::new(""),
                ])
                .add_row(vec![
                    Cell::new("Status"),
                    if team.enabled {
                        Cell::new("● Enabled").fg(Color::Green)
                    } else {
                        Cell::new("● Disabled").fg(Color::Yellow)
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

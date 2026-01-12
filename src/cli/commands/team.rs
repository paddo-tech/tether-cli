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

/// Prompt for team repository - offers to create or use existing
async fn prompt_for_team_repo() -> Result<String> {
    use crate::github::GitHubCli;

    // Check if gh CLI is available and authenticated
    let gh_available =
        GitHubCli::is_installed() && GitHubCli::is_authenticated().await.unwrap_or(false);

    if gh_available {
        let options = vec!["Create new private GitHub repo", "Use existing repo URL"];
        let choice = Prompt::select("Team repository:", options, 0)?;

        if choice == 0 {
            // Create new repo - fetch orgs and username
            let spinner = Progress::spinner("Fetching GitHub organizations...");
            let orgs = GitHubCli::list_orgs().await.unwrap_or_default();
            let username = GitHubCli::get_username().await?;
            Progress::finish_success(&spinner, "Done");

            // Build location options: personal account + orgs
            let mut locations: Vec<String> = vec![format!("{} (personal)", username)];
            for org in &orgs {
                locations.push(org.clone());
            }
            let location_refs: Vec<&str> = locations.iter().map(|s| s.as_str()).collect();

            let loc_choice = Prompt::select("Where to create the repo?", location_refs, 0)?;
            let owner = if loc_choice == 0 {
                username.clone()
            } else {
                orgs[loc_choice - 1].clone()
            };

            // Prompt for repo name
            let default_name = "team-dotfiles";
            let repo_name = Prompt::input("Repository name:", Some(default_name))?;

            // Create the repo
            let spinner = Progress::spinner(&format!("Creating {}/{}...", owner, repo_name));
            let url = if loc_choice == 0 {
                GitHubCli::create_repo(&repo_name, true).await?
            } else {
                GitHubCli::create_org_repo(&owner, &repo_name, true).await?
            };
            Progress::finish_success(
                &spinner,
                &format!("Created {}/{} (private)", owner, repo_name),
            );

            Ok(url)
        } else {
            // Use existing URL
            Output::dim("Enter the Git URL of your team's shared config repository");
            Output::dim("Example: git@github.com:your-org/team-dotfiles.git");
            println!();
            Prompt::input("Team repository URL:", None)
        }
    } else {
        // No gh CLI - just ask for URL
        if !GitHubCli::is_installed() {
            Output::dim("Tip: Install 'gh' CLI to create repos directly from this wizard");
        }
        Output::dim("Enter the Git URL of your team's shared config repository");
        Output::dim("Example: git@github.com:your-org/team-dotfiles.git");
        println!();
        Prompt::input("Team repository URL:", None)
    }
}

/// Interactive team setup wizard
pub async fn setup() -> Result<()> {
    use crate::sync::git::{find_git_repos, get_remote_url, normalize_remote_url};

    println!();
    println!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");
    Output::info("Team Setup Wizard");
    println!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");
    println!();
    println!("This wizard will help you:");
    println!("  1. Connect to a team repository");
    println!("  2. Set up your encryption identity");
    println!("  3. Map your GitHub/GitLab orgs for project secrets");
    println!("  4. Configure team secret recipients");
    println!();

    let mut config = Config::load()?;
    let home = home::home_dir().ok_or_else(|| anyhow::anyhow!("Could not find home directory"))?;

    // Step 1: Check for existing teams or add new one
    let team_name = if let Some(teams) = &config.teams {
        if !teams.teams.is_empty() {
            let team_names: Vec<&str> = teams.teams.keys().map(|s| s.as_str()).collect();
            let mut options: Vec<&str> = team_names.clone();
            options.push("Add new team");

            println!("You have {} team(s) configured.", teams.teams.len());
            let choice = Prompt::select("What would you like to do?", options.clone(), 0)?;

            if choice == options.len() - 1 {
                // Add new team
                println!();
                let url = prompt_for_team_repo().await?;
                add(&url, None, false).await?;
                crate::sync::extract_team_name_from_url(&url).unwrap_or_else(|| "team".to_string())
            } else {
                team_names[choice].to_string()
            }
        } else {
            Output::info("Step 1: Connect to team repository");
            let url = prompt_for_team_repo().await?;
            add(&url, None, false).await?;
            crate::sync::extract_team_name_from_url(&url).unwrap_or_else(|| "team".to_string())
        }
    } else {
        Output::info("Step 1: Connect to team repository");
        let url = prompt_for_team_repo().await?;
        add(&url, None, false).await?;
        crate::sync::extract_team_name_from_url(&url).unwrap_or_else(|| "team".to_string())
    };

    // Reload config after potential add
    config = Config::load()?;

    println!();
    Output::info(&format!("Configuring team '{}'", team_name));

    // Step 2: Identity setup
    println!();
    Output::info("Step 2: Encryption identity");
    let identity_path = Config::config_dir()?.join("identity.age");
    if !identity_path.exists() {
        Output::dim("An identity is required to encrypt/decrypt team secrets");
        if Prompt::confirm("Create identity now?", true)? {
            crate::cli::commands::identity::init().await?;
        }
    } else {
        Output::success("Identity already configured");
    }

    // Step 3: Org mapping for project secrets
    println!();
    Output::info("Project secrets: Map GitHub/GitLab orgs to this team");
    Output::dim("Projects from mapped orgs will use team secrets instead of personal sync");
    println!();

    // Determine search paths
    let default_paths: Vec<String> = config
        .project_configs
        .search_paths
        .iter()
        .map(|p| {
            if let Some(stripped) = p.strip_prefix("~/") {
                home.join(stripped).to_string_lossy().to_string()
            } else {
                p.clone()
            }
        })
        .collect();

    println!("Default search paths: {}", default_paths.join(", "));
    let custom_path = if Prompt::confirm("Scan a different directory?", false)? {
        Some(Prompt::input("Directory to scan:", None)?)
    } else {
        None
    };

    // Scan for git repos
    let mut discovered_orgs: std::collections::HashSet<String> = std::collections::HashSet::new();
    let search_paths: Vec<std::path::PathBuf> = if let Some(ref p) = custom_path {
        let path = if let Some(stripped) = p.strip_prefix("~/") {
            home.join(stripped)
        } else {
            std::path::PathBuf::from(p)
        };
        vec![path]
    } else {
        config
            .project_configs
            .search_paths
            .iter()
            .map(|p| {
                if let Some(stripped) = p.strip_prefix("~/") {
                    home.join(stripped)
                } else {
                    std::path::PathBuf::from(p)
                }
            })
            .collect()
    };

    let spinner = Progress::spinner("Scanning for git repos...");
    for search_path in &search_paths {
        if let Ok(repos) = find_git_repos(search_path) {
            for repo_path in repos {
                if let Ok(remote_url) = get_remote_url(&repo_path) {
                    let normalized = normalize_remote_url(&remote_url);
                    if let Some(org) = crate::sync::extract_org_from_normalized_url(&normalized) {
                        discovered_orgs.insert(org);
                    }
                }
            }
        }
    }
    Progress::finish_success(
        &spinner,
        &format!("Found {} unique org(s)", discovered_orgs.len()),
    );

    let teams = config.teams.as_ref().unwrap();
    let team_config = teams.teams.get(&team_name).unwrap();
    let existing_orgs: std::collections::HashSet<String> =
        team_config.orgs.iter().cloned().collect();

    // Filter out already-mapped orgs
    let suggested_orgs: Vec<String> = discovered_orgs
        .difference(&existing_orgs)
        .cloned()
        .collect();

    if !existing_orgs.is_empty() {
        println!("Currently mapped orgs:");
        for org in &existing_orgs {
            println!("  • {}", org);
        }
        println!();
    }

    if !suggested_orgs.is_empty() {
        println!("Discovered orgs from your projects:");
        for (i, org) in suggested_orgs.iter().enumerate() {
            println!("  {}. {}", i + 1, org);
        }
        println!();

        if Prompt::confirm("Add any of these orgs to the team?", true)? {
            let selections = Prompt::input(
                "Enter numbers to add (comma-separated, or 'all'):",
                Some("all"),
            )?;

            let orgs_to_add: Vec<&String> = if selections.trim().eq_ignore_ascii_case("all") {
                suggested_orgs.iter().collect()
            } else {
                selections
                    .split(',')
                    .filter_map(|s| s.trim().parse::<usize>().ok())
                    .filter_map(|i| suggested_orgs.get(i.saturating_sub(1)))
                    .collect()
            };

            for org in orgs_to_add {
                orgs_add(org).await?;
            }
        }
    } else if existing_orgs.is_empty() {
        Output::dim("No local projects found to suggest orgs");
        if Prompt::confirm("Add an org manually?", false)? {
            let org = Prompt::input("Org (e.g., github.com/acme-corp):", None)?;
            orgs_add(&org).await?;
        }
    }

    // Step 4: Recipient setup (if admin)
    println!();
    let teams = config.teams.as_ref().unwrap();
    let team_config = teams.teams.get(&team_name).unwrap();

    if !team_config.read_only {
        Output::info("Team secrets: Add recipients who can decrypt team secrets");

        let repo_dir = Config::team_repo_dir(&team_name)?;
        let recipients_dir = repo_dir.join("recipients");
        let existing_recipients = if recipients_dir.exists() {
            std::fs::read_dir(&recipients_dir)?
                .filter_map(|e| e.ok())
                .filter(|e| {
                    e.path()
                        .extension()
                        .map(|ext| ext == "pub")
                        .unwrap_or(false)
                })
                .count()
        } else {
            0
        };

        println!("Current recipients: {}", existing_recipients);

        // Check if user's identity is a recipient
        let identity_pub_path = Config::config_dir()?.join("identity.pub");
        if identity_pub_path.exists() {
            let my_pubkey = std::fs::read_to_string(&identity_pub_path)?;
            let my_pubkey = my_pubkey.trim();

            let am_recipient = if recipients_dir.exists() {
                std::fs::read_dir(&recipients_dir)?
                    .filter_map(|e| e.ok())
                    .any(|e| {
                        std::fs::read_to_string(e.path())
                            .map(|content| content.trim() == my_pubkey)
                            .unwrap_or(false)
                    })
            } else {
                false
            };

            if !am_recipient {
                println!();
                Output::warning(
                    "You are not a recipient - you won't be able to decrypt team secrets",
                );
                if Prompt::confirm("Add yourself as a recipient?", true)? {
                    let username = std::env::var("USER").unwrap_or_else(|_| "user".to_string());
                    let name = Prompt::input("Your name for this recipient:", Some(&username))?;
                    secrets_add_recipient(&identity_pub_path.to_string_lossy(), Some(&name))
                        .await?;
                }
            } else {
                Output::success("You are a recipient");
            }
        }
    } else {
        Output::dim("Team is read-only - recipient management disabled");
    }

    // Step 5: Summary
    println!();
    println!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");
    Output::success("Team setup complete!");
    println!();

    // Reload and show status
    status().await?;

    println!();
    Output::info("Next steps:");
    println!("  • Run 'tether sync' to sync team dotfiles");
    println!("  • Use 'tether team projects add .env' to share project secrets");
    println!("  • Use 'tether team secrets set KEY' to share team-wide secrets");

    Ok(())
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
                orgs: Vec::new(), // Configure via 'tether team orgs add'
            },
        );

        // Add to active teams if first or user confirms
        if teams.active.is_empty()
            || Prompt::confirm(&format!("Activate team '{}'?", team_name), true)?
        {
            if !teams.active.contains(&team_name) {
                teams.active.push(team_name.clone());
            }
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

/// Toggle a team's active status (activate if inactive, deactivate if active)
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

    let is_active = teams.active.contains(&name.to_string());

    if is_active {
        // Deactivate this team
        Output::info(&format!("Deactivating team '{}'...", name));

        // Remove symlinks for this team
        let mut manifest = crate::sync::TeamManifest::load()?;
        manifest.cleanup_team(Some(name))?;

        // Remove injections for this team
        cleanup_team_injections(name)?;

        // Remove from active list
        teams.active.retain(|n| n != name);
        let active_teams = teams.active.clone();
        config.save()?;

        Output::success(&format!("Team '{}' deactivated", name));

        // Show current active teams
        if !active_teams.is_empty() {
            Output::info(&format!("Active teams: {}", active_teams.join(", ")));
        }
    } else {
        // Activate this team
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

        // Create symlinks
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

        // Apply layer sync for dotfiles
        let dotfiles_dir = team_repo_dir.join("dotfiles");
        if dotfiles_dir.exists() {
            let mut team_files = Vec::new();
            for entry in std::fs::read_dir(&dotfiles_dir)? {
                let entry = entry?;
                if entry.file_type()?.is_file() {
                    if let Some(filename) = entry.file_name().to_str() {
                        team_files.push(filename.to_string());
                    }
                }
            }
            if !team_files.is_empty() {
                apply_layer_sync(name, &team_repo_dir, &team_files).await?;
            }
        }

        // Add to active list
        teams.active.push(name.to_string());
        let active_teams = teams.active.clone();
        config.save()?;

        Output::success(&format!("Team '{}' activated", name));

        // Show current active teams
        Output::info(&format!("Active teams: {}", active_teams.join(", ")));
    }

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
        let active_marker = if teams.active.contains(name) {
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

pub async fn remove(name: Option<&str>) -> Result<()> {
    let mut config = Config::load()?;

    let teams = match config.teams.as_mut() {
        Some(t) if !t.teams.is_empty() => t,
        _ => {
            Output::warning("No teams configured");
            return Ok(());
        }
    };

    // Determine which team to remove
    let team_name = match name {
        Some(n) => n.to_string(),
        None => {
            // Use first active team or prompt if multiple
            if !teams.active.is_empty() {
                teams.active[0].clone()
            } else if teams.teams.len() == 1 {
                teams.teams.keys().next().unwrap().clone()
            } else {
                Output::error("Multiple teams configured. Specify which to remove:");
                for name in teams.teams.keys() {
                    println!("  • {}", name);
                }
                return Ok(());
            }
        }
    };

    if !teams.teams.contains_key(&team_name) {
        Output::error(&format!("Team '{}' not found", team_name));
        return Ok(());
    }

    if !Prompt::confirm(&format!("Remove team '{}'?", team_name), false)? {
        return Ok(());
    }

    Output::info(&format!("Removing team '{}'...", team_name));

    // Clean up injected source/include lines
    Output::info("Cleaning up dotfile injections...");
    cleanup_team_injections(&team_name)?;

    // Clean up symlinks
    Output::info("Removing symlinks...");
    let mut manifest = crate::sync::TeamManifest::load()?;
    manifest.cleanup_team(Some(&team_name))?;
    Output::success("Symlinks removed");

    // Remove team repo directory
    let team_repo_dir = Config::team_repo_dir(&team_name)?;
    if team_repo_dir.exists() {
        std::fs::remove_dir_all(&team_repo_dir)?;
    }

    // Remove from config
    teams.teams.remove(&team_name);
    teams.active.retain(|n| n != &team_name);
    config.save()?;

    Output::success(&format!("Team '{}' removed", team_name));
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
    use crate::sync::{
        detect_file_type, init_layers, sync_dotfile_with_layers, sync_team_to_layer, FileType,
    };

    let home = home::home_dir().ok_or_else(|| anyhow::anyhow!("Could not find home directory"))?;
    let dotfiles_dir = team_repo_dir.join("dotfiles");

    Output::info("Setting up team dotfile sync...");

    // Check if any files need layer-based merge (TOML/JSON)
    let needs_layers = team_files.iter().any(|f| {
        let personal_name = map_team_to_personal_name(f, team_name);
        matches!(
            detect_file_type(std::path::Path::new(&personal_name)),
            FileType::Toml | FileType::Json
        )
    });

    // Initialize layers once if needed
    if needs_layers {
        init_layers(team_name)?;
        sync_team_to_layer(team_name, &dotfiles_dir)?;
    }

    for file in team_files {
        let personal_name = map_team_to_personal_name(file, team_name);
        let team_file_path = dotfiles_dir.join(file);
        let personal_file = home.join(&personal_name);
        let file_type = detect_file_type(std::path::Path::new(&personal_name));

        match file_type {
            FileType::Shell => {
                // Only inject if personal file exists
                if !personal_file.exists() {
                    Output::warning(&format!(
                        "  {} skipped ({} doesn't exist)",
                        file, personal_name
                    ));
                    continue;
                }

                let source_line = format!(
                    "[ -f \"{}\" ] && source \"{}\"",
                    team_file_path.display(),
                    team_file_path.display()
                );

                if inject_source_line(&personal_file, &source_line)? {
                    Output::success(&format!("  {} → {} (source injected)", file, personal_name));
                } else {
                    Output::dim(&format!("  {} → {} (already sourced)", file, personal_name));
                }
            }
            FileType::GitConfig => {
                // Only inject if personal file exists
                if !personal_file.exists() {
                    Output::warning(&format!(
                        "  {} skipped ({} doesn't exist)",
                        file, personal_name
                    ));
                    continue;
                }

                if inject_gitconfig_include(&personal_file, &team_file_path)? {
                    Output::success(&format!("  {} → {} (include added)", file, personal_name));
                } else {
                    Output::dim(&format!(
                        "  {} → {} (already included)",
                        file, personal_name
                    ));
                }
            }
            FileType::Toml | FileType::Json => {
                match sync_dotfile_with_layers(team_name, &personal_name) {
                    Ok(crate::sync::LayerSyncResult::Merged { file_type }) => {
                        let merge_type = match file_type {
                            FileType::Toml => "TOML merged",
                            FileType::Json => "JSON merged",
                            _ => "merged",
                        };
                        Output::success(&format!(
                            "  {} → {} ({})",
                            file, personal_name, merge_type
                        ));
                    }
                    Ok(crate::sync::LayerSyncResult::TeamOnly) => {
                        Output::success(&format!("  {} → {} (team only)", file, personal_name));
                    }
                    Ok(crate::sync::LayerSyncResult::Skipped) => {
                        Output::dim(&format!("  {} skipped", file));
                    }
                    Err(e) => {
                        Output::warning(&format!("  {} merge failed: {}", file, e));
                    }
                }
            }
            FileType::Unknown => {
                Output::warning(&format!("  {} skipped (unknown file type)", file));
            }
        }
    }

    Output::success("Team dotfile sync configured");
    Ok(())
}

/// Inject a source line into a shell config file (at the top, after any shebang/comments)
/// Caller must verify file exists before calling.
fn inject_source_line(file: &std::path::Path, source_line: &str) -> Result<bool> {
    let content = std::fs::read_to_string(file)?;

    // Check if already sourced
    if content.contains(source_line) {
        return Ok(false);
    }

    // Find insertion point (after shebang and initial comments)
    let mut lines: Vec<&str> = content.lines().collect();
    let mut insert_idx = 0;

    for (i, line) in lines.iter().enumerate() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            insert_idx = i + 1;
        } else {
            break;
        }
    }

    // Insert with a blank line after for readability
    lines.insert(insert_idx, source_line);
    if insert_idx + 1 < lines.len() && !lines[insert_idx + 1].is_empty() {
        lines.insert(insert_idx + 1, "");
    }

    let new_content = lines.join("\n");
    std::fs::write(file, new_content)?;
    Ok(true)
}

/// Inject an [include] directive into gitconfig (at the top)
/// Caller must verify file exists before calling.
fn inject_gitconfig_include(file: &std::path::Path, team_file: &std::path::Path) -> Result<bool> {
    let content = std::fs::read_to_string(file)?;
    let team_path_str = team_file.display().to_string();

    // Check if already included
    if content.contains(&team_path_str) {
        return Ok(false);
    }

    let include_block = format!("[include]\n\tpath = {}\n\n", team_path_str);
    let new_content = format!("{}{}", include_block, content);

    std::fs::write(file, new_content)?;
    Ok(true)
}

/// Remove source lines that reference a team repo path from shell config files
fn remove_source_lines(file: &std::path::Path, team_repo_path: &std::path::Path) -> Result<bool> {
    if !file.exists() {
        return Ok(false);
    }

    let content = std::fs::read_to_string(file)?;
    let team_path_str = team_repo_path.display().to_string();

    // Check if file contains any reference to team repo
    if !content.contains(&team_path_str) {
        return Ok(false);
    }

    // Remove lines containing the team repo path
    let new_lines: Vec<&str> = content
        .lines()
        .filter(|line| !line.contains(&team_path_str))
        .collect();

    // Clean up any resulting double blank lines
    let mut cleaned = Vec::new();
    let mut prev_blank = false;
    for line in new_lines {
        let is_blank = line.trim().is_empty();
        if is_blank && prev_blank {
            continue;
        }
        cleaned.push(line);
        prev_blank = is_blank;
    }

    let new_content = cleaned.join("\n");
    std::fs::write(file, new_content)?;
    Ok(true)
}

/// Remove [include] blocks that reference a team repo path from gitconfig
fn remove_gitconfig_include(
    file: &std::path::Path,
    team_repo_path: &std::path::Path,
) -> Result<bool> {
    if !file.exists() {
        return Ok(false);
    }

    let content = std::fs::read_to_string(file)?;
    let team_path_str = team_repo_path.display().to_string();

    // Check if file contains any reference to team repo
    if !content.contains(&team_path_str) {
        return Ok(false);
    }

    // Parse and filter out [include] sections that reference team repo
    let mut new_lines: Vec<&str> = Vec::new();
    let mut skip_until_next_section = false;
    let mut in_include_section = false;

    for line in content.lines() {
        let trimmed = line.trim();

        // Check for section header
        if trimmed.starts_with('[') {
            in_include_section = trimmed.to_lowercase().starts_with("[include]");
            skip_until_next_section = false;
        }

        // If in [include] section and line contains team path, skip this include block
        if in_include_section && line.contains(&team_path_str) {
            // Remove the [include] header we just added and skip until next section
            if let Some(last) = new_lines.last() {
                if last.trim().to_lowercase() == "[include]" {
                    new_lines.pop();
                }
            }
            skip_until_next_section = true;
            continue;
        }

        if skip_until_next_section {
            if trimmed.starts_with('[') {
                skip_until_next_section = false;
            } else {
                continue;
            }
        }

        new_lines.push(line);
    }

    // Clean up leading/trailing blank lines and double blank lines
    let mut cleaned: Vec<&str> = Vec::new();
    let mut prev_blank = true; // Start true to skip leading blanks
    for line in new_lines {
        let is_blank = line.trim().is_empty();
        if is_blank && prev_blank {
            continue;
        }
        cleaned.push(line);
        prev_blank = is_blank;
    }
    // Remove trailing blank line
    while cleaned
        .last()
        .map(|s: &&str| s.trim().is_empty())
        .unwrap_or(false)
    {
        cleaned.pop();
    }

    let new_content = cleaned.join("\n");
    std::fs::write(file, new_content)?;
    Ok(true)
}

/// Clean up all injected source/include lines for a team
fn cleanup_team_injections(team_name: &str) -> Result<()> {
    let home = home::home_dir().ok_or_else(|| anyhow::anyhow!("Could not find home directory"))?;
    let team_repo_dir = Config::team_repo_dir(team_name)?;

    // Shell files to check
    let shell_files = [
        ".zshrc",
        ".bashrc",
        ".bash_profile",
        ".profile",
        ".zprofile",
        ".zshenv",
    ];
    for shell_file in &shell_files {
        let path = home.join(shell_file);
        if remove_source_lines(&path, &team_repo_dir)? {
            Output::success(&format!("  Removed source line from {}", shell_file));
        }
    }

    // Gitconfig
    let gitconfig = home.join(".gitconfig");
    if remove_gitconfig_include(&gitconfig, &team_repo_dir)? {
        Output::success("  Removed include from .gitconfig");
    }

    // Clean up merged files
    let merged_dir = crate::sync::layers::merged_dir()?;
    if merged_dir.exists() {
        std::fs::remove_dir_all(&merged_dir)?;
        Output::success("  Removed merged dotfiles");
    }

    // Clean up team layer
    crate::sync::layers::cleanup_team_layers(team_name)?;

    Ok(())
}

// --- Per-team org management ---
// Maps GitHub/GitLab orgs to teams for project secrets

pub async fn orgs_add(org: &str) -> Result<()> {
    use crate::sync::SyncEngine;

    let mut config = Config::load()?;

    let teams = match config.teams.as_mut() {
        Some(t) => t,
        None => {
            Output::error("No teams configured. Run 'tether team add' first.");
            return Ok(());
        }
    };

    let active = teams
        .active
        .first()
        .ok_or_else(|| anyhow::anyhow!("No active team"))?
        .clone();

    let team_config = teams
        .teams
        .get_mut(&active)
        .ok_or_else(|| anyhow::anyhow!("Team not found"))?;

    // Validate format (should be host/org like github.com/acme-corp)
    if !org.contains('/') {
        Output::error("Org format should be 'host/org' (e.g., github.com/acme-corp)");
        return Ok(());
    }

    // Check if already exists
    if team_config.orgs.iter().any(|o| o.eq_ignore_ascii_case(org)) {
        Output::info(&format!("'{}' already mapped to team '{}'", org, active));
        return Ok(());
    }

    team_config.orgs.push(org.to_string());
    config.save()?;

    Output::success(&format!("Mapped '{}' to team '{}'", org, active));
    Output::info("Projects from this org will now use team secrets");

    // Check for personal project files that should be cleaned up
    let sync_path = SyncEngine::sync_path()?;
    let personal_projects_dir = sync_path.join("projects");

    if personal_projects_dir.exists() {
        let matching = find_personal_projects_for_org(&personal_projects_dir, org)?;
        if !matching.is_empty() {
            println!();
            Output::warning(&format!(
                "Found {} project(s) in personal sync that now belong to team:",
                matching.len()
            ));
            for project in &matching {
                println!("  • {}", project);
            }
            println!();

            if Prompt::confirm("Remove these from personal sync?", true)? {
                purge_personal_project_files(&sync_path, &matching)?;
                Output::success("Removed from personal sync");

                if Prompt::confirm("Also purge from git history? (rewrites history)", false)? {
                    purge_from_git_history(&sync_path, &matching)?;
                    force_push_sync_repo(&sync_path)?;
                    Output::success("Purged from git history and pushed");
                }
            }
        }
    }

    Ok(())
}

pub async fn orgs_list() -> Result<()> {
    let config = Config::load()?;

    let teams = match config.teams.as_ref() {
        Some(t) => t,
        None => {
            Output::info("No teams configured");
            return Ok(());
        }
    };

    println!();
    for (name, team_config) in &teams.teams {
        let active = teams.active.contains(name);
        let marker = if active { " (active)" } else { "" };
        println!("Team '{}'{}:", name, marker);

        if team_config.orgs.is_empty() {
            Output::dim("  No orgs mapped - project secrets disabled");
        } else {
            for org in &team_config.orgs {
                println!("  • {}", org);
            }
        }
        println!();
    }

    Ok(())
}

pub async fn orgs_remove(org: &str) -> Result<()> {
    let mut config = Config::load()?;

    let teams = match config.teams.as_mut() {
        Some(t) => t,
        None => {
            Output::error("No teams configured");
            return Ok(());
        }
    };

    let active = teams
        .active
        .first()
        .ok_or_else(|| anyhow::anyhow!("No active team"))?
        .clone();

    let team_config = teams
        .teams
        .get_mut(&active)
        .ok_or_else(|| anyhow::anyhow!("Team not found"))?;

    let original_len = team_config.orgs.len();
    team_config.orgs.retain(|o| !o.eq_ignore_ascii_case(org));

    if team_config.orgs.len() == original_len {
        Output::warning(&format!("'{}' not mapped to team '{}'", org, active));
        return Ok(());
    }

    config.save()?;
    Output::success(&format!("Removed '{}' from team '{}'", org, active));
    Ok(())
}

/// Find personal project directories matching an org
fn find_personal_projects_for_org(
    personal_projects_dir: &std::path::Path,
    org: &str,
) -> Result<Vec<String>> {
    use walkdir::WalkDir;

    let mut matching = Vec::new();

    // Org format is "github.com/acme-corp"
    // Projects are stored as "projects/github.com/acme-corp/repo-name/..."
    let parts: Vec<&str> = org.split('/').collect();
    if parts.len() != 2 {
        return Ok(matching);
    }

    let (host, org_name) = (parts[0], parts[1]);
    let org_path = personal_projects_dir.join(host).join(org_name);

    if !org_path.exists() {
        return Ok(matching);
    }

    // Find all repo directories under this org
    for entry in WalkDir::new(&org_path).min_depth(1).max_depth(1) {
        let entry = match entry {
            Ok(e) => e,
            Err(_) => continue,
        };

        if entry.file_type().is_dir() {
            let repo_name = entry.file_name().to_string_lossy();
            matching.push(format!("{}/{}/{}", host, org_name, repo_name));
        }
    }

    Ok(matching)
}

/// Remove project files from personal sync
fn purge_personal_project_files(sync_path: &std::path::Path, projects: &[String]) -> Result<()> {
    let projects_dir = sync_path.join("projects");

    for project in projects {
        let project_path = projects_dir.join(project);
        if project_path.exists() {
            std::fs::remove_dir_all(&project_path)?;
        }
    }

    // Clean up empty parent directories
    for project in projects {
        let parts: Vec<&str> = project.split('/').collect();
        if parts.len() >= 2 {
            let org_path = projects_dir.join(parts[0]).join(parts[1]);
            if org_path.exists() && org_path.read_dir()?.next().is_none() {
                std::fs::remove_dir_all(&org_path)?;
            }
            let host_path = projects_dir.join(parts[0]);
            if host_path.exists() && host_path.read_dir()?.next().is_none() {
                std::fs::remove_dir_all(&host_path)?;
            }
        }
    }

    // Commit the removal
    let git = GitBackend::open(sync_path)?;
    if git.has_changes()? {
        git.commit("Remove project secrets moved to team sync", "tether")?;
    }

    Ok(())
}

/// Purge project files from git history using git filter-repo
fn purge_from_git_history(sync_path: &std::path::Path, projects: &[String]) -> Result<()> {
    use std::process::Command;

    // Check if git-filter-repo is available
    let filter_repo_check = Command::new("git")
        .args(["filter-repo", "--version"])
        .output();

    if filter_repo_check.is_err() || !filter_repo_check.unwrap().status.success() {
        Output::warning("git-filter-repo not found, using git filter-branch instead");
        Output::dim("  Install git-filter-repo for safer history rewriting");
        return purge_with_filter_branch(sync_path, projects);
    }

    // Build paths to purge
    let mut args = vec!["filter-repo".to_string(), "--force".to_string()];
    for project in projects {
        args.push("--path".to_string());
        args.push(format!("projects/{}", project));
        args.push("--invert-paths".to_string());
    }

    let output = Command::new("git")
        .current_dir(sync_path)
        .args(&args)
        .output()?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("git filter-repo failed: {}", stderr);
    }

    Ok(())
}

/// Force push the sync repo after history rewrite
fn force_push_sync_repo(sync_path: &std::path::Path) -> Result<()> {
    use std::process::Command;

    let output = Command::new("git")
        .current_dir(sync_path)
        .args(["push", "--force-with-lease"])
        .output()?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        // Don't fail if remote doesn't exist or other non-critical errors
        if !stderr.contains("No configured push destination") {
            Output::warning(&format!("Push failed: {}", stderr.trim()));
        }
    }

    Ok(())
}

/// Fallback to git filter-branch if git-filter-repo isn't available
fn purge_with_filter_branch(sync_path: &std::path::Path, projects: &[String]) -> Result<()> {
    use std::process::Command;

    // Build the filter command
    let mut rm_commands = Vec::new();
    for project in projects {
        rm_commands.push(format!(
            "git rm -rf --cached --ignore-unmatch projects/{}",
            project
        ));
    }
    let filter_cmd = rm_commands.join(" && ");

    let output = Command::new("git")
        .current_dir(sync_path)
        .args([
            "filter-branch",
            "--force",
            "--index-filter",
            &filter_cmd,
            "--prune-empty",
            "HEAD",
        ])
        .output()?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("git filter-branch failed: {}", stderr);
    }

    // Clean up refs
    let _ = Command::new("git")
        .current_dir(sync_path)
        .args(["for-each-ref", "--format=%(refname)", "refs/original/"])
        .output()
        .map(|o| {
            if o.status.success() {
                let refs = String::from_utf8_lossy(&o.stdout);
                for r in refs.lines() {
                    let _ = Command::new("git")
                        .current_dir(sync_path)
                        .args(["update-ref", "-d", r])
                        .output();
                }
            }
        });

    let _ = Command::new("git")
        .current_dir(sync_path)
        .args(["reflog", "expire", "--expire=now", "--all"])
        .output();

    let _ = Command::new("git")
        .current_dir(sync_path)
        .args(["gc", "--prune=now"])
        .output();

    Ok(())
}

// --- Team secrets management ---

/// Get first active team's repo directory or error
fn get_active_team_repo() -> Result<(String, std::path::PathBuf)> {
    let config = Config::load()?;
    let teams = config
        .teams
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("No teams configured. Run 'tether team add' first."))?;
    let active = teams
        .active
        .first()
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
        Output::info(&format!(
            "No recipients configured for team '{}'",
            team_name
        ));
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

// ============================================================================
// Files subcommands
// ============================================================================

/// List synced team files and their status
pub async fn files_list() -> Result<()> {
    let (team_name, _repo_dir) = get_active_team_repo()?;
    let manifest = crate::sync::TeamManifest::load()?;

    // List team layer files
    let team_files = crate::sync::layers::list_team_layer_files(&team_name)?;

    if team_files.is_empty() {
        Output::info("No team files synced");
        return Ok(());
    }

    let mut table = comfy_table::Table::new();
    table.set_header(vec![
        Cell::new("File").add_attribute(Attribute::Bold),
        Cell::new("Status").add_attribute(Attribute::Bold),
    ]);

    let personal_files = manifest.get_personal_files(&team_name);
    let local_patterns = manifest.get_local_patterns(&team_name);

    for file in &team_files {
        let status = if personal_files.contains(file) {
            Cell::new("personal (ignored)").fg(Color::Yellow)
        } else if crate::sync::is_local_file(file, &local_patterns) {
            Cell::new("local pattern").fg(Color::Cyan)
        } else {
            Cell::new("synced").fg(Color::Green)
        };

        table.add_row(vec![Cell::new(file), status]);
    }

    println!("{}", table);
    Ok(())
}

/// Show local patterns (files that are never synced)
pub async fn files_local_patterns() -> Result<()> {
    let (team_name, _repo_dir) = get_active_team_repo()?;
    let manifest = crate::sync::TeamManifest::load()?;
    let patterns = manifest.get_local_patterns(&team_name);

    Output::info(&format!("Local patterns for team '{}':", team_name));
    println!();
    for pattern in &patterns {
        println!("  {}", pattern);
    }
    println!();
    Output::info("Files matching these patterns are never synced from team");
    Ok(())
}

/// Reset a file to the team version (clobber local changes)
pub async fn files_reset(file: Option<&str>, all: bool) -> Result<()> {
    let (team_name, _repo_dir) = get_active_team_repo()?;

    if all {
        if !Prompt::confirm(
            "Reset ALL files to team versions? This will overwrite local changes.",
            false,
        )? {
            return Ok(());
        }

        let reset_files = crate::sync::layers::reset_all_to_team(&team_name)?;
        Output::success(&format!(
            "Reset {} files to team versions",
            reset_files.len()
        ));
        for file in &reset_files {
            println!("  {}", file);
        }
    } else if let Some(filename) = file {
        if !Prompt::confirm(&format!("Reset '{}' to team version?", filename), false)? {
            return Ok(());
        }

        crate::sync::layers::reset_to_team(&team_name, filename)?;
        Output::success(&format!("Reset '{}' to team version", filename));
    } else {
        Output::error("Specify a file or use --all");
    }

    Ok(())
}

/// Promote a local file to the team repository
pub async fn files_promote(file: &str) -> Result<()> {
    let config = Config::load()?;
    let (team_name, team_config) = config
        .active_team()
        .ok_or_else(|| anyhow::anyhow!("No active team"))?;

    if team_config.read_only {
        Output::error("Cannot promote files: team is configured as read-only");
        Output::info("Ask a team admin to grant you write access");
        return Ok(());
    }

    let repo_dir = Config::team_repo_dir(&team_name)?;

    // Promote the file
    crate::sync::layers::promote_to_team(&team_name, file, &repo_dir)?;

    // Commit and push
    let git = GitBackend::open(&repo_dir)?;
    git.commit(&format!("Promote file: {}", file), "tether")?;

    let pb = Progress::spinner("Pushing to team repository...");
    match git.push() {
        Ok(_) => {
            Progress::finish_success(&pb, "Pushed to team repository");
            Output::success(&format!("Promoted '{}' to team '{}'", file, team_name));
        }
        Err(e) => {
            Progress::finish_error(&pb, "Push failed");
            Output::error(&format!("Failed to push: {}", e));
            Output::info("You may not have write access to the team repository");
        }
    }

    Ok(())
}

/// Mark a file as personal (skip team sync)
pub async fn files_ignore(file: &str) -> Result<()> {
    let (team_name, _repo_dir) = get_active_team_repo()?;
    let mut manifest = crate::sync::TeamManifest::load()?;

    manifest.add_personal_file(&team_name, file);
    manifest.save()?;

    Output::success(&format!(
        "Marked '{}' as personal - will skip team sync",
        file
    ));
    Ok(())
}

/// Remove personal file marker (resume team sync)
pub async fn files_unignore(file: &str) -> Result<()> {
    let (team_name, _repo_dir) = get_active_team_repo()?;
    let mut manifest = crate::sync::TeamManifest::load()?;

    manifest.remove_personal_file(&team_name, file);
    manifest.save()?;

    Output::success(&format!("Unmarked '{}' - will resume team sync", file));
    Ok(())
}

/// Show diff between local and team version of a file
pub async fn files_diff(file: Option<&str>) -> Result<()> {
    use similar::{ChangeTag, TextDiff};

    let (team_name, _repo_dir) = get_active_team_repo()?;
    let home = home::home_dir().ok_or_else(|| anyhow::anyhow!("Could not find home directory"))?;

    let files_to_diff = if let Some(f) = file {
        vec![f.to_string()]
    } else {
        crate::sync::layers::list_team_layer_files(&team_name)?
    };

    if files_to_diff.is_empty() {
        Output::info("No team files to diff");
        return Ok(());
    }

    for filename in &files_to_diff {
        let team_file = crate::sync::layers::team_layer_dir(&team_name)?.join(filename);
        let home_file = home.join(filename);

        if !team_file.exists() {
            continue;
        }

        if !home_file.exists() {
            println!("--- {}: only in team (not in home)", filename);
            continue;
        }

        let team_content = std::fs::read_to_string(&team_file)?;
        let home_content = std::fs::read_to_string(&home_file)?;

        if team_content == home_content {
            println!("--- {}: identical", filename);
            continue;
        }

        println!("--- {} (team vs local)", filename);
        let diff = TextDiff::from_lines(&team_content, &home_content);

        for change in diff.iter_all_changes() {
            let (sign, color) = match change.tag() {
                ChangeTag::Delete => ("-", "\x1b[31m"), // red
                ChangeTag::Insert => ("+", "\x1b[32m"), // green
                ChangeTag::Equal => (" ", ""),
            };
            if change.tag() != ChangeTag::Equal {
                print!("{}{}{}\x1b[0m", color, sign, change);
            }
        }
        println!();
    }

    Ok(())
}

// ============================================================================
// Projects subcommands
// ============================================================================

/// Add a project secret to the team repo
pub async fn projects_add(file: &str, project_path: Option<&str>) -> Result<()> {
    use crate::sync::git::{get_remote_url, normalize_remote_url};

    let (team_name, repo_dir) = get_active_team_repo()?;
    let config = Config::load()?;

    // Check that team has write access
    let teams = config
        .teams
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("No teams configured"))?;
    let team_config = teams
        .teams
        .get(&team_name)
        .ok_or_else(|| anyhow::anyhow!("Team not found"))?;

    if team_config.read_only {
        Output::error("This team is read-only. Only admins can add project secrets.");
        return Ok(());
    }

    // Determine project path
    let project_dir = match project_path {
        Some(p) => std::path::PathBuf::from(p),
        None => std::env::current_dir()?,
    };

    // Get remote URL and normalize
    let remote_url = get_remote_url(&project_dir)?;
    let normalized_url = normalize_remote_url(&remote_url);

    // Check if project belongs to this team's orgs
    let project_org = crate::sync::extract_org_from_normalized_url(&normalized_url);
    let belongs_to_team = project_org
        .as_ref()
        .map(|org| team_config.orgs.iter().any(|t| t.eq_ignore_ascii_case(org)))
        .unwrap_or(false);

    if !belongs_to_team {
        Output::error(&format!(
            "Project '{}' doesn't belong to team '{}' orgs",
            normalized_url, team_name
        ));
        Output::info(&format!("Team orgs: {:?}", team_config.orgs));
        return Ok(());
    }

    // Read the file
    let file_path = project_dir.join(file);
    if !file_path.exists() {
        Output::error(&format!("File not found: {}", file_path.display()));
        return Ok(());
    }

    let content = std::fs::read(&file_path)?;

    // Load recipients and encrypt
    let recipients_dir = repo_dir.join("recipients");
    let recipients = crate::security::load_recipients(&recipients_dir)?;
    if recipients.is_empty() {
        Output::error("No recipients configured. Add recipients first.");
        Output::info("Run: tether team secrets add-recipient <pubkey>");
        return Ok(());
    }

    let encrypted = crate::security::encrypt_to_recipients(&content, &recipients)?;

    // Write to team repo: projects/{normalized_url}/{file}.age
    let dest_dir = repo_dir.join("projects").join(&normalized_url);
    std::fs::create_dir_all(&dest_dir)?;
    let dest_file = dest_dir.join(format!("{}.age", file));
    std::fs::write(&dest_file, &encrypted)?;

    // Commit
    let git = GitBackend::open(&repo_dir)?;
    git.commit(
        &format!("Add project secret: {}/{}", normalized_url, file),
        "tether",
    )?;

    Output::success(&format!(
        "Added '{}' to team '{}' for project '{}'",
        file, team_name, normalized_url
    ));
    Output::info(&format!("Encrypted to {} recipient(s)", recipients.len()));
    Output::info("Run 'tether sync' to push to team repo");
    Ok(())
}

/// List team project secrets
pub async fn projects_list() -> Result<()> {
    use walkdir::WalkDir;

    let (team_name, repo_dir) = get_active_team_repo()?;
    let projects_dir = repo_dir.join("projects");

    if !projects_dir.exists() {
        Output::info(&format!(
            "No project secrets configured for team '{}'",
            team_name
        ));
        return Ok(());
    }

    println!();
    println!("Project secrets for team '{}':", team_name);

    let mut current_project = String::new();

    for entry in WalkDir::new(&projects_dir)
        .follow_links(false)
        .min_depth(4)
        .sort_by_file_name()
    {
        let entry = match entry {
            Ok(e) => e,
            Err(_) => continue,
        };

        if !entry.file_type().is_file() {
            continue;
        }

        let file_path = entry.path();
        if !file_path.to_string_lossy().ends_with(".age") {
            continue;
        }

        if let Ok(rel_path) = file_path.strip_prefix(&projects_dir) {
            let components: Vec<_> = rel_path.components().collect();
            if components.len() >= 4 {
                let project = format!(
                    "{}/{}/{}",
                    components[0].as_os_str().to_string_lossy(),
                    components[1].as_os_str().to_string_lossy(),
                    components[2].as_os_str().to_string_lossy()
                );

                if project != current_project {
                    println!();
                    println!("  {}:", project);
                    current_project = project;
                }

                let file_name = file_path
                    .file_name()
                    .unwrap()
                    .to_string_lossy()
                    .trim_end_matches(".age")
                    .to_string();
                println!("    • {}", file_name);
            }
        }
    }
    println!();

    Ok(())
}

/// Remove a project secret from the team repo
pub async fn projects_remove(file: &str, project: Option<&str>) -> Result<()> {
    use crate::sync::git::{get_remote_url, normalize_remote_url};

    let (team_name, repo_dir) = get_active_team_repo()?;
    let config = Config::load()?;

    // Check that team has write access
    let teams = config
        .teams
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("No teams configured"))?;
    let team_config = teams
        .teams
        .get(&team_name)
        .ok_or_else(|| anyhow::anyhow!("Team not found"))?;

    if team_config.read_only {
        Output::error("This team is read-only. Only admins can remove project secrets.");
        return Ok(());
    }

    // Determine project URL
    let normalized_url = if let Some(p) = project {
        p.to_string()
    } else {
        let project_dir = std::env::current_dir()?;
        let remote_url = get_remote_url(&project_dir)?;
        normalize_remote_url(&remote_url)
    };

    let secret_file = repo_dir
        .join("projects")
        .join(&normalized_url)
        .join(format!("{}.age", file));

    if !secret_file.exists() {
        Output::error(&format!(
            "Secret '{}' not found for project '{}'",
            file, normalized_url
        ));
        return Ok(());
    }

    std::fs::remove_file(&secret_file)?;

    // Commit
    let git = GitBackend::open(&repo_dir)?;
    git.commit(
        &format!("Remove project secret: {}/{}", normalized_url, file),
        "tether",
    )?;

    Output::success(&format!(
        "Removed '{}' from project '{}'",
        file, normalized_url
    ));
    Ok(())
}

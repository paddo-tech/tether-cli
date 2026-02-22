use crate::cli::{Output, Progress, Prompt};
use crate::config::{Config, FeaturesConfig};
use crate::github::GitHubCli;
use crate::sync::{GitBackend, SyncEngine, SyncState};
use anyhow::Result;

pub async fn run(repo: Option<&str>, no_daemon: bool, team_only: bool) -> Result<()> {
    Output::header("Welcome to Tether!");
    Output::dim("Sync your dev environment across machines");
    println!();

    // Check if already initialized
    let config_path = Config::config_path()?;
    let already_initialized = config_path.exists();

    if already_initialized {
        Output::info("Tether is already initialized");

        // Sync first to ensure we don't lose any data (skip if no personal repo)
        let existing_config = Config::load().ok();
        let has_personal = existing_config
            .as_ref()
            .map(|c| c.has_personal_repo())
            .unwrap_or(false);

        if has_personal {
            Output::info("Running sync to preserve your data...");
            if let Err(e) = super::sync::run(false, false).await {
                Output::warning(&format!("Sync failed: {}", e));
                if !Prompt::confirm(
                    "Continue with reinit anyway? (may lose unsynced changes)",
                    false,
                )? {
                    return Ok(());
                }
            }
        }

        if !Prompt::confirm("Reinitialize? (your config will be preserved)", false)? {
            return Ok(());
        }
    }

    // Load existing config or create new one
    let mut config = if already_initialized {
        Config::load().unwrap_or_default()
    } else {
        Config::default()
    };

    // Legacy --team-only flag support
    if team_only {
        config.features.personal_dotfiles = false;
        config.features.personal_packages = false;
        config.features.team_dotfiles = true;
    } else {
        // Feature selection prompt
        let features = select_features(&config.features)?;
        config.features = features;
    }

    let needs_personal_repo =
        config.features.personal_dotfiles || config.features.personal_packages;

    // Personal repo setup (if personal features enabled)
    if needs_personal_repo {
        let repo_url = if let Some(url) = repo {
            url.to_string()
        } else if already_initialized && !config.backend.url.is_empty() {
            Output::dim(&format!("  Current repo: {}", config.backend.url));
            if Prompt::confirm("Keep current repository?", true)? {
                config.backend.url.clone()
            } else {
                setup_repository().await?
            }
        } else {
            setup_repository().await?
        };

        if repo_url.is_empty() {
            Output::error("Repository URL cannot be empty");
            return Err(anyhow::anyhow!("Repository URL is required"));
        }

        config.backend.url = repo_url.clone();

        // Create .tether directory
        let tether_dir = Config::config_dir()?;
        std::fs::create_dir_all(&tether_dir)?;

        // Clone or pull repository
        let sync_path = SyncEngine::sync_path()?;
        if sync_path.exists() {
            let git = GitBackend::open(&sync_path)?;
            git.pull()?;
        } else {
            GitBackend::clone(&repo_url, &sync_path)?;
        }

        // Create sync repo structure
        std::fs::create_dir_all(sync_path.join("manifests"))?;
        std::fs::create_dir_all(sync_path.join("profiles"))?;
        std::fs::create_dir_all(sync_path.join("machines"))?;

        crate::sync::check_sync_format_version(&sync_path)?;

        // Setup encryption if enabled
        if config.security.encrypt_dotfiles {
            setup_encryption()?;
        }
    } else {
        // No personal features - create minimal .tether directory
        // Note: We don't clear dotfiles/packages config, just disable syncing
        // This preserves settings if user re-enables features later
        let tether_dir = Config::config_dir()?;
        std::fs::create_dir_all(&tether_dir)?;
        config.backend.url = String::new();
        config.security.encrypt_dotfiles = false;
    }

    // Profile assignment
    if needs_personal_repo {
        assign_profile_during_init(&mut config)?;
    }

    config.save()?;

    // Create initial state
    let state = SyncState::load()?;
    state.save()?;

    // Initial sync (only if personal features enabled)
    if needs_personal_repo {
        super::sync::run(false, false).await?;
    }

    // Install daemon for auto-sync (unless opted out)
    if !no_daemon {
        if let Err(err) = super::daemon::install().await {
            Output::warning(&format!("Failed to install daemon: {}", err));
        }
    }

    let sync_path = SyncEngine::sync_path()?;
    Output::success("Initialized!");
    println!("  Config: {}", config_path.display());
    if needs_personal_repo {
        println!("  Sync:   {}", sync_path.display());
    }

    // Follow-up setup for selected features
    println!();

    if config.features.team_dotfiles {
        Output::info("Team dotfiles enabled. Set up your team:");
        println!("  tether team setup");
        println!();
    }

    if config.features.collab_secrets {
        Output::info("Project collaboration enabled. In a project directory:");
        println!("  tether collab init");
        println!();
    }

    Ok(())
}

/// Prompt user to select features
fn select_features(current: &FeaturesConfig) -> Result<FeaturesConfig> {
    let options = vec![
        "Personal dotfiles (shell, git, etc.)",
        "Personal packages (brew, npm, etc.)",
        "Team dotfiles (org-based)",
        "Project secrets with collaborators",
    ];

    // Default selections based on current config
    let mut defaults = Vec::new();
    if current.personal_dotfiles {
        defaults.push(0);
    }
    if current.personal_packages {
        defaults.push(1);
    }
    if current.team_dotfiles {
        defaults.push(2);
    }
    if current.collab_secrets {
        defaults.push(3);
    }
    // Default to personal dotfiles + packages for new installs
    if defaults.is_empty() {
        defaults = vec![0, 1];
    }

    let selected = Prompt::multi_select("What would you like to sync?", options, &defaults)?;

    Ok(FeaturesConfig {
        personal_dotfiles: selected.contains(&0),
        personal_packages: selected.contains(&1),
        team_dotfiles: selected.contains(&2),
        collab_secrets: selected.contains(&3),
        team_layering: current.team_layering, // Preserve hidden setting
    })
}

/// Assign a profile to the current machine during init.
fn assign_profile_during_init(config: &mut Config) -> Result<()> {
    let state = SyncState::load()?;
    let machine_id = &state.machine_id;

    // Already assigned
    if config.machine_profiles.contains_key(machine_id) {
        return Ok(());
    }

    if config.profiles.is_empty() {
        // No profiles exist yet — v1->v2 migration should have created "dev"
        // but if it hasn't (e.g., fresh init), create it now
        config.migrate_v1_to_v2();
        config
            .machine_profiles
            .insert(machine_id.clone(), "dev".to_string());
        return Ok(());
    }

    // Profiles exist — let user pick
    let mut names: Vec<&str> = config.profiles.keys().map(|s| s.as_str()).collect();
    names.sort();
    let mut options: Vec<&str> = names.clone();
    options.push("Create new");

    let idx = Prompt::select("Assign a profile to this machine", options.clone(), 0)?;

    if idx < names.len() {
        config
            .machine_profiles
            .insert(machine_id.clone(), names[idx].to_string());
        Output::success(&format!("Assigned profile '{}'", names[idx]));
    } else {
        // Create new
        let name = Prompt::input("Profile name", None)?;
        if name.is_empty() {
            Output::warning("Skipping profile creation");
            return Ok(());
        }
        if !Config::is_safe_profile_name(&name) {
            Output::error(&format!("Invalid profile name: '{}'", name));
            return Ok(());
        }
        // Ensure "dev" profile exists as a base to clone
        if config.profiles.is_empty() {
            config.migrate_v1_to_v2();
        }
        if !config.profiles.contains_key(&name) {
            // Clone dev profile as starting point
            if let Some(dev) = config.profiles.get("dev").cloned() {
                config.profiles.insert(name.clone(), dev);
            } else {
                config
                    .profiles
                    .insert(name.clone(), crate::config::ProfileConfig::default());
            }
        }
        config
            .machine_profiles
            .insert(machine_id.clone(), name.clone());
        Output::success(&format!("Created and assigned profile '{}'", name));
    }

    Ok(())
}

fn setup_encryption() -> Result<()> {
    if crate::security::has_encryption_key() {
        Output::info("Encrypted key found. Enter passphrase:");
        let passphrase = Prompt::password("Passphrase")?;
        crate::security::unlock_with_passphrase(&passphrase)?;
    } else {
        Output::info("Creating encryption key. Choose a passphrase (min 8 chars).");
        println!();

        let passphrase = Prompt::password("Passphrase")?;
        let confirm = Prompt::password("Confirm passphrase")?;

        if passphrase != confirm {
            return Err(anyhow::anyhow!("Passphrases do not match"));
        }

        if passphrase.len() < 8 {
            return Err(anyhow::anyhow!("Passphrase must be at least 8 characters"));
        }

        let key = crate::security::encryption::generate_key();
        crate::security::store_encryption_key_with_passphrase(&key, &passphrase)?;
        crate::security::unlock_with_passphrase(&passphrase)?;
    }
    Ok(())
}

async fn setup_repository() -> Result<String> {
    let options = vec![
        "GitHub (automatic - recommended)",
        "GitHub (manual - I'll create the repo)",
        "GitLab",
        "Custom Git URL",
    ];

    let selection = Prompt::select("How would you like to sync your dotfiles?", options, 0)?;

    match selection {
        0 => {
            Output::info("Setting up GitHub sync...");
            setup_github_automatic().await
        }
        1 => {
            Output::info("Create a private repository on GitHub first");
            Output::dim("  Visit: https://github.com/new");
            println!();
            Prompt::input_with_help(
                "Repository URL",
                None,
                "e.g., https://github.com/user/tether-sync.git",
            )
        }
        2 => {
            Output::info("Create a private repository on GitLab first");
            Output::dim("  Visit: https://gitlab.com/projects/new");
            println!();
            Prompt::input_with_help(
                "Repository URL",
                None,
                "e.g., https://gitlab.com/user/tether-sync.git",
            )
        }
        3 => Prompt::input_with_help("Git repository URL", None, "SSH or HTTPS URL"),
        _ => unreachable!(),
    }
}

async fn setup_github_automatic() -> Result<String> {
    if !GitHubCli::is_installed() {
        Output::warning("GitHub CLI (gh) is not installed");

        if Prompt::confirm("Install GitHub CLI via Homebrew?", true)? {
            let pb = Progress::spinner("Installing GitHub CLI...");
            GitHubCli::install().await?;
            Progress::finish_success(&pb, "GitHub CLI installed");
        } else {
            Output::info("Falling back to manual setup");
            return Prompt::input_with_help(
                "GitHub repository URL",
                None,
                "SSH or HTTPS URL to your repo",
            );
        }
    }

    if !GitHubCli::is_authenticated().await? {
        Output::info("Authenticating with GitHub...");
        Output::dim(&format!("  {} This will open your browser", Output::ARROW));

        if Prompt::confirm("Continue?", true)? {
            GitHubCli::authenticate().await?;
            Output::success("Authenticated with GitHub");
        } else {
            return Err(anyhow::anyhow!("GitHub authentication required"));
        }
    } else {
        let username = GitHubCli::get_username().await?;
        Output::success(&format!("Authenticated as @{}", username));
    }

    if !GitHubCli::check_ssh_access().await? {
        Output::warning("SSH key not configured with GitHub");
        Output::dim("  Tether uses SSH for secure Git operations");

        if Prompt::confirm("Set up SSH key now?", true)? {
            Output::info("Follow the prompts to add your SSH key...");
            if let Err(e) = GitHubCli::setup_ssh_key().await {
                Output::warning(&format!("Automatic setup failed: {}", e));
                println!();
                Output::info("Manual setup:");
                Output::list_item("Generate: ssh-keygen -t ed25519 -C \"your@email.com\"");
                Output::list_item("Add: gh ssh-key add ~/.ssh/id_ed25519.pub");
                Output::dim("  Or visit: https://github.com/settings/keys");
                println!();

                if !Prompt::confirm("Continue after setting up SSH key?", false)? {
                    return Err(anyhow::anyhow!("SSH key setup required"));
                }
            }
        } else {
            Output::warning("SSH key required for Git operations");
            Output::dim(
                "  https://docs.github.com/en/authentication/connecting-to-github-with-ssh",
            );
            return Err(anyhow::anyhow!("SSH key setup required"));
        }
    }

    let default_name = "tether-sync";
    let repo_name = Prompt::input("Repository name", Some(default_name))?;

    let username = GitHubCli::get_username().await?;
    if GitHubCli::repo_exists(&username, &repo_name).await? {
        Output::warning(&format!("{}/{} already exists", username, repo_name));

        if Prompt::confirm("Use existing repository?", true)? {
            return Ok(format!("git@github.com:{}/{}.git", username, repo_name));
        } else {
            let alt_name = GitHubCli::suggest_repo_name(&repo_name, &username).await?;
            Output::dim(&format!("  Suggested: {}", alt_name));

            let final_name = Prompt::input("Repository name", Some(&alt_name))?;
            return create_github_repo(&final_name).await;
        }
    }

    create_github_repo(&repo_name).await
}

async fn create_github_repo(repo_name: &str) -> Result<String> {
    let pb = Progress::spinner("Creating private repository...");
    let repo_url = GitHubCli::create_repo(repo_name, true).await?;
    Progress::finish_success(&pb, "Repository created");

    let username = GitHubCli::get_username().await?;
    Output::dim(&format!("  https://github.com/{}/{}", username, repo_name));

    Ok(repo_url)
}

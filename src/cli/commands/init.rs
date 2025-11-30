use crate::cli::{Output, Prompt};
use crate::config::Config;
use crate::github::GitHubCli;
use crate::sync::{GitBackend, SyncEngine, SyncState};
use anyhow::Result;
use indicatif::{ProgressBar, ProgressStyle};
use owo_colors::OwoColorize;

pub async fn run(repo: Option<&str>, no_daemon: bool) -> Result<()> {
    Output::header("Welcome to Tether!");
    println!(
        "{}",
        "Sync your dev environment across machines".bright_black()
    );
    println!();

    // Check if already initialized
    let config_path = Config::config_path()?;
    if config_path.exists() {
        Output::warning("Tether is already initialized");
        let overwrite = Prompt::confirm("Do you want to reinitialize?", false)?;
        if !overwrite {
            return Ok(());
        }
    }

    // Get repository URL
    let repo_url = if let Some(url) = repo {
        url.to_string()
    } else {
        setup_repository().await?
    };

    if repo_url.is_empty() {
        Output::error("Repository URL cannot be empty");
        return Err(anyhow::anyhow!("Repository URL is required"));
    }

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

    // Create default config
    let mut config = Config::default();
    config.backend.url = repo_url;
    config.save()?;

    // Setup encryption if enabled
    if config.security.encrypt_dotfiles {
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
    }

    // Create initial state
    let state = SyncState::load()?;
    state.save()?;

    // Create sync repo structure
    std::fs::create_dir_all(sync_path.join("manifests"))?;
    std::fs::create_dir_all(sync_path.join("dotfiles"))?;
    std::fs::create_dir_all(sync_path.join("machines"))?;

    // Initial sync
    super::sync::run(false, false).await?;

    // Start daemon if requested
    if !no_daemon {
        if let Err(err) = super::daemon::start().await {
            Output::warning(&format!("Failed to start daemon: {}", err));
        }
    }

    Output::success("Initialized!");
    println!("  Config: {}", config_path.display());
    println!("  Sync:   {}", sync_path.display());

    Ok(())
}

async fn setup_repository() -> Result<String> {
    let options = vec![
        "GitHub (automatic - recommended) ⭐",
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
            Output::info("You'll need to create a private repository on GitHub first.");
            Output::info("Visit: https://github.com/new");
            println!();
            Prompt::input(
                "Enter the repository URL (e.g., https://github.com/user/tether-sync.git)",
                None,
            )
        }
        2 => {
            Output::info("You'll need to create a private repository on GitLab first.");
            Output::info("Visit: https://gitlab.com/projects/new");
            println!();
            Prompt::input(
                "Enter the repository URL (e.g., https://gitlab.com/user/tether-sync.git)",
                None,
            )
        }
        3 => Prompt::input("Enter your Git repository URL", None),
        _ => unreachable!(),
    }
}

async fn setup_github_automatic() -> Result<String> {
    if !GitHubCli::is_installed() {
        Output::warning("GitHub CLI (gh) is not installed");

        if Prompt::confirm("Install GitHub CLI via Homebrew?", true)? {
            let pb = ProgressBar::new_spinner();
            pb.set_style(
                ProgressStyle::default_spinner()
                    .template("{spinner:.green} {msg}")
                    .unwrap(),
            );
            pb.set_message("Installing GitHub CLI...");
            pb.enable_steady_tick(std::time::Duration::from_millis(100));

            GitHubCli::install().await?;

            pb.finish_with_message("GitHub CLI installed ✓");
        } else {
            Output::info("Falling back to manual setup");
            return Prompt::input("Enter your GitHub repository URL", None);
        }
    }

    if !GitHubCli::is_authenticated().await? {
        Output::info("Authenticating with GitHub...");
        println!("  → This will open your browser");

        if Prompt::confirm("Continue?", true)? {
            GitHubCli::authenticate().await?;
            Output::success("Authenticated with GitHub");
        } else {
            return Err(anyhow::anyhow!("GitHub authentication required"));
        }
    } else {
        let username = GitHubCli::get_username().await?;
        Output::success(&format!("Already authenticated as @{}", username));
    }

    if !GitHubCli::check_ssh_access().await? {
        Output::warning("SSH key not configured with GitHub");
        Output::info("Tether uses SSH for secure Git operations");

        if Prompt::confirm("Set up SSH key now?", true)? {
            Output::info("Follow the prompts to add your SSH key to GitHub...");
            if let Err(e) = GitHubCli::setup_ssh_key().await {
                Output::warning(&format!("Automatic setup failed: {}", e));
                Output::info("Manual setup:");
                Output::info("  1. Generate key: ssh-keygen -t ed25519 -C \"your@email.com\"");
                Output::info("  2. Add to GitHub: gh ssh-key add ~/.ssh/id_ed25519.pub");
                Output::info("  Or visit: https://github.com/settings/keys");

                if !Prompt::confirm("Continue after setting up SSH key?", false)? {
                    return Err(anyhow::anyhow!("SSH key setup required"));
                }
            }
        } else {
            Output::warning("SSH key required for Git operations");
            Output::info(
                "Setup instructions: https://docs.github.com/en/authentication/connecting-to-github-with-ssh",
            );
            return Err(anyhow::anyhow!("SSH key setup required"));
        }
    }

    let default_name = "tether-sync";
    let repo_name = Prompt::input("Repository name", Some(default_name))?;

    let username = GitHubCli::get_username().await?;
    if GitHubCli::repo_exists(&username, &repo_name).await? {
        Output::warning(&format!(
            "Repository {}/{} already exists",
            username, repo_name
        ));

        if Prompt::confirm("Use existing repository?", true)? {
            return Ok(format!("git@github.com:{}/{}.git", username, repo_name));
        } else {
            let alt_name = GitHubCli::suggest_repo_name(&repo_name, &username).await?;
            Output::info(&format!("Suggested name: {}", alt_name));

            let final_name = Prompt::input("Repository name", Some(&alt_name))?;
            return create_github_repo(&final_name).await;
        }
    }

    create_github_repo(&repo_name).await
}

async fn create_github_repo(repo_name: &str) -> Result<String> {
    let pb = ProgressBar::new_spinner();
    pb.set_style(
        ProgressStyle::default_spinner()
            .template("{spinner:.green} {msg}")
            .unwrap(),
    );
    pb.set_message("Creating private repository...");
    pb.enable_steady_tick(std::time::Duration::from_millis(100));

    let repo_url = GitHubCli::create_repo(repo_name, true).await?;

    pb.finish_with_message("Repository created ✓");

    let username = GitHubCli::get_username().await?;
    Output::success(&format!(
        "Created https://github.com/{}/{}",
        username, repo_name
    ));

    Ok(repo_url)
}

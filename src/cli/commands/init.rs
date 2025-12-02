use crate::cli::{Output, Progress, Prompt};
use crate::config::Config;
use crate::github::GitHubCli;
use crate::sync::{GitBackend, SyncEngine, SyncState};
use anyhow::Result;

pub async fn run(repo: Option<&str>, no_daemon: bool) -> Result<()> {
    Output::header("Welcome to Tether!");
    Output::dim("Sync your dev environment across machines");
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

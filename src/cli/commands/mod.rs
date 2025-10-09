use anyhow::Result;
use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "tether")]
#[command(about = "Sync your development environment across multiple Macs", long_about = None)]
#[command(version)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Commands,
}

#[derive(Subcommand)]
pub enum Commands {
    /// Initialize Tether on this machine
    Init {
        /// Git repository URL
        #[arg(long)]
        repo: Option<String>,

        /// Don't start the daemon automatically
        #[arg(long)]
        no_daemon: bool,
    },

    /// Manually trigger a sync
    Sync {
        /// Show what would be synced without doing it
        #[arg(long)]
        dry_run: bool,

        /// Skip conflict prompts
        #[arg(long)]
        force: bool,
    },

    /// Show current sync status
    Status,

    /// Show differences between machines
    Diff {
        /// Compare with specific machine
        #[arg(long)]
        machine: Option<String>,
    },

    /// Control the background daemon
    Daemon {
        #[command(subcommand)]
        action: DaemonAction,
    },

    /// Manage machines in sync network
    Machines {
        #[command(subcommand)]
        action: MachineAction,
    },

    /// Manage ignore patterns
    Ignore {
        #[command(subcommand)]
        action: IgnoreAction,
    },

    /// Manage configuration
    Config {
        #[command(subcommand)]
        action: ConfigAction,
    },
}

#[derive(Subcommand)]
pub enum DaemonAction {
    /// Start the daemon
    Start,
    /// Stop the daemon
    Stop,
    /// Restart the daemon
    Restart,
    /// View daemon logs
    Logs,
}

#[derive(Subcommand)]
pub enum MachineAction {
    /// List all machines
    List,
    /// Rename this machine
    Rename { old: String, new: String },
    /// Remove a machine from sync
    Remove { name: String },
}

#[derive(Subcommand)]
pub enum IgnoreAction {
    /// Add ignore pattern
    Add { pattern: String },
    /// List ignore patterns
    List,
    /// Remove ignore pattern
    Remove { pattern: String },
}

#[derive(Subcommand)]
pub enum ConfigAction {
    /// Get config value
    Get { key: String },
    /// Set config value
    Set { key: String, value: String },
    /// Open config in editor
    Edit,
}

impl Cli {
    pub async fn run(&self) -> Result<()> {
        match &self.command {
            Commands::Init { repo, no_daemon } => self.init(repo.as_deref(), *no_daemon).await,
            Commands::Sync { dry_run, force } => self.sync(*dry_run, *force).await,
            Commands::Status => self.status().await,
            Commands::Diff { machine } => self.diff(machine.as_deref()).await,
            Commands::Daemon { action } => self.daemon(action).await,
            Commands::Machines { action } => self.machines(action).await,
            Commands::Ignore { action } => self.ignore(action).await,
            Commands::Config { action } => self.config(action).await,
        }
    }

    async fn init(&self, repo: Option<&str>, no_daemon: bool) -> Result<()> {
        use crate::cli::{Output, Prompt};
        use crate::config::Config;
        use crate::sync::{GitBackend, SyncEngine, SyncState};
        use owo_colors::OwoColorize;

        Output::header("🔗 Welcome to Tether!");
        println!(
            "{}",
            "Sync your development environment across all your Macs".bright_black()
        );
        println!();
        Output::info("Initializing Tether...");

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
            // Manual repo URL provided via --repo flag
            url.to_string()
        } else {
            // Interactive setup wizard
            self.setup_repository().await?
        };

        if repo_url.is_empty() {
            Output::error("Repository URL cannot be empty");
            return Err(anyhow::anyhow!("Repository URL is required"));
        }

        // Create .tether directory
        let tether_dir = Config::config_dir()?;
        std::fs::create_dir_all(&tether_dir)?;
        Output::success(&format!("Created directory: {}", tether_dir.display()));

        // Clone or pull repository
        let sync_path = SyncEngine::sync_path()?;

        if sync_path.exists() {
            Output::info("Sync directory already exists, pulling latest changes...");
            let git = GitBackend::open(&sync_path)?;
            git.pull()?;
            Output::success("Pulled latest changes");
        } else {
            Output::info("Cloning repository...");
            GitBackend::clone(&repo_url, &sync_path)?;
            Output::success("Repository cloned successfully");
        }

        // Create default config
        let mut config = Config::default();
        config.backend.url = repo_url;
        config.save()?;
        Output::success("Configuration saved");

        // Create initial state
        let state = SyncState::load()?;
        state.save()?;
        Output::success(&format!(
            "Initialized with machine ID: {}",
            state.machine_id
        ));

        // Create manifests directory in sync repo
        let manifests_dir = sync_path.join("manifests");
        std::fs::create_dir_all(&manifests_dir)?;

        // Create dotfiles directory in sync repo
        let dotfiles_dir = sync_path.join("dotfiles");
        std::fs::create_dir_all(&dotfiles_dir)?;

        // Create machines directory in sync repo
        let machines_dir = sync_path.join("machines");
        std::fs::create_dir_all(&machines_dir)?;

        Output::success("Sync repository structure created");

        // Initial sync
        Output::info("Performing initial sync...");
        self.sync(false, false).await?;

        // Start daemon if requested
        if !no_daemon {
            Output::info("Starting daemon...");
            // TODO: Start daemon
            Output::warning("Daemon support coming soon - run 'tether daemon start' manually");
        }

        Output::success("Tether initialized successfully!");
        Output::info(&format!("Config: {}", config_path.display()));
        Output::info(&format!("Sync directory: {}", sync_path.display()));

        Ok(())
    }

    async fn sync(&self, dry_run: bool, _force: bool) -> Result<()> {
        use crate::cli::Output;
        use crate::config::Config;
        use crate::packages::{BrewManager, NpmManager, PackageManager, PnpmManager};
        use crate::sync::{GitBackend, SyncEngine, SyncState};
        use sha2::{Digest, Sha256};

        if dry_run {
            Output::info("Running in dry-run mode...");
        } else {
            Output::info("Starting sync...");
        }

        // Load config
        let config = Config::load()?;
        let sync_path = SyncEngine::sync_path()?;
        let home =
            home::home_dir().ok_or_else(|| anyhow::anyhow!("Could not find home directory"))?;

        // Pull latest changes
        Output::info("Pulling latest changes...");
        let git = GitBackend::open(&sync_path)?;
        if !dry_run {
            git.pull()?;
        }
        Output::success("Pulled latest changes");

        // Sync dotfiles
        Output::info("Syncing dotfiles...");
        let dotfiles_dir = sync_path.join("dotfiles");
        std::fs::create_dir_all(&dotfiles_dir)?;

        let mut state = SyncState::load()?;
        let mut changes_made = false;

        for file in &config.dotfiles.files {
            let source = home.join(file);
            let dest = dotfiles_dir.join(file.trim_start_matches('.'));

            if source.exists() {
                if let Ok(content) = std::fs::read(&source) {
                    let hash = format!("{:x}", Sha256::digest(&content));

                    // Check if file changed
                    let file_changed = state
                        .files
                        .get(file)
                        .map(|f| f.hash != hash)
                        .unwrap_or(true);

                    if file_changed {
                        Output::info(&format!("  {} (changed)", file));
                        if !dry_run {
                            if let Some(parent) = dest.parent() {
                                std::fs::create_dir_all(parent)?;
                            }
                            std::fs::copy(&source, &dest)?;
                            state.update_file(file, hash);
                            changes_made = true;
                        }
                    } else {
                        Output::info(&format!("  {} (unchanged)", file));
                    }
                }
            }
        }

        // Sync package manifests
        Output::info("Syncing package manifests...");
        let manifests_dir = sync_path.join("manifests");
        std::fs::create_dir_all(&manifests_dir)?;

        // Homebrew
        if config.packages.brew.enabled {
            let brew = BrewManager::new();
            if brew.is_available().await {
                Output::info("  Syncing Homebrew packages...");
                let packages = brew.list_installed().await?;
                let manifest = serde_json::to_string_pretty(&packages)?;
                let hash = format!("{:x}", Sha256::digest(manifest.as_bytes()));

                if state
                    .packages
                    .get("brew")
                    .map(|p| p.hash != hash)
                    .unwrap_or(true)
                {
                    Output::info(&format!("    {} packages found", packages.len()));
                    if !dry_run {
                        std::fs::write(manifests_dir.join("brew.json"), manifest)?;
                        use chrono::Utc;
                        state.packages.insert(
                            "brew".to_string(),
                            crate::sync::state::PackageState {
                                last_sync: Utc::now(),
                                hash,
                            },
                        );
                        changes_made = true;
                    }
                }
            }
        }

        // npm
        if config.packages.npm.enabled {
            let npm = NpmManager::new();
            if npm.is_available().await {
                Output::info("  Syncing npm packages...");
                let packages = npm.list_installed().await?;
                let manifest = serde_json::to_string_pretty(&packages)?;
                let hash = format!("{:x}", Sha256::digest(manifest.as_bytes()));

                if state
                    .packages
                    .get("npm")
                    .map(|p| p.hash != hash)
                    .unwrap_or(true)
                {
                    Output::info(&format!("    {} packages found", packages.len()));
                    if !dry_run {
                        std::fs::write(manifests_dir.join("npm.json"), manifest)?;
                        use chrono::Utc;
                        state.packages.insert(
                            "npm".to_string(),
                            crate::sync::state::PackageState {
                                last_sync: Utc::now(),
                                hash,
                            },
                        );
                        changes_made = true;
                    }
                }
            }
        }

        // pnpm
        if config.packages.pnpm.enabled {
            let pnpm = PnpmManager::new();
            if pnpm.is_available().await {
                Output::info("  Syncing pnpm packages...");
                let packages = pnpm.list_installed().await?;
                let manifest = serde_json::to_string_pretty(&packages)?;
                let hash = format!("{:x}", Sha256::digest(manifest.as_bytes()));

                if state
                    .packages
                    .get("pnpm")
                    .map(|p| p.hash != hash)
                    .unwrap_or(true)
                {
                    Output::info(&format!("    {} packages found", packages.len()));
                    if !dry_run {
                        std::fs::write(manifests_dir.join("pnpm.json"), manifest)?;
                        use chrono::Utc;
                        state.packages.insert(
                            "pnpm".to_string(),
                            crate::sync::state::PackageState {
                                last_sync: Utc::now(),
                                hash,
                            },
                        );
                        changes_made = true;
                    }
                }
            }
        }

        // Commit and push changes
        if changes_made && !dry_run {
            Output::info("Committing changes...");
            git.commit("Sync dotfiles and packages", &state.machine_id)?;
            Output::success("Changes committed");

            Output::info("Pushing to remote...");
            git.push()?;
            Output::success("Changes pushed");

            state.mark_synced();
            state.save()?;
        } else if dry_run {
            Output::info("Dry-run complete - no changes made");
        } else {
            Output::info("No changes to sync");
        }

        Output::success("Sync complete!");
        Ok(())
    }

    async fn status(&self) -> Result<()> {
        use crate::cli::Output;
        use crate::config::Config;
        use crate::sync::SyncState;
        use comfy_table::{presets::UTF8_FULL, Attribute, Cell, Color, ContentArrangement, Table};
        use owo_colors::OwoColorize;

        // Load config and state
        let config = match Config::load() {
            Ok(c) => c,
            Err(_) => {
                Output::error("Tether is not initialized. Run 'tether init' first.");
                return Ok(());
            }
        };

        let state = SyncState::load()?;

        // Header
        println!();
        println!("{}", "🔗 Tether Status".bright_cyan().bold());
        println!();

        // Daemon table
        let mut daemon_table = Table::new();
        daemon_table
            .load_preset(UTF8_FULL)
            .set_content_arrangement(ContentArrangement::Dynamic)
            .set_header(vec![
                Cell::new("Daemon")
                    .add_attribute(Attribute::Bold)
                    .fg(Color::Cyan),
                Cell::new(""),
            ])
            .add_row(vec![
                Cell::new("Status"),
                Cell::new("● Not running").fg(Color::Yellow),
            ])
            .add_row(vec![
                Cell::new("Info"),
                Cell::new("Daemon support coming soon"),
            ]);
        println!("{daemon_table}");
        println!();

        // Sync table
        let mut sync_table = Table::new();
        sync_table
            .load_preset(UTF8_FULL)
            .set_content_arrangement(ContentArrangement::Dynamic)
            .set_header(vec![
                Cell::new("Sync")
                    .add_attribute(Attribute::Bold)
                    .fg(Color::Cyan),
                Cell::new(""),
            ])
            .add_row(vec![
                Cell::new("Last Sync"),
                Cell::new(state.last_sync.format("%Y-%m-%d %H:%M:%S").to_string()).fg(Color::Green),
            ])
            .add_row(vec![Cell::new("Machine ID"), Cell::new(&state.machine_id)])
            .add_row(vec![Cell::new("Backend"), Cell::new(&config.backend.url)]);
        println!("{sync_table}");
        println!();

        // Dotfiles table
        if !state.files.is_empty() {
            let mut files_table = Table::new();
            files_table
                .load_preset(UTF8_FULL)
                .set_content_arrangement(ContentArrangement::Dynamic)
                .set_header(vec![
                    Cell::new("📁 Dotfiles")
                        .add_attribute(Attribute::Bold)
                        .fg(Color::Cyan),
                    Cell::new("Status")
                        .add_attribute(Attribute::Bold)
                        .fg(Color::Cyan),
                    Cell::new("Last Modified")
                        .add_attribute(Attribute::Bold)
                        .fg(Color::Cyan),
                ]);

            for (file, file_state) in &state.files {
                let status_cell = if file_state.synced {
                    Cell::new("✓ Synced").fg(Color::Green)
                } else {
                    Cell::new("⚠ Modified").fg(Color::Yellow)
                };

                files_table.add_row(vec![
                    Cell::new(file),
                    status_cell,
                    Cell::new(
                        file_state
                            .last_modified
                            .format("%Y-%m-%d %H:%M:%S")
                            .to_string(),
                    ),
                ]);
            }
            println!("{files_table}");
            println!();
        } else {
            println!("{}", "📁 Dotfiles: No files synced yet".bright_black());
            println!();
        }

        // Packages table
        if !state.packages.is_empty() {
            let mut packages_table = Table::new();
            packages_table
                .load_preset(UTF8_FULL)
                .set_content_arrangement(ContentArrangement::Dynamic)
                .set_header(vec![
                    Cell::new("📦 Package Manager")
                        .add_attribute(Attribute::Bold)
                        .fg(Color::Cyan),
                    Cell::new("Last Sync")
                        .add_attribute(Attribute::Bold)
                        .fg(Color::Cyan),
                ]);

            for (manager, pkg_state) in &state.packages {
                packages_table.add_row(vec![
                    Cell::new(format!("✓ {}", manager)).fg(Color::Green),
                    Cell::new(pkg_state.last_sync.format("%Y-%m-%d %H:%M:%S").to_string()),
                ]);
            }
            println!("{packages_table}");
            println!();
        } else {
            println!("{}", "📦 Packages: No packages synced yet".bright_black());
            println!();
        }

        Ok(())
    }

    async fn diff(&self, _machine: Option<&str>) -> Result<()> {
        crate::cli::Output::info("Diff...");
        // TODO: Implement diff
        Ok(())
    }

    async fn daemon(&self, _action: &DaemonAction) -> Result<()> {
        crate::cli::Output::info("Daemon...");
        // TODO: Implement daemon control
        Ok(())
    }

    async fn machines(&self, _action: &MachineAction) -> Result<()> {
        crate::cli::Output::info("Machines...");
        // TODO: Implement machines
        Ok(())
    }

    async fn ignore(&self, _action: &IgnoreAction) -> Result<()> {
        crate::cli::Output::info("Ignore...");
        // TODO: Implement ignore
        Ok(())
    }

    async fn config(&self, _action: &ConfigAction) -> Result<()> {
        crate::cli::Output::info("Config...");
        // TODO: Implement config
        Ok(())
    }

    // Helper method for repository setup wizard
    async fn setup_repository(&self) -> Result<String> {
        use crate::cli::{Output, Prompt};

        let options = vec![
            "GitHub (automatic - recommended) ⭐",
            "GitHub (manual - I'll create the repo)",
            "GitLab",
            "Custom Git URL",
        ];

        let selection = Prompt::select("How would you like to sync your dotfiles?", options, 0)?;

        match selection {
            0 => {
                // GitHub automatic setup
                Output::info("Setting up GitHub sync...");
                self.setup_github_automatic().await
            }
            1 => {
                // GitHub manual setup
                Output::info("You'll need to create a private repository on GitHub first.");
                Output::info("Visit: https://github.com/new");
                println!();
                Prompt::input(
                    "Enter the repository URL (e.g., https://github.com/user/tether-sync.git)",
                    None,
                )
            }
            2 => {
                // GitLab
                Output::info("You'll need to create a private repository on GitLab first.");
                Output::info("Visit: https://gitlab.com/projects/new");
                println!();
                Prompt::input(
                    "Enter the repository URL (e.g., https://gitlab.com/user/tether-sync.git)",
                    None,
                )
            }
            3 => {
                // Custom URL
                Prompt::input("Enter your Git repository URL", None)
            }
            _ => unreachable!(),
        }
    }

    async fn setup_github_automatic(&self) -> Result<String> {
        use crate::cli::{Output, Prompt};
        use crate::github::GitHubCli;
        use indicatif::{ProgressBar, ProgressStyle};

        // Check if gh CLI is installed
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

        // Check authentication
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

        // Check SSH access
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
                Output::info("Setup instructions: https://docs.github.com/en/authentication/connecting-to-github-with-ssh");
                return Err(anyhow::anyhow!("SSH key setup required"));
            }
        }

        // Get repository name
        let default_name = "tether-sync";
        let repo_name = Prompt::input("Repository name", Some(default_name))?;

        // Check if repo already exists
        let username = GitHubCli::get_username().await?;
        if GitHubCli::repo_exists(&username, &repo_name).await? {
            Output::warning(&format!(
                "Repository {}/{} already exists",
                username, repo_name
            ));

            if Prompt::confirm("Use existing repository?", true)? {
                return Ok(format!("git@github.com:{}/{}.git", username, repo_name));
            } else {
                // Suggest alternative name
                let alt_name = GitHubCli::suggest_repo_name(&repo_name, &username).await?;
                Output::info(&format!("Suggested name: {}", alt_name));

                let final_name = Prompt::input("Repository name", Some(&alt_name))?;
                return self.create_github_repo(&final_name).await;
            }
        }

        // Create repository
        self.create_github_repo(&repo_name).await
    }

    async fn create_github_repo(&self, repo_name: &str) -> Result<String> {
        use crate::cli::Output;
        use crate::github::GitHubCli;
        use indicatif::{ProgressBar, ProgressStyle};

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
}

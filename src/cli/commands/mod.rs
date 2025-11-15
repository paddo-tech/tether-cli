use crate::config::Config;
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

    /// Manage team sync
    Team {
        #[command(subcommand)]
        action: TeamAction,
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

#[derive(Subcommand)]
pub enum TeamAction {
    /// Add team sync repository
    Add {
        /// Team repository URL
        url: String,
        /// Skip auto-injection of source lines
        #[arg(long)]
        no_auto_inject: bool,
    },
    /// Remove team sync
    Remove,
    /// Enable team sync
    Enable,
    /// Disable team sync
    Disable,
    /// Show team sync status
    Status,
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
            Commands::Team { action } => self.team(action).await,
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

        // Setup encryption if enabled
        if config.security.encrypt_dotfiles {
            Output::info("Setting up encryption for dotfiles...");

            // Check if key already exists (from another machine via iCloud sync)
            if crate::security::keychain::has_encryption_key() {
                Output::success(
                    "Encryption key found in iCloud Keychain (synced from another device)",
                );
            } else {
                // Generate new encryption key
                let key = crate::security::encryption::generate_key();

                // Store in iCloud Keychain
                crate::security::store_encryption_key(&key)?;
                Output::success("Generated encryption key and stored in iCloud Keychain");
                Output::info("This key will automatically sync to your other Macs");
            }
        }

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
        use crate::packages::{
            BrewManager, BunManager, GemManager, NpmManager, PackageManager, PnpmManager,
        };
        use crate::sync::{GitBackend, SyncEngine, SyncState};
        use sha2::{Digest, Sha256};
        use std::path::PathBuf;

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

        // Decrypt dotfiles from sync repo (if encrypted)
        if config.security.encrypt_dotfiles && !dry_run {
            Output::info("Decrypting dotfiles from sync repository...");

            let key = crate::security::get_encryption_key()?;

            for file in &config.dotfiles.files {
                let filename = file.trim_start_matches('.');
                let enc_file = dotfiles_dir.join(format!("{}.enc", filename));

                if enc_file.exists() {
                    let encrypted_content = std::fs::read(&enc_file)?;
                    match crate::security::decrypt_file(&encrypted_content, &key) {
                        Ok(plaintext) => {
                            let local_file = home.join(file);
                            std::fs::write(&local_file, plaintext)?;
                            Output::info(&format!("  {} (decrypted)", file));
                        }
                        Err(e) => {
                            Output::warning(&format!("  {} (failed to decrypt: {})", file, e));
                        }
                    }
                }
            }

            // Decrypt global config directories from sync repo
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

                        // Only process .enc files
                        if file_name.ends_with(".enc") {
                            // Get relative path from configs dir
                            let rel_path = file_path.strip_prefix(&configs_dir).unwrap();
                            let rel_path_str = rel_path.to_string_lossy();
                            let rel_path_no_enc = rel_path_str.trim_end_matches(".enc");

                            // Decrypt and write to home directory
                            if let Ok(encrypted_content) = std::fs::read(file_path) {
                                match crate::security::decrypt_file(&encrypted_content, &key) {
                                    Ok(plaintext) => {
                                        let local_file = home.join(rel_path_no_enc);
                                        if let Some(parent) = local_file.parent() {
                                            std::fs::create_dir_all(parent)?;
                                        }
                                        std::fs::write(&local_file, plaintext)?;
                                        Output::info(&format!(
                                            "  ~/{} (decrypted)",
                                            rel_path_no_enc
                                        ));
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

            // Decrypt project-local configs from sync repo
            if config.project_configs.enabled {
                let projects_dir = sync_path.join("projects");
                if projects_dir.exists() {
                    Output::info("Decrypting project configs from sync repository...");

                    use crate::sync::git::{find_git_repos, get_remote_url, normalize_remote_url};
                    use walkdir::WalkDir;

                    // Build a map of normalized URLs to local repo paths
                    let mut repo_map = std::collections::HashMap::new();
                    for search_path_str in &config.project_configs.search_paths {
                        let search_path = if let Some(stripped) = search_path_str.strip_prefix("~/")
                        {
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

                    // Iterate through projects in sync repo
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

                        // Find matching local repo
                        if let Some(local_repo_path) = repo_map.get(&project_name) {
                            // Decrypt all files in this project directory
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
                                    // Get relative path from project dir
                                    let rel_path = enc_file.strip_prefix(project_dir).unwrap();
                                    let rel_path_str = rel_path.to_string_lossy();
                                    let rel_path_no_enc = rel_path_str.trim_end_matches(".enc");

                                    // Decrypt and write to local repo
                                    if let Ok(encrypted_content) = std::fs::read(enc_file) {
                                        match crate::security::decrypt_file(
                                            &encrypted_content,
                                            &key,
                                        ) {
                                            Ok(plaintext) => {
                                                let local_file =
                                                    local_repo_path.join(rel_path_no_enc);
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
                }
            }
        }

        // Sync dotfiles (local → Git)
        Output::info("Syncing dotfiles...");

        let mut state = SyncState::load()?;
        let mut changes_made = false;

        for file in &config.dotfiles.files {
            let source = home.join(file);

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
                        // Scan for secrets if enabled
                        if config.security.scan_secrets {
                            match crate::security::scan_for_secrets(&source) {
                                Ok(findings) if !findings.is_empty() => {
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
                                        Output::warning(&format!(
                                            "    ... and {} more",
                                            findings.len() - 3
                                        ));
                                    }
                                    Output::info("  Secrets will be encrypted before syncing");
                                }
                                _ => {}
                            }
                        }

                        Output::info(&format!("  {} (changed)", file));

                        if !dry_run {
                            let filename = file.trim_start_matches('.');

                            if config.security.encrypt_dotfiles {
                                // Encrypt and save as .enc
                                let key = crate::security::get_encryption_key()?;
                                let encrypted = crate::security::encrypt_file(&content, &key)?;

                                let dest = dotfiles_dir.join(format!("{}.enc", filename));
                                if let Some(parent) = dest.parent() {
                                    std::fs::create_dir_all(parent)?;
                                }
                                std::fs::write(&dest, encrypted)?;
                            } else {
                                // Save as plaintext
                                let dest = dotfiles_dir.join(filename);
                                if let Some(parent) = dest.parent() {
                                    std::fs::create_dir_all(parent)?;
                                }
                                std::fs::write(&dest, &content)?;
                            }

                            state.update_file(file, hash);
                            changes_made = true;
                        }
                    } else {
                        Output::info(&format!("  {} (unchanged)", file));
                    }
                }
            }
        }

        // Sync global config directories
        if !config.dotfiles.dirs.is_empty() {
            Output::info("Syncing global config directories...");

            let configs_dir = sync_path.join("configs");
            std::fs::create_dir_all(&configs_dir)?;

            for dir_path in &config.dotfiles.dirs {
                // Expand ~ to home directory
                let expanded_path = if let Some(stripped) = dir_path.strip_prefix("~/") {
                    home.join(stripped)
                } else {
                    PathBuf::from(dir_path)
                };

                if !expanded_path.exists() {
                    Output::warning(&format!("  {} (not found, skipping)", dir_path));
                    continue;
                }

                // Handle both files and directories
                if expanded_path.is_file() {
                    // Single file
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
                                // Store with path relative to home
                                let rel_path =
                                    expanded_path.strip_prefix(&home).unwrap_or(&expanded_path);
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
                                changes_made = true;
                            }
                        } else {
                            Output::info(&format!("  {} (unchanged)", dir_path));
                        }
                    }
                } else if expanded_path.is_dir() {
                    // Directory - recursively sync all files
                    Output::info(&format!("  {} (directory)", dir_path));

                    use walkdir::WalkDir;
                    for entry in WalkDir::new(&expanded_path).follow_links(false) {
                        let entry = match entry {
                            Ok(e) => e,
                            Err(_) => continue,
                        };

                        if entry.file_type().is_file() {
                            let file_path = entry.path();
                            let rel_to_home = file_path.strip_prefix(&home).unwrap_or(file_path);
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
                                        let encrypted =
                                            crate::security::encrypt_file(&content, &key)?;
                                        std::fs::write(
                                            format!("{}.enc", dest.display()),
                                            encrypted,
                                        )?;
                                    } else {
                                        std::fs::write(&dest, &content)?;
                                    }

                                    state.update_file(&state_key, hash);
                                    changes_made = true;
                                }
                            }
                        }
                    }
                }
            }
        }

        // Sync project-local configs (gitignored files in git repos)
        if config.project_configs.enabled {
            Output::info("Syncing project-local configs...");

            use crate::sync::git::{
                find_git_repos, get_remote_url, is_gitignored, normalize_remote_url,
            };

            let projects_dir = sync_path.join("projects");
            std::fs::create_dir_all(&projects_dir)?;

            for search_path_str in &config.project_configs.search_paths {
                // Expand ~ to home directory
                let search_path = if let Some(stripped) = search_path_str.strip_prefix("~/") {
                    home.join(stripped)
                } else {
                    PathBuf::from(search_path_str)
                };

                if !search_path.exists() {
                    continue;
                }

                // Find all git repos in this search path
                let repos = match find_git_repos(&search_path) {
                    Ok(r) => r,
                    Err(_) => continue,
                };

                for repo_path in repos {
                    // Get remote URL
                    let remote_url = match get_remote_url(&repo_path) {
                        Ok(url) => url,
                        Err(_) => continue, // Skip repos without remotes
                    };

                    let normalized_url = normalize_remote_url(&remote_url);

                    // Search for files matching patterns
                    use walkdir::WalkDir;
                    for pattern in &config.project_configs.patterns {
                        // Simple glob-like matching (supports * wildcard)
                        for entry in WalkDir::new(&repo_path).follow_links(false).max_depth(5)
                        // Limit depth to avoid deep recursion
                        {
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

                            // Check if file matches pattern (simple wildcard matching)
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

                            // Check if gitignored (if required)
                            if config.project_configs.only_if_gitignored {
                                match is_gitignored(file_path) {
                                    Ok(true) => {} // File is gitignored, continue
                                    _ => continue, // File not gitignored or error, skip
                                }
                            }

                            // Read and hash the file
                            if let Ok(content) = std::fs::read(file_path) {
                                let hash = format!("{:x}", Sha256::digest(&content));

                                // Create state key as project/file_path
                                let rel_to_repo = file_path.strip_prefix(&repo_path).unwrap();
                                let state_key =
                                    format!("project:{}/{}", normalized_url, rel_to_repo.display());

                                let file_changed = state
                                    .files
                                    .get(&state_key)
                                    .map(|f| f.hash != hash)
                                    .unwrap_or(true);

                                if file_changed {
                                    // Scan for secrets
                                    if config.security.scan_secrets {
                                        match crate::security::scan_for_secrets(file_path) {
                                            Ok(findings) if !findings.is_empty() => {
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
                                                Output::info(
                                                    "  Secrets will be encrypted before syncing",
                                                );
                                            }
                                            _ => {}
                                        }
                                    }

                                    Output::info(&format!(
                                        "  {}: {} (changed)",
                                        normalized_url,
                                        rel_to_repo.display()
                                    ));

                                    if !dry_run {
                                        // Store in projects/<normalized-url>/<relative-path>
                                        let dest =
                                            projects_dir.join(&normalized_url).join(rel_to_repo);

                                        if let Some(parent) = dest.parent() {
                                            std::fs::create_dir_all(parent)?;
                                        }

                                        if config.security.encrypt_dotfiles {
                                            let key = crate::security::get_encryption_key()?;
                                            let encrypted =
                                                crate::security::encrypt_file(&content, &key)?;
                                            std::fs::write(
                                                format!("{}.enc", dest.display()),
                                                encrypted,
                                            )?;
                                        } else {
                                            std::fs::write(&dest, &content)?;
                                        }

                                        state.update_file(&state_key, hash);
                                        changes_made = true;
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }

        // Sync package manifests using native tooling
        Output::info("Syncing package manifests...");
        let manifests_dir = sync_path.join("manifests");
        std::fs::create_dir_all(&manifests_dir)?;

        // Homebrew - use Brewfile
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
                                changes_made = true;
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

        // npm - use simple package list
        if config.packages.npm.enabled {
            let npm = NpmManager::new();
            if npm.is_available().await {
                Output::info("  Syncing npm packages...");

                match npm.export_manifest().await {
                    Ok(manifest) => {
                        let hash = format!("{:x}", Sha256::digest(manifest.as_bytes()));

                        if state
                            .packages
                            .get("npm")
                            .map(|p| p.hash != hash)
                            .unwrap_or(true)
                        {
                            let count = manifest.lines().filter(|l| !l.trim().is_empty()).count();
                            Output::info(&format!("    {} packages", count));
                            if !dry_run {
                                std::fs::write(manifests_dir.join("npm.txt"), manifest)?;
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
                        } else {
                            Output::info("    No changes");
                        }
                    }
                    Err(e) => {
                        Output::warning(&format!("Failed to export npm manifest: {}", e));
                    }
                }
            }
        }

        // pnpm - use simple package list
        if config.packages.pnpm.enabled {
            let pnpm = PnpmManager::new();
            if pnpm.is_available().await {
                Output::info("  Syncing pnpm packages...");

                match pnpm.export_manifest().await {
                    Ok(manifest) => {
                        let hash = format!("{:x}", Sha256::digest(manifest.as_bytes()));

                        if state
                            .packages
                            .get("pnpm")
                            .map(|p| p.hash != hash)
                            .unwrap_or(true)
                        {
                            let count = manifest.lines().filter(|l| !l.trim().is_empty()).count();
                            Output::info(&format!("    {} packages", count));
                            if !dry_run {
                                std::fs::write(manifests_dir.join("pnpm.txt"), manifest)?;
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
                        } else {
                            Output::info("    No changes");
                        }
                    }
                    Err(e) => {
                        Output::warning(&format!("Failed to export pnpm manifest: {}", e));
                    }
                }
            }
        }

        // bun - use simple package list
        if config.packages.bun.enabled {
            let bun = BunManager::new();
            if bun.is_available().await {
                Output::info("  Syncing bun packages...");

                match bun.export_manifest().await {
                    Ok(manifest) => {
                        let hash = format!("{:x}", Sha256::digest(manifest.as_bytes()));

                        if state
                            .packages
                            .get("bun")
                            .map(|p| p.hash != hash)
                            .unwrap_or(true)
                        {
                            let count = manifest.lines().filter(|l| !l.trim().is_empty()).count();
                            Output::info(&format!("    {} packages", count));
                            if !dry_run {
                                std::fs::write(manifests_dir.join("bun.txt"), manifest)?;
                                use chrono::Utc;
                                state.packages.insert(
                                    "bun".to_string(),
                                    crate::sync::state::PackageState {
                                        last_sync: Utc::now(),
                                        hash,
                                    },
                                );
                                changes_made = true;
                            }
                        } else {
                            Output::info("    No changes");
                        }
                    }
                    Err(e) => {
                        Output::warning(&format!("Failed to export bun manifest: {}", e));
                    }
                }
            }
        }

        // gem - use simple package list
        if config.packages.gem.enabled {
            let gem = GemManager::new();
            if gem.is_available().await {
                Output::info("  Syncing gem packages...");

                match gem.export_manifest().await {
                    Ok(manifest) => {
                        let hash = format!("{:x}", Sha256::digest(manifest.as_bytes()));

                        if state
                            .packages
                            .get("gem")
                            .map(|p| p.hash != hash)
                            .unwrap_or(true)
                        {
                            let count = manifest.lines().filter(|l| !l.trim().is_empty()).count();
                            Output::info(&format!("    {} packages", count));
                            if !dry_run {
                                std::fs::write(manifests_dir.join("gems.txt"), manifest)?;
                                use chrono::Utc;
                                state.packages.insert(
                                    "gem".to_string(),
                                    crate::sync::state::PackageState {
                                        last_sync: Utc::now(),
                                        hash,
                                    },
                                );
                                changes_made = true;
                            }
                        } else {
                            Output::info("    No changes");
                        }
                    }
                    Err(e) => {
                        Output::warning(&format!("Failed to export gem manifest: {}", e));
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

        // Check and push team repo changes (if write access enabled)
        if !dry_run {
            if let Some(team) = &config.team {
                if team.enabled && !team.read_only {
                    let team_sync_dir = Config::team_sync_dir()?;
                    if team_sync_dir.exists() {
                        let team_git = GitBackend::open(&team_sync_dir)?;

                        // Check for uncommitted changes in team repo
                        if team_git.has_changes()? {
                            println!();
                            Output::info("Detected changes in team repository");

                            // Commit and push team changes
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

    async fn config(&self, action: &ConfigAction) -> Result<()> {
        use crate::cli::Output;
        use crate::config::Config;

        match action {
            ConfigAction::Get { key } => {
                let config = Config::load()?;
                let config_toml = toml::to_string_pretty(&config)?;
                let value = toml::from_str::<toml::Value>(&config_toml)?;

                // Parse nested key (e.g., "project_configs.enabled")
                let keys: Vec<&str> = key.split('.').collect();
                let mut current = &value;

                for k in &keys {
                    match current.get(k) {
                        Some(v) => current = v,
                        None => {
                            Output::error(&format!("Key '{}' not found in config", key));
                            return Ok(());
                        }
                    }
                }

                // Pretty print the value
                match current {
                    toml::Value::String(s) => println!("{}", s),
                    toml::Value::Integer(i) => println!("{}", i),
                    toml::Value::Float(f) => println!("{}", f),
                    toml::Value::Boolean(b) => println!("{}", b),
                    toml::Value::Array(arr) => {
                        println!("[");
                        for item in arr {
                            println!("  {},", toml::to_string(item)?.trim());
                        }
                        println!("]");
                    }
                    toml::Value::Table(_) => {
                        println!("{}", toml::to_string_pretty(current)?);
                    }
                    _ => println!("{:?}", current),
                }

                Ok(())
            }

            ConfigAction::Set { key, value } => {
                let mut config = Config::load()?;
                let mut config_toml = toml::to_string_pretty(&config)?;
                let mut toml_value = toml::from_str::<toml::Value>(&config_toml)?;

                // Parse nested key (e.g., "project_configs.enabled")
                let keys: Vec<&str> = key.split('.').collect();

                // Navigate to the parent of the target key
                let mut current = &mut toml_value;
                for k in &keys[..keys.len() - 1] {
                    match current.get_mut(k) {
                        Some(v) => current = v,
                        None => {
                            Output::error(&format!("Key path '{}' not found in config", key));
                            return Ok(());
                        }
                    }
                }

                // Set the value
                let last_key = keys[keys.len() - 1];
                let table = match current.as_table_mut() {
                    Some(t) => t,
                    None => {
                        Output::error(&format!("Cannot set value at '{}'", key));
                        return Ok(());
                    }
                };

                // Parse the value string into appropriate TOML type
                let new_value: toml::Value = if value == "true" {
                    toml::Value::Boolean(true)
                } else if value == "false" {
                    toml::Value::Boolean(false)
                } else if let Ok(i) = value.parse::<i64>() {
                    toml::Value::Integer(i)
                } else if let Ok(f) = value.parse::<f64>() {
                    toml::Value::Float(f)
                } else if value.starts_with('[') && value.ends_with(']') {
                    // Array value - parse as TOML
                    match toml::from_str(value) {
                        Ok(v) => v,
                        Err(e) => {
                            Output::error(&format!("Failed to parse array: {}", e));
                            return Ok(());
                        }
                    }
                } else {
                    toml::Value::String(value.clone())
                };

                table.insert(last_key.to_string(), new_value);

                // Convert back to config and save
                config_toml = toml::to_string_pretty(&toml_value)?;
                config = toml::from_str(&config_toml)?;
                config.save()?;

                Output::success(&format!("Set {} = {}", key, value));
                Ok(())
            }

            ConfigAction::Edit => {
                let config_path = Config::config_path()?;

                // Get editor from environment or use default
                let editor = std::env::var("EDITOR").unwrap_or_else(|_| {
                    if cfg!(target_os = "macos") {
                        "nano".to_string()
                    } else {
                        "vi".to_string()
                    }
                });

                Output::info(&format!("Opening config in {}...", editor));
                Output::info(&format!("File: {}", config_path.display()));

                // Open editor
                let status = std::process::Command::new(&editor)
                    .arg(&config_path)
                    .status()?;

                if status.success() {
                    // Validate the config by trying to load it
                    match Config::load() {
                        Ok(_) => {
                            Output::success("Config updated successfully");
                        }
                        Err(e) => {
                            Output::error(&format!("Config validation failed: {}", e));
                            Output::warning("Your changes were saved but contain errors");
                        }
                    }
                } else {
                    Output::warning("Editor exited with error");
                }

                Ok(())
            }
        }
    }

    async fn team(&self, action: &TeamAction) -> Result<()> {
        use crate::cli::{Output, Prompt};
        use crate::config::{Config, TeamConfig};
        use crate::sync::GitBackend;

        match action {
            TeamAction::Add {
                url,
                no_auto_inject,
            } => {
                // Load config
                let mut config = Config::load()?;

                if config.team.is_some() {
                    Output::warning("Team sync is already configured");
                    if !Prompt::confirm("Replace existing team configuration?", false)? {
                        return Ok(());
                    }
                }

                Output::info(&format!("Adding team sync: {}", url));

                // Clone team repository
                let team_sync_dir = Config::team_sync_dir()?;
                if team_sync_dir.exists() {
                    std::fs::remove_dir_all(&team_sync_dir)?;
                }

                Output::info("Cloning team repository...");
                GitBackend::clone(url, &team_sync_dir)?;
                Output::success("Team repository cloned successfully");

                // Security check: Scan for secrets in team repo
                Output::info("Scanning team configs for secrets...");
                let dotfiles_dir = team_sync_dir.join("dotfiles");
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
                                if let Ok(findings) = crate::security::scan_for_secrets(&file_path)
                                {
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
                    Output::warning(
                        "Team repositories should only contain non-sensitive shared configs.",
                    );
                    Output::info(
                        "For sensitive data, use a secrets manager (1Password, Vault, etc.)",
                    );
                    println!();

                    if !Prompt::confirm("Continue anyway?", false)? {
                        // Clean up
                        std::fs::remove_dir_all(&team_sync_dir)?;
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
                let team_git = GitBackend::open(&team_sync_dir)?;
                let has_write = team_git.has_write_access().unwrap_or(false);

                let read_only = if has_write {
                    println!();
                    Output::success("You have write access to this repository!");
                    Output::info(
                        "As a team admin/contributor, you can push updates to team configs.",
                    );
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
                let auto_inject = if *no_auto_inject {
                    false
                } else if !team_files.is_empty() {
                    Prompt::confirm("Auto-inject source lines to your personal configs?", true)?
                } else {
                    false
                };

                // Perform auto-injection if requested
                if auto_inject {
                    self.inject_team_sources(&team_files).await?;
                } else if !team_files.is_empty() {
                    println!();
                    Output::info("To use team configs, add these lines to your dotfiles:");
                    self.show_injection_instructions(&team_files);
                }

                // Save config
                config.team = Some(TeamConfig {
                    enabled: true,
                    url: url.clone(),
                    auto_inject,
                    read_only,
                });
                config.save()?;

                Output::success("Team sync added successfully!");
                Ok(())
            }

            TeamAction::Remove => {
                let mut config = Config::load()?;

                if config.team.is_none() {
                    Output::warning("Team sync is not configured");
                    return Ok(());
                }

                if !Prompt::confirm("Remove team sync configuration?", false)? {
                    return Ok(());
                }

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

            TeamAction::Enable => {
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

            TeamAction::Disable => {
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

            TeamAction::Status => {
                let config = Config::load()?;

                println!();
                match &config.team {
                    Some(team) => {
                        use comfy_table::{
                            presets::UTF8_FULL, Attribute, Cell, Color, ContentArrangement, Table,
                        };

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
        }
    }

    async fn inject_team_sources(&self, team_files: &[String]) -> Result<()> {
        use crate::cli::Output;

        let home =
            home::home_dir().ok_or_else(|| anyhow::anyhow!("Could not find home directory"))?;
        let team_sync_dir = Config::team_sync_dir()?;

        for file in team_files {
            // Determine the personal config file and source line
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
                // For gitconfig, we use [include] section
                let include_line = format!(
                    "[include]\n    path = {}/dotfiles/team.gitconfig",
                    team_sync_dir.display()
                );
                (home.join(".gitconfig"), include_line)
            } else if file.starts_with("team.")
                && (file.ends_with("rc") || file.ends_with("profile"))
            {
                // Generic shell config
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
                // Skip files we don't know how to inject
                continue;
            };

            // Check if file exists
            if !personal_file.exists() {
                Output::warning(&format!(
                    "  {} not found, skipping",
                    personal_file.display()
                ));
                continue;
            }

            // Read current content
            let content = std::fs::read_to_string(&personal_file)?;

            // Check if already sourced
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

            // Add source line
            let new_content = if file == "team.gitconfig" {
                // For gitconfig, prepend the include section
                format!("{}\n\n{}", source_line, content)
            } else {
                // For shell configs, add near the top (after any shebang)
                if content.starts_with("#!") {
                    let mut lines: Vec<&str> = content.lines().collect();
                    lines.insert(1, "");
                    lines.insert(2, &source_line);
                    lines.join("\n")
                } else {
                    format!("{}\n\n{}", source_line, content)
                }
            };

            std::fs::write(&personal_file, new_content)?;
            Output::success(&format!(
                "  Added source line to {}",
                file.replace("team.", ".")
            ));
        }

        Ok(())
    }

    fn show_injection_instructions(&self, team_files: &[String]) {
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

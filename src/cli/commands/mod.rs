mod collab;
mod config;
mod daemon;
mod diff;
mod identity;
mod ignore;
mod init;
mod machines;
mod packages;
mod resolve;
mod restore;
mod status;
pub mod sync;
mod team;
mod unlock;
mod upgrade;

use anyhow::Result;
use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "tether")]
#[command(about = "Sync your dev environment across machines", long_about = None)]
#[command(version)]
pub struct Cli {
    /// Skip confirmation prompts (non-interactive mode)
    #[arg(short = 'y', long, global = true)]
    pub yes: bool,

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

        /// Team-only mode: skip personal dotfiles/packages, only use team sync
        #[arg(long)]
        team_only: bool,
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

    /// Resolve file conflicts
    Resolve {
        /// Specific file to resolve (resolves all if not specified)
        file: Option<String>,
    },

    /// Unlock encryption key with passphrase
    Unlock,

    /// Clear cached encryption key
    Lock,

    /// Upgrade all installed packages
    Upgrade,

    /// List and manage installed packages
    Packages {
        /// List packages without interactive selection
        #[arg(long)]
        list: bool,
    },

    /// Restore files from backup
    Restore {
        #[command(subcommand)]
        action: RestoreAction,
    },

    /// Manage age identity for team secrets
    Identity {
        #[command(subcommand)]
        action: IdentityAction,
    },

    /// Manage collaborator-based project secret sharing
    Collab {
        #[command(subcommand)]
        action: CollabAction,
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
    /// Install launchd service (auto-start on login)
    Install,
    /// Uninstall launchd service
    Uninstall,
    /// Internal daemon runner
    #[command(hide = true)]
    Run,
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
    /// Add secret scanning ignore pattern
    Add { pattern: String },
    /// List secret scanning ignore patterns
    List,
    /// Remove secret scanning ignore pattern
    Remove { pattern: String },
    /// Ignore a dotfile on this machine (won't be overwritten during sync)
    Dotfile { file: String },
    /// Ignore a project config on this machine
    Project {
        /// Project identifier (repo name or path)
        project: String,
        /// Config file path relative to project root
        path: String,
    },
    /// List files ignored on this machine
    SyncList,
    /// Unignore a file on this machine
    SyncRemove {
        /// File to unignore (dotfile name or "project:path")
        file: String,
    },
}

#[derive(Subcommand)]
pub enum ConfigAction {
    /// Get config value
    Get { key: String },
    /// Set config value
    Set { key: String, value: String },
    /// Open config in editor
    Edit,
    /// Interactive UI for managing files, folders, and patterns
    Dotfiles,
    /// Manage feature toggles
    Features {
        #[command(subcommand)]
        action: Option<FeaturesAction>,
    },
}

#[derive(Subcommand)]
pub enum FeaturesAction {
    /// Enable a feature
    Enable {
        /// Feature name
        feature: String,
    },
    /// Disable a feature
    Disable {
        /// Feature name
        feature: String,
    },
}

#[derive(Subcommand)]
pub enum RestoreAction {
    /// List available backups
    List,
    /// Restore a file from backup (interactive if no args)
    File {
        /// Backup timestamp (e.g., 2024-01-15T10-30-00)
        #[arg(long)]
        from: Option<String>,
        /// File to restore (e.g., dotfiles/.zshrc)
        file: Option<String>,
    },
}

#[derive(Subcommand)]
pub enum IdentityAction {
    /// Generate a new age identity
    Init,
    /// Show your public key
    Show,
    /// Unlock identity with passphrase
    Unlock,
    /// Lock identity (clear cached key)
    Lock,
    /// Reset identity (generate new, destroys old)
    Reset,
}

#[derive(Subcommand)]
pub enum CollabAction {
    /// Initialize a new collab for the current project
    Init {
        /// Project path (defaults to current directory)
        #[arg(long)]
        project: Option<String>,
    },
    /// Join an existing collab
    Join {
        /// Collab sync repo URL
        url: String,
    },
    /// Add a secret file to the collab
    Add {
        /// File to add (e.g., .env)
        file: String,
        /// Project path (defaults to current directory)
        #[arg(long)]
        project: Option<String>,
    },
    /// Refresh collaborators from GitHub and re-encrypt secrets
    Refresh {
        /// Project path (defaults to current directory)
        #[arg(long)]
        project: Option<String>,
    },
    /// List all collabs
    List,
    /// Add another project to an existing collab
    AddProject {
        /// Project path to add
        project: String,
    },
    /// Remove a collab
    Remove {
        /// Collab name (interactive if not specified)
        name: Option<String>,
    },
}

#[derive(Subcommand)]
pub enum TeamAction {
    /// Interactive team setup wizard
    Setup,
    /// Add team sync repository
    Add {
        /// Team repository URL
        url: String,
        /// Custom team name (defaults to org/owner from URL)
        #[arg(long)]
        name: Option<String>,
        /// Skip auto-injection of source lines
        #[arg(long)]
        no_auto_inject: bool,
    },
    /// Switch active team
    Switch {
        /// Team name to switch to
        name: String,
    },
    /// List all teams
    List,
    /// Remove team sync
    Remove {
        /// Team name to remove (defaults to active team)
        name: Option<String>,
    },
    /// Enable team sync
    Enable,
    /// Disable team sync
    Disable,
    /// Show team sync status
    Status,
    /// Manage allowed organizations for team repos
    Orgs {
        #[command(subcommand)]
        action: OrgAction,
    },
    /// Manage team secrets (encrypted with age)
    Secrets {
        #[command(subcommand)]
        action: SecretsAction,
    },
    /// Manage team files and sync preferences
    Files {
        #[command(subcommand)]
        action: FilesAction,
    },
    /// Manage team project secrets
    Projects {
        #[command(subcommand)]
        action: ProjectsAction,
    },
}

#[derive(Subcommand)]
pub enum OrgAction {
    /// Add allowed organization
    Add {
        /// GitHub organization name
        org: String,
    },
    /// List allowed organizations
    List,
    /// Remove allowed organization
    Remove {
        /// GitHub organization name
        org: String,
    },
}

#[derive(Subcommand)]
pub enum SecretsAction {
    /// Add a recipient's public key to the team
    AddRecipient {
        /// age public key or path to .pub file
        key: String,
        /// Name for this recipient (defaults to username)
        #[arg(long)]
        name: Option<String>,
    },
    /// List team recipients
    ListRecipients,
    /// Remove a recipient from the team
    RemoveRecipient {
        /// Recipient name
        name: String,
    },
    /// Add or update a secret
    Set {
        /// Secret name (e.g., "GITHUB_TOKEN")
        name: String,
        /// Secret value (prompts if not provided)
        #[arg(long)]
        value: Option<String>,
    },
    /// Get a secret value
    Get {
        /// Secret name
        name: String,
    },
    /// List all secrets
    List,
    /// Remove a secret
    Remove {
        /// Secret name
        name: String,
    },
}

#[derive(Subcommand)]
pub enum FilesAction {
    /// List synced team files
    List,
    /// Show local patterns (files never synced)
    LocalPatterns,
    /// Reset file to team version (clobber local changes)
    Reset {
        /// File to reset
        file: Option<String>,
        /// Reset all files
        #[arg(long)]
        all: bool,
    },
    /// Promote local file to team repository
    Promote {
        /// File to promote
        file: String,
    },
    /// Mark file as personal (skip team sync)
    Ignore {
        /// File to ignore
        file: String,
    },
    /// Unmark file as personal (resume team sync)
    Unignore {
        /// File to unignore
        file: String,
    },
    /// Show diff between local and team version
    Diff {
        /// File to diff (all if not specified)
        file: Option<String>,
    },
}

#[derive(Subcommand)]
pub enum ProjectsAction {
    /// Add a project secret to the team repo
    Add {
        /// File to add (e.g., .env)
        file: String,
        /// Project path (defaults to current directory)
        #[arg(long)]
        project: Option<String>,
    },
    /// List team project secrets
    List,
    /// Remove a project secret
    Remove {
        /// File to remove
        file: String,
        /// Project (normalized URL like github.com/org/repo)
        #[arg(long)]
        project: Option<String>,
    },
    /// Remove personal project secrets that are now team-owned
    PurgePersonal {
        /// Also purge from git history
        #[arg(long)]
        history: bool,
    },
    /// Migrate personal project secrets to team repo
    Migrate,
}

impl Cli {
    pub async fn run(&self) -> Result<()> {
        match &self.command {
            Commands::Init {
                repo,
                no_daemon,
                team_only,
            } => init::run(repo.as_deref(), *no_daemon, *team_only).await,
            Commands::Sync { dry_run, force } => sync::run(*dry_run, *force).await,
            Commands::Status => status::run().await,
            Commands::Diff { machine } => diff::run(machine.as_deref()).await,
            Commands::Daemon { action } => match action {
                DaemonAction::Start => daemon::start().await,
                DaemonAction::Stop => daemon::stop().await,
                DaemonAction::Restart => daemon::restart().await,
                DaemonAction::Logs => daemon::logs().await,
                DaemonAction::Install => daemon::install().await,
                DaemonAction::Uninstall => daemon::uninstall().await,
                DaemonAction::Run => daemon::run_daemon().await,
            },
            Commands::Machines { action } => match action {
                MachineAction::List => machines::list().await,
                MachineAction::Rename { old, new } => machines::rename(old, new).await,
                MachineAction::Remove { name } => machines::remove(name).await,
            },
            Commands::Ignore { action } => match action {
                IgnoreAction::Add { pattern } => ignore::add(pattern).await,
                IgnoreAction::List => ignore::list().await,
                IgnoreAction::Remove { pattern } => ignore::remove(pattern).await,
                IgnoreAction::Dotfile { file } => ignore::ignore_dotfile(file).await,
                IgnoreAction::Project { project, path } => {
                    ignore::ignore_project(project, path).await
                }
                IgnoreAction::SyncList => ignore::sync_list().await,
                IgnoreAction::SyncRemove { file } => ignore::sync_remove(file).await,
            },
            Commands::Config { action } => match action {
                ConfigAction::Get { key } => config::get(key).await,
                ConfigAction::Set { key, value } => config::set(key, value).await,
                ConfigAction::Edit => config::edit().await,
                ConfigAction::Dotfiles => config::dotfiles().await,
                ConfigAction::Features { action } => match action {
                    None => config::features_list().await,
                    Some(FeaturesAction::Enable { feature }) => {
                        config::features_enable(feature).await
                    }
                    Some(FeaturesAction::Disable { feature }) => {
                        config::features_disable(feature).await
                    }
                },
            },
            Commands::Team { action } => match action {
                TeamAction::Setup => team::setup().await,
                TeamAction::Add {
                    url,
                    name,
                    no_auto_inject,
                } => team::add(url, name.as_deref(), *no_auto_inject).await,
                TeamAction::Switch { name } => team::switch(name).await,
                TeamAction::List => team::list().await,
                TeamAction::Remove { name } => team::remove(name.as_deref()).await,
                TeamAction::Enable => team::enable().await,
                TeamAction::Disable => team::disable().await,
                TeamAction::Status => team::status().await,
                TeamAction::Orgs { action } => match action {
                    OrgAction::Add { org } => team::orgs_add(org, self.yes).await,
                    OrgAction::List => team::orgs_list().await,
                    OrgAction::Remove { org } => team::orgs_remove(org).await,
                },
                TeamAction::Secrets { action } => match action {
                    SecretsAction::AddRecipient { key, name } => {
                        team::secrets_add_recipient(key, name.as_deref()).await
                    }
                    SecretsAction::ListRecipients => team::secrets_list_recipients().await,
                    SecretsAction::RemoveRecipient { name } => {
                        team::secrets_remove_recipient(name).await
                    }
                    SecretsAction::Set { name, value } => {
                        team::secrets_set(name, value.as_deref()).await
                    }
                    SecretsAction::Get { name } => team::secrets_get(name).await,
                    SecretsAction::List => team::secrets_list().await,
                    SecretsAction::Remove { name } => team::secrets_remove(name).await,
                },
                TeamAction::Files { action } => match action {
                    FilesAction::List => team::files_list().await,
                    FilesAction::LocalPatterns => team::files_local_patterns().await,
                    FilesAction::Reset { file, all } => {
                        team::files_reset(file.as_deref(), *all).await
                    }
                    FilesAction::Promote { file } => team::files_promote(file).await,
                    FilesAction::Ignore { file } => team::files_ignore(file).await,
                    FilesAction::Unignore { file } => team::files_unignore(file).await,
                    FilesAction::Diff { file } => team::files_diff(file.as_deref()).await,
                },
                TeamAction::Projects { action } => match action {
                    ProjectsAction::Add { file, project } => {
                        team::projects_add(file, project.as_deref()).await
                    }
                    ProjectsAction::List => team::projects_list().await,
                    ProjectsAction::Remove { file, project } => {
                        team::projects_remove(file, project.as_deref()).await
                    }
                    ProjectsAction::PurgePersonal { history } => {
                        team::projects_purge_personal(*history, self.yes).await
                    }
                    ProjectsAction::Migrate => team::projects_migrate(self.yes).await,
                },
            },
            Commands::Resolve { file } => resolve::run(file.as_deref()).await,
            Commands::Unlock => unlock::run().await,
            Commands::Lock => unlock::lock().await,
            Commands::Upgrade => upgrade::run().await,
            Commands::Packages { list } => packages::run(*list).await,
            Commands::Restore { action } => match action {
                RestoreAction::List => restore::list_cmd().await,
                RestoreAction::File { from, file } => {
                    restore::run(from.as_deref(), file.as_deref()).await
                }
            },
            Commands::Identity { action } => match action {
                IdentityAction::Init => identity::init().await,
                IdentityAction::Show => identity::show().await,
                IdentityAction::Unlock => identity::unlock().await,
                IdentityAction::Lock => identity::lock().await,
                IdentityAction::Reset => identity::reset().await,
            },
            Commands::Collab { action } => match action {
                CollabAction::Init { project } => collab::init(project.as_deref()).await,
                CollabAction::Join { url } => collab::join(url).await,
                CollabAction::Add { file, project } => collab::add(file, project.as_deref()).await,
                CollabAction::Refresh { project } => collab::refresh(project.as_deref()).await,
                CollabAction::List => collab::list().await,
                CollabAction::AddProject { project } => collab::add_project(project).await,
                CollabAction::Remove { name } => collab::remove(name.as_deref()).await,
            },
        }
    }
}

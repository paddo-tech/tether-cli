mod config;
mod daemon;
mod diff;
mod identity;
mod ignore;
mod init;
mod machines;
mod resolve;
mod restore;
mod status;
mod sync;
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
pub enum TeamAction {
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

impl Cli {
    pub async fn run(&self) -> Result<()> {
        match &self.command {
            Commands::Init { repo, no_daemon } => init::run(repo.as_deref(), *no_daemon).await,
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
            },
            Commands::Team { action } => match action {
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
                    OrgAction::Add { org } => team::orgs_add(org).await,
                    OrgAction::List => team::orgs_list().await,
                    OrgAction::Remove { org } => team::orgs_remove(org).await,
                },
            },
            Commands::Resolve { file } => resolve::run(file.as_deref()).await,
            Commands::Unlock => unlock::run().await,
            Commands::Lock => unlock::lock().await,
            Commands::Upgrade => upgrade::run().await,
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
        }
    }
}

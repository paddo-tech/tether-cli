mod collab;
mod config;
mod daemon;
mod diff;
mod history;
mod identity;
mod ignore;
mod init;
pub mod machines;
mod packages;
mod resolve;
mod restore;
mod rollback;
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
    /// Confirm without asking and take each question's default answer. Approvals and key
    /// trust still need --expect or --fingerprint
    #[arg(short = 'y', long, global = true)]
    pub yes: bool,

    #[command(subcommand)]
    pub command: Option<Commands>,
}

#[derive(Subcommand)]
pub enum Commands {
    /// Interactive dashboard
    Dashboard,

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

        /// Re-prompt for previously dismissed file imports
        #[arg(long)]
        rediscover: bool,
    },

    /// Show current sync status
    Status {
        /// Print JSON (experimental in 2.0: field names may still change)
        #[arg(long)]
        json: bool,
    },

    /// Show differences between machines
    Diff {
        /// Compare with this machine (id or hostname)
        #[arg(long)]
        machine: Option<String>,
    },

    /// Control the background daemon
    Daemon {
        #[command(subcommand)]
        action: DaemonAction,
    },

    /// List, show, trust and remove machines, and manage profiles
    Machines {
        #[command(subcommand)]
        action: MachineAction,
    },

    /// Manage what Tether ignores: secret scanning patterns, and files this machine keeps
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

    /// Upgrade installed packages to the newest release the release-age limit allows. Never
    /// downgrades. Asks first in a terminal; without one, needs -y
    Upgrade {
        /// List what would change without upgrading
        #[arg(long)]
        dry_run: bool,
    },

    /// List, share and uninstall packages, and decide on the inbox. Without a subcommand,
    /// lists the installed packages
    ///
    /// A package id is manager:name, such as npm:typescript. Managers: brew_formulae (or
    /// brew), brew_casks (or cask), brew_taps, npm, pnpm, bun, gem, uv.
    Packages {
        /// Same as `tether packages list`
        #[arg(long, hide = true)]
        list: bool,

        #[command(subcommand)]
        action: Option<PackagesAction>,
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

    /// Show file change history from sync repo
    History {
        /// Dotfile path (e.g., .zshrc)
        file: String,
        /// Maximum number of entries to show
        #[arg(short, long, default_value = "20")]
        limit: usize,
    },

    /// Roll back synced state to an earlier point
    Rollback {
        #[command(subcommand)]
        action: RollbackAction,
    },
}

#[derive(Subcommand)]
pub enum PackagesAction {
    /// List installed packages by manager, with the profiles each belongs to
    List {
        /// Print JSON (experimental in 2.0: field names may still change)
        #[arg(long)]
        json: bool,
        /// List instead the packages of other profiles that trusted machines list and this
        /// machine does not install
        #[arg(long)]
        other_profiles: bool,
    },
    /// List the inbox: packages and machine keys that wait for your approval
    Inbox {
        /// Print JSON (experimental in 2.0: field names may still change)
        #[arg(long)]
        json: bool,
    },
    /// Approve an inbox item: install the package, or trust the machine key
    Approve {
        /// Item id (manager:name) or a package name. machine:<id> trusts that machine's key,
        /// as 'tether machines trust' does
        #[arg(required_unless_present = "all", conflicts_with = "all")]
        id: Option<String>,
        /// Same as --expect
        #[arg(conflicts_with = "expect", hide = true)]
        expected: Option<String>,
        /// The version, Homebrew tap or key fingerprint you reviewed, as 'packages inbox'
        /// shows it. Required without a terminal
        #[arg(long, value_name = "VERSION|TAP|KEY")]
        expect: Option<String>,
        /// Approve a package whose source record fails its signature, without a terminal.
        /// Needs --expect too. In a terminal, Tether asks instead
        #[arg(long, conflicts_with = "all")]
        allow_signature_failed: bool,
        /// Approve and install every package in the inbox, except packages OSV lists as
        /// malicious, packages whose record fails its signature, packages without a version
        /// or tap to name, and machine keys. Asks first; without a terminal, needs -y. This
        /// is the only command where -y approves inbox items
        #[arg(long)]
        all: bool,
        /// With --all, only the items from this machine (id or hostname)
        #[arg(long, requires = "all", value_name = "MACHINE")]
        from: Option<String>,
    },
    /// Reject an inbox item, so syncs stop offering that version, tap or key
    Reject {
        /// Item id (manager:name) or a package name
        id: String,
        /// Same as --expect
        #[arg(conflicts_with = "expect", hide = true)]
        expected: Option<String>,
        /// The version, Homebrew tap or key fingerprint you reviewed, as 'packages inbox'
        /// shows it. Required without a terminal
        #[arg(long, value_name = "VERSION|TAP|KEY")]
        expect: Option<String>,
    },
    /// Install a package that another machine lists, as the dashboard's Import does. OSV
    /// checks the release first. A package that waits in the inbox needs 'approve' instead
    Install {
        /// Package as manager:name, such as npm:typescript or cask:zoom
        id: String,
    },
    /// Add profiles to a package's members, so machines in those profiles install it
    Share {
        /// Package as manager:name, such as npm:typescript or cask:zoom
        id: String,
        /// Profiles to add, separated by commas
        #[arg(long, value_delimiter = ',', required = true, value_name = "PROFILES")]
        to: Vec<String>,
    },
    /// Take profiles out of a package's members, so machines in those profiles stop
    /// installing it. Nothing is uninstalled; machines keep any copy they have
    Unshare {
        /// Package as manager:name, such as npm:typescript or cask:zoom
        id: String,
        /// Profiles to take out, separated by commas
        #[arg(long, value_delimiter = ',', required = true, value_name = "PROFILES")]
        from: Vec<String>,
    },
    /// Uninstall a package here and take this machine's profile out of its members. Without
    /// a package, pick packages to uninstall in a terminal
    #[command(visible_alias = "remove")]
    Uninstall {
        /// Package as manager:name, such as npm:typescript or cask:zoom
        id: Option<String>,
    },
}

#[derive(Subcommand)]
pub enum RollbackAction {
    /// Roll back a package manager's installed set to a manifest commit. Homebrew is not
    /// supported
    Packages {
        /// Manager key (npm, pnpm, bun, gem, uv). brew is not supported
        manager: String,
        /// Manifest commit hash to roll back to
        commit: String,
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
    /// Show whether the daemon runs, whether its login service is installed, and the last
    /// sync and upgrade
    Status,
    /// Print the end of the daemon log
    Logs {
        /// Keep printing lines as the daemon writes them
        #[arg(short, long)]
        follow: bool,
        /// How many lines to print from the end
        #[arg(short = 'n', long, default_value = "50")]
        lines: usize,
    },
    /// Install the login service (launchd on macOS, systemd on Linux)
    Install,
    /// Uninstall the login service
    Uninstall,
    /// Internal daemon runner
    #[command(hide = true)]
    Run,
}

#[derive(Subcommand)]
pub enum MachineAction {
    /// List all machines
    List {
        /// Print JSON (experimental in 2.0: field names may still change)
        #[arg(long)]
        json: bool,
    },
    /// Show one machine: profile, versions, record status, and its full key fingerprint and
    /// trust
    Show {
        /// Machine id or hostname. Without it, this machine
        machine: Option<String>,
    },
    /// Rename this machine: 'tether machines rename <NEW>'. The form '<OLD> <NEW>' still
    /// works in 2.0 and is deprecated
    Rename {
        /// The new name, or this machine's current name in the deprecated two-name form
        #[arg(value_name = "NEW")]
        name: String,
        /// The new name, in the deprecated form
        #[arg(hide = true)]
        new: Option<String>,
    },
    /// Remove a machine from sync. With -y, Tether removes it without asking and prints
    /// what it removed
    Remove {
        /// Machine id or hostname
        machine: String,
    },
    /// Trust the signing key a machine published, so its package changes install on their own.
    /// Without a fingerprint, Tether shows the current one and asks in a terminal
    Trust {
        /// Machine id or hostname
        machine: String,
        /// Same as --fingerprint
        #[arg(conflicts_with = "fingerprint_flag", hide = true)]
        fingerprint: Option<String>,
        /// Fingerprint you checked on that machine with 'tether machines show'
        /// (`SHA256:...`). Required without a terminal
        #[arg(
            long = "fingerprint",
            id = "fingerprint_flag",
            value_name = "SHA256:..."
        )]
        fingerprint_flag: Option<String>,
    },
    /// Stop trusting a machine's signing key
    Untrust {
        /// Machine id or hostname
        machine: String,
    },
    /// Manage machine profile assignment
    Profile {
        #[command(subcommand)]
        action: MachineProfileAction,
    },
}

#[derive(Subcommand)]
pub enum MachineProfileAction {
    /// Assign a profile to this machine. Each machine sets only its own profile
    Set {
        /// Profile name (must exist in config)
        profile: String,
    },
    /// Remove profile assignment from this machine
    Unset,
    /// Create a profile. Asks about each dotfile, folder and package manager in a terminal;
    /// -y takes every default, and --from copies a profile without asking
    Create {
        /// Profile name
        #[arg(value_name = "PROFILE")]
        name: String,
        /// Copy this existing profile
        #[arg(long, value_name = "PROFILE")]
        from: Option<String>,
        /// Package managers of the profile, separated by commas (brew, npm, pnpm, bun, gem, uv)
        #[arg(long, value_delimiter = ',', value_name = "MANAGERS")]
        managers: Option<Vec<String>>,
    },
    /// Edit an existing profile
    Edit {
        /// Profile name
        #[arg(value_name = "PROFILE")]
        name: String,
    },
    /// List all profiles
    List,
}

#[derive(Subcommand)]
pub enum IgnoreAction {
    /// Patterns the secret scanner skips
    Secrets {
        #[command(subcommand)]
        action: IgnoreSecretsAction,
    },
    /// Files this machine keeps: a sync does not overwrite them
    Files {
        #[command(subcommand)]
        action: IgnoreFilesAction,
    },
    #[command(hide = true)]
    Add { pattern: String },
    #[command(hide = true)]
    List,
    #[command(hide = true)]
    Remove { pattern: String },
    #[command(hide = true)]
    Dotfile { file: String },
    #[command(hide = true)]
    Project { project: String, path: String },
    #[command(hide = true)]
    SyncList,
    #[command(hide = true)]
    SyncRemove { file: String },
}

#[derive(Subcommand)]
pub enum IgnoreSecretsAction {
    /// Add a pattern the secret scanner skips
    Add { pattern: String },
    /// List the patterns
    List,
    /// Remove a pattern
    Remove { pattern: String },
}

#[derive(Subcommand)]
pub enum IgnoreFilesAction {
    /// Keep a dotfile on this machine, such as .zshrc
    Add { file: String },
    /// Keep a project config file on this machine
    Project {
        /// Project identifier (repo name or path)
        project: String,
        /// Config file path relative to project root
        path: String,
    },
    /// List the files this machine keeps
    List,
    /// Sync a kept file again
    Remove {
        /// Dotfile name, or project:path
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
    /// Restore a dotfile from git history
    Git {
        /// Dotfile path (e.g., .zshrc)
        file: String,
        /// Commit hash (interactive picker if omitted)
        #[arg(long)]
        commit: Option<String>,
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
    pub fn is_daemon_run(&self) -> bool {
        matches!(
            self.command,
            Some(Commands::Daemon {
                action: DaemonAction::Run
            })
        )
    }

    pub async fn run(&self) -> Result<()> {
        crate::cli::Prompt::set_assume_yes(self.yes);
        crate::cli::Output::set_json(matches!(
            &self.command,
            Some(
                Commands::Status { json: true }
                    | Commands::Machines {
                        action: MachineAction::List { json: true }
                    }
                    | Commands::Packages {
                        action: Some(
                            PackagesAction::List { json: true, .. }
                                | PackagesAction::Inbox { json: true }
                        ),
                        ..
                    }
            )
        ));
        match &self.command {
            None | Some(Commands::Dashboard) => {
                tokio::task::spawn_blocking(crate::dashboard::run).await?
            }
            Some(cmd) => self.run_command(cmd).await,
        }
    }

    async fn run_command(&self, command: &Commands) -> Result<()> {
        match command {
            Commands::Dashboard => unreachable!(),
            Commands::Init {
                repo,
                no_daemon,
                team_only,
            } => init::run(repo.as_deref(), *no_daemon, *team_only).await,
            Commands::Sync {
                dry_run,
                force,
                rediscover,
            } => sync::run(*dry_run, *force, *rediscover).await,
            Commands::Status { json } => status::run(*json).await,
            Commands::Diff { machine } => diff::run(machine.as_deref()).await,
            Commands::Daemon { action } => match action {
                DaemonAction::Start => daemon::start().await,
                DaemonAction::Stop => daemon::stop().await,
                DaemonAction::Restart => daemon::restart().await,
                DaemonAction::Status => daemon::status().await,
                DaemonAction::Logs { follow, lines } => daemon::logs(*lines, *follow).await,
                DaemonAction::Install => daemon::install().await,
                DaemonAction::Uninstall => daemon::uninstall().await,
                DaemonAction::Run => daemon::run_daemon().await,
            },
            Commands::Machines { action } => match action {
                MachineAction::List { json } => machines::list(*json).await,
                MachineAction::Show { machine } => machines::show(machine.as_deref()).await,
                MachineAction::Rename { name, new } => match new {
                    Some(new) => {
                        crate::cli::Output::warning(
                            "'tether machines rename <OLD> <NEW>' is deprecated. Run 'tether machines rename <NEW>'",
                        );
                        machines::rename(Some(name), new).await
                    }
                    None => machines::rename(None, name).await,
                },
                MachineAction::Remove { machine } => machines::remove(machine, self.yes).await,
                MachineAction::Trust {
                    machine,
                    fingerprint,
                    fingerprint_flag,
                } => {
                    let fingerprint = fingerprint.as_deref().or(fingerprint_flag.as_deref());
                    machines::trust(machine, fingerprint).await
                }
                MachineAction::Untrust { machine } => machines::untrust(machine).await,
                MachineAction::Profile { action } => match action {
                    MachineProfileAction::Set { profile } => machines::profile_set(profile).await,
                    MachineProfileAction::Unset => machines::profile_unset().await,
                    MachineProfileAction::Create {
                        name,
                        from,
                        managers,
                    } => machines::profile_create(name, from.as_deref(), managers.as_deref()).await,
                    MachineProfileAction::Edit { name } => machines::profile_edit(name).await,
                    MachineProfileAction::List => machines::profile_list().await,
                },
            },
            Commands::Ignore { action } => match action {
                IgnoreAction::Secrets {
                    action: IgnoreSecretsAction::Add { pattern },
                }
                | IgnoreAction::Add { pattern } => ignore::add(pattern).await,
                IgnoreAction::Secrets {
                    action: IgnoreSecretsAction::List,
                }
                | IgnoreAction::List => ignore::list().await,
                IgnoreAction::Secrets {
                    action: IgnoreSecretsAction::Remove { pattern },
                }
                | IgnoreAction::Remove { pattern } => ignore::remove(pattern).await,
                IgnoreAction::Files {
                    action: IgnoreFilesAction::Add { file },
                }
                | IgnoreAction::Dotfile { file } => ignore::ignore_dotfile(file).await,
                IgnoreAction::Files {
                    action: IgnoreFilesAction::Project { project, path },
                }
                | IgnoreAction::Project { project, path } => {
                    ignore::ignore_project(project, path).await
                }
                IgnoreAction::Files {
                    action: IgnoreFilesAction::List,
                }
                | IgnoreAction::SyncList => ignore::sync_list().await,
                IgnoreAction::Files {
                    action: IgnoreFilesAction::Remove { file },
                }
                | IgnoreAction::SyncRemove { file } => ignore::sync_remove(file).await,
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
            Commands::Upgrade { dry_run } => upgrade::run(*dry_run).await,
            Commands::Packages { list: _, action } => match action {
                None => packages::list(false, false).await,
                Some(PackagesAction::List {
                    json,
                    other_profiles,
                }) => packages::list(*json, *other_profiles).await,
                Some(PackagesAction::Inbox { json }) => packages::inbox_list(*json).await,
                Some(PackagesAction::Approve {
                    id,
                    expected,
                    expect,
                    allow_signature_failed,
                    all,
                    from,
                }) => match id {
                    _ if *all => packages::approve_all(from.as_deref()).await,
                    Some(id) => {
                        packages::approve(
                            id,
                            expected.as_deref().or(expect.as_deref()),
                            *allow_signature_failed,
                        )
                        .await
                    }
                    None => unreachable!("clap requires an id without --all"),
                },
                Some(PackagesAction::Reject {
                    id,
                    expected,
                    expect,
                }) => packages::reject(id, expected.as_deref().or(expect.as_deref())).await,
                Some(PackagesAction::Install { id }) => packages::install(id).await,
                Some(PackagesAction::Share { id, to }) => packages::share(id, to),
                Some(PackagesAction::Unshare { id, from }) => packages::unshare(id, from),
                Some(PackagesAction::Uninstall { id: Some(id) }) => packages::remove(id).await,
                Some(PackagesAction::Uninstall { id: None }) => packages::pick_uninstall().await,
            },
            Commands::Restore { action } => match action {
                RestoreAction::List => restore::list_cmd().await,
                RestoreAction::File { from, file } => {
                    restore::run(from.as_deref(), file.as_deref()).await
                }
                RestoreAction::Git { file, commit } => {
                    restore::git_restore(file, commit.as_deref()).await
                }
            },
            Commands::Identity { action } => match action {
                IdentityAction::Init => identity::init().await,
                IdentityAction::Show => identity::show().await,
                IdentityAction::Unlock => identity::unlock().await,
                IdentityAction::Lock => identity::lock().await,
                IdentityAction::Reset => identity::reset().await,
            },
            Commands::History { file, limit } => history::run(file, *limit).await,
            Commands::Rollback { action } => match action {
                RollbackAction::Packages { manager, commit } => {
                    rollback::packages(manager, commit, self.yes).await
                }
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

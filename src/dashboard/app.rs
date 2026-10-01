use super::components::config::ConfigTabState;
use super::components::confirm::Confirm;
use super::components::file_import::FileImport;
use super::components::files::{self, FilesTabState};
use super::components::machines::MachinesTabState;
use super::components::packages::{self, PackagesTabState};
use super::components::pkg_import::PkgImport;
use super::components::profile_picker::ProfilePicker;
use super::msg::Cmd;
use super::repo;
use super::state::DashboardState;
use super::theme::Theme;
use std::collections::HashMap;
use std::time::Instant;

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Tab {
    Overview,
    Files,
    Packages,
    Machines,
    Config,
}

impl Tab {
    pub fn title(&self) -> &str {
        match self {
            Tab::Overview => "Overview",
            Tab::Files => "Files",
            Tab::Packages => "Packages",
            Tab::Machines => "Machines",
            Tab::Config => "Config",
        }
    }

    pub fn all() -> &'static [Tab] {
        &[
            Tab::Overview,
            Tab::Files,
            Tab::Packages,
            Tab::Machines,
            Tab::Config,
        ]
    }
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum DaemonOp {
    None,
    Starting,
    Stopping,
}

/// A `tether` subcommand run in the background. Only one runs at a time.
#[derive(Clone, PartialEq, Debug)]
pub enum Job {
    Sync,
    Rollback {
        manager: String,
        commit: String,
        short_hash: String,
    },
}

impl Job {
    pub fn args(&self) -> Vec<&str> {
        match self {
            Job::Sync => vec!["sync"],
            Job::Rollback {
                manager, commit, ..
            } => vec!["rollback", "packages", manager, commit],
        }
    }
}

/// Layers drawn over the active tab, bottom to top.
pub enum Overlay {
    /// Not modal: keys still reach the tab underneath.
    Help,
    Confirm(Confirm),
    FileImport(FileImport),
    PkgImport(PkgImport),
    ProfilePicker(ProfilePicker),
}

impl Overlay {
    pub fn is_modal(&self) -> bool {
        !matches!(self, Overlay::Help)
    }
}

pub struct App {
    pub state: DashboardState,
    pub theme: Theme,
    pub active_tab: Tab,
    pub should_quit: bool,
    pub running: Option<Job>,
    pub daemon_op: DaemonOp,
    pub last_refresh: Instant,
    pub flash_error: Option<(Instant, String)>,
    pub flash_message: Option<(Instant, String)>,
    pub overlays: Vec<Overlay>,
    pub overview_scroll: usize,
    pub files: FilesTabState,
    pub packages: PackagesTabState,
    pub machines: MachinesTabState,
    pub config: ConfigTabState,
    pub uninstalling: Option<(String, String)>,
    pub installing: Option<(String, String)>,
    /// A sync was asked for while a job ran; it starts when that job exits.
    pub sync_pending: bool,
}

impl App {
    pub fn new(state: DashboardState, deleted: HashMap<String, Vec<String>>) -> Self {
        Self {
            state,
            theme: Theme::ansi(),
            active_tab: Tab::Overview,
            should_quit: false,
            running: None,
            daemon_op: DaemonOp::None,
            last_refresh: Instant::now(),
            flash_error: None,
            flash_message: None,
            overlays: Vec::new(),
            overview_scroll: 0,
            files: FilesTabState::new(deleted),
            packages: PackagesTabState::new(),
            machines: MachinesTabState::default(),
            config: ConfigTabState::default(),
            uninstalling: None,
            installing: None,
            sync_pending: false,
        }
    }

    pub fn flash_error(&mut self, msg: impl Into<String>) {
        self.flash_error = Some((Instant::now(), msg.into()));
    }

    pub fn flash_success(&mut self, msg: impl Into<String>) {
        self.flash_message = Some((Instant::now(), msg.into()));
    }

    pub fn machine_id(&self) -> &str {
        self.state
            .sync_state
            .as_ref()
            .map(|s| s.machine_id.as_str())
            .unwrap_or("")
    }

    pub fn encrypted(&self) -> bool {
        self.state
            .config
            .as_ref()
            .map(|c| c.security.encrypt_dotfiles)
            .unwrap_or(false)
    }

    pub fn help_open(&self) -> bool {
        self.overlays.iter().any(|o| matches!(o, Overlay::Help))
    }

    pub fn rollback_running(&self) -> bool {
        matches!(self.running, Some(Job::Rollback { .. }))
    }

    /// Start a sync unless a job already runs.
    pub fn sync_cmd(&self) -> Option<Cmd> {
        if self.running.is_some() {
            None
        } else {
            Some(Cmd::Run(Job::Sync))
        }
    }

    /// Sync after a change, or once the running job exits.
    pub fn follow_up_sync(&mut self) -> Option<Cmd> {
        let cmd = self.sync_cmd();
        self.sync_pending = cmd.is_none();
        cmd
    }

    pub fn reload_state(&mut self) {
        self.state = DashboardState::load();
        self.files.deleted = repo::load_deleted_files(&self.state);
        files::refresh_expanded(self);
        packages::refresh_expanded(self);
        self.last_refresh = Instant::now();
    }
}

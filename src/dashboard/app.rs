use super::components::config::ConfigTabState;
use super::components::confirm::Confirm;
use super::components::file_import::FileImport;
use super::components::files::{self, FilesTabState};
use super::components::machines::MachinesTabState;
use super::components::packages::{self, PackagesTabState};
use super::components::palette::Palette;
use super::components::pkg_import::PkgImport;
use super::components::profile_picker::ProfilePicker;
use super::components::security::{self, SecurityTabState};
use super::components::toast::{Toast, ToastKind};
use super::msg::Cmd;
use super::repo;
use super::state::DashboardState;
use super::theme::Theme;
use crate::packages::inbox::InboxItem;
use crossterm::event::KeyEvent;
use ratatui::layout::{Position, Rect};
use std::cell::RefCell;
use std::collections::HashMap;
use std::time::Instant;

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Tab {
    Overview,
    Files,
    Packages,
    Machines,
    Config,
    Security,
}

impl Tab {
    pub fn title(&self) -> &str {
        match self {
            Tab::Overview => "Overview",
            Tab::Files => "Files",
            Tab::Packages => "Packages",
            Tab::Machines => "Machines",
            Tab::Config => "Config",
            Tab::Security => "Security",
        }
    }

    pub fn all() -> &'static [Tab] {
        &[
            Tab::Overview,
            Tab::Files,
            Tab::Packages,
            Tab::Machines,
            Tab::Config,
            Tab::Security,
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
            // The confirm popup is the user's yes. Without a terminal, rollback installs only
            // the newest trusted versions, never an older one
            Job::Rollback {
                manager, commit, ..
            } => vec!["rollback", "packages", manager, commit, "--yes"],
        }
    }
}

/// A package install in flight. `id` tells its result apart from an earlier install of the same package.
#[derive(Clone, PartialEq, Debug)]
pub struct InstallOp {
    pub id: u64,
    pub manager_key: String,
    pub name: String,
}

/// Layers drawn over the active tab, bottom to top.
pub enum Overlay {
    /// Not modal: keys still reach the tab underneath.
    Help,
    Confirm(Confirm),
    FileImport(FileImport),
    PkgImport(PkgImport),
    ProfilePicker(ProfilePicker),
    Palette(Palette),
}

impl Overlay {
    pub fn is_modal(&self) -> bool {
        !matches!(self, Overlay::Help)
    }
}

/// Something the user can run from a key, a footer hint or the command palette.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Action {
    Sync,
    ToggleDaemon,
    Refresh,
    Help,
    Quit,
    ImportPackages,
    ImportDotfile,
    PickProfile,
    ApproveAll,
}

/// A clickable region recorded by the last draw.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Hit {
    Tab(Tab),
    /// Row `n` of the active tab's list.
    Row(usize),
    /// Item `n` of the top overlay's list.
    Item(usize),
    /// Acts like pressing this key.
    Key(KeyEvent),
    /// Outside the help overlay: closes it.
    CloseHelp,
    /// Inside the help overlay: absorbs the click.
    Block,
    /// Toast `n` of `App::toasts`: dismisses it.
    Toast(usize),
}

pub type PackageLists = HashMap<String, Vec<String>>;

pub struct App {
    pub state: DashboardState,
    pub theme: Theme,
    pub active_tab: Tab,
    pub should_quit: bool,
    pub running: Option<Job>,
    pub daemon_op: DaemonOp,
    pub last_refresh: Instant,
    pub toasts: Vec<Toast>,
    pub overlays: Vec<Overlay>,
    pub overview_scroll: usize,
    pub files: FilesTabState,
    pub packages: PackagesTabState,
    pub machines: MachinesTabState,
    pub config: ConfigTabState,
    pub security: SecurityTabState,
    pub uninstalling: Option<(String, String)>,
    pub installing: Option<InstallOp>,
    next_op_id: u64,
    /// A sync was asked for while a job ran; it starts when that job exits.
    pub sync_pending: bool,
    /// Sync commits per day, oldest first, ending today.
    pub sync_activity: Vec<u64>,
    /// Terminal size, for layout math outside `view`.
    pub viewport: Rect,
    /// Clickable regions from the last draw, topmost last. `view` refills it.
    pub hits: RefCell<Vec<(Rect, Hit)>>,
    /// This machine's packages as its managers reported them, and when, shown over its record
    /// until a later sync writes that record.
    pub local_packages: Option<(chrono::DateTime<chrono::Utc>, PackageLists)>,
    /// Animation clock origin.
    started: Instant,
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
            toasts: Vec::new(),
            overlays: Vec::new(),
            overview_scroll: 0,
            files: FilesTabState::new(deleted),
            packages: PackagesTabState::new(),
            machines: MachinesTabState::default(),
            config: ConfigTabState::default(),
            security: SecurityTabState::default(),
            uninstalling: None,
            installing: None,
            next_op_id: 0,
            sync_pending: false,
            sync_activity: Vec::new(),
            viewport: Rect::new(0, 0, 80, 24),
            hits: RefCell::new(Vec::new()),
            local_packages: None,
            started: Instant::now(),
        }
    }

    pub fn flash_error(&mut self, msg: impl Into<String>) {
        self.toast(ToastKind::Error, msg);
    }

    pub fn flash_success(&mut self, msg: impl Into<String>) {
        self.toast(ToastKind::Success, msg);
    }

    pub fn flash_info(&mut self, msg: impl Into<String>) {
        self.toast(ToastKind::Info, msg);
    }

    fn toast(&mut self, kind: ToastKind, msg: impl Into<String>) {
        self.toasts
            .push(Toast::new(kind, msg.into(), Instant::now()));
        let excess = self
            .toasts
            .len()
            .saturating_sub(super::components::toast::MAX_TOASTS);
        self.toasts.drain(..excess);
    }

    /// Something on screen moves, so the loop draws at frame rate.
    pub fn animating(&self) -> bool {
        self.running.is_some()
            || self.daemon_op != DaemonOp::None
            || self.installing.is_some()
            || self.uninstalling.is_some()
            || !self.toasts.is_empty()
            || self.confirm_arming()
    }

    /// The top confirm counts down before it accepts keys.
    fn confirm_arming(&self) -> bool {
        match self.overlays.last() {
            Some(Overlay::Confirm(c)) => c.arming().is_some_and(|a| !a.armed(Instant::now())),
            _ => false,
        }
    }

    /// Milliseconds since start, for spinners and pulses.
    pub fn clock_ms(&self) -> u128 {
        self.started.elapsed().as_millis()
    }

    /// Topmost clickable region under a cell.
    pub fn hit_at(&self, x: u16, y: u16) -> Option<Hit> {
        self.hits
            .borrow()
            .iter()
            .rev()
            .find(|(r, _)| r.contains(Position::new(x, y)))
            .map(|(_, h)| *h)
    }

    pub fn add_hit(&self, area: Rect, hit: Hit) {
        self.hits.borrow_mut().push((area, hit));
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

    /// One install runs at a time: a second would hide the first one's result, and both
    /// would rewrite this machine's state file.
    pub fn install_busy(&mut self) -> bool {
        if self.installing.is_some() {
            self.flash_error("Install in progress");
        }
        self.installing.is_some()
    }

    /// Track a new install and return the command that runs it, unless one already runs.
    /// Without `osv_required`, the user has agreed to install without an OSV answer.
    pub fn start_install(
        &mut self,
        manager_key: String,
        name: String,
        osv_required: bool,
    ) -> Option<Cmd> {
        if self.install_busy() {
            return None;
        }
        let op = self.track_install(manager_key, name);
        Some(Cmd::Install {
            op,
            machine_id: self.machine_id().to_string(),
            osv_required,
        })
    }

    /// Track the approval and install of inbox items as displayed. `label` names them in the
    /// header and toasts. The caller checks `install_busy` first. Without `osv_required`, the
    /// user has agreed to install without an OSV answer.
    pub fn start_inbox_install(
        &mut self,
        label: String,
        items: Vec<InboxItem>,
        osv_required: bool,
    ) -> Cmd {
        let manager_key = items.first().map(|i| i.manager.clone()).unwrap_or_default();
        let op = self.track_install(manager_key, label);
        Cmd::ApprovePackages {
            op,
            items,
            osv_required,
        }
    }

    fn track_install(&mut self, manager_key: String, name: String) -> InstallOp {
        self.next_op_id += 1;
        let op = InstallOp {
            id: self.next_op_id,
            manager_key,
            name,
        };
        self.installing = Some(op.clone());
        op
    }

    /// Sync after a change, or once the running job exits.
    pub fn follow_up_sync(&mut self) -> Option<Cmd> {
        let cmd = self.sync_cmd();
        self.sync_pending = cmd.is_none();
        cmd
    }

    pub fn show_local_packages(&mut self) {
        let machine_id = self.machine_id().to_string();
        let Some((at, packages)) = self.local_packages.clone() else {
            return;
        };
        if machine_id.is_empty() {
            return;
        }
        match self
            .state
            .machines
            .iter_mut()
            .find(|m| m.machine_id == machine_id)
        {
            // A sync after the report read the managers again, so its record is newer
            Some(machine) if machine.last_sync > at => self.local_packages = None,
            Some(machine) => machine.packages = packages,
            None => {
                let mut ms = crate::sync::MachineState::new(&machine_id);
                ms.packages = packages;
                self.state.machines.push(ms);
            }
        }
    }

    pub fn reload_state(&mut self) {
        self.state = DashboardState::load();
        self.show_local_packages();
        self.files.deleted = repo::load_deleted_files(&self.state);
        files::refresh_expanded(self);
        packages::refresh_expanded(self);
        security::reselect(self);
        self.last_refresh = Instant::now();
    }
}

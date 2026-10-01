use super::app::{DaemonOp, InstallOp, Job};
use crossterm::event::{KeyEvent, MouseEvent};
use std::collections::HashMap;

/// Everything that can change dashboard state. Terminal events, the tick and
/// background work all arrive as a `Msg` and go through `update`.
pub enum Msg {
    Key(KeyEvent),
    Mouse(MouseEvent),
    Resize(u16, u16),
    /// Sent once per loop pass; drives timed state such as toast expiry and auto-refresh.
    Tick,
    JobStarted(Job),
    JobSpawnFailed(Job),
    JobExited {
        job: Job,
        success: bool,
    },
    DaemonOpStarted(DaemonOp),
    DaemonOpExited,
    UninstallDone(Result<(), String>),
    InstallDone {
        op: InstallOp,
        result: Result<(), String>,
    },
    LocalPackages(HashMap<String, Vec<String>>),
    /// Sync commits per day, oldest first, ending today.
    Activity(Vec<u64>),
    RestoreDone {
        dotfile: String,
        short_hash: String,
        result: Result<(), String>,
    },
}

/// Side effects `update` asks for. The runtime executes them and reports back with a `Msg`.
pub enum Cmd {
    Run(Job),
    Daemon(DaemonOp),
    Uninstall {
        manager_key: String,
        name: String,
    },
    Install {
        op: InstallOp,
        machine_id: String,
    },
    /// Count sync commits per day; `git log` over the sync repo is too slow for the UI thread.
    LoadActivity,
    CollectPackages {
        config: Box<crate::config::Config>,
        machine_id: String,
    },
    Restore {
        repo_path: String,
        dotfile: String,
        commit: String,
        short_hash: String,
    },
}

/// What a component did with a key it was offered.
pub enum KeyOutcome {
    Ignored,
    Handled(Option<Cmd>),
}

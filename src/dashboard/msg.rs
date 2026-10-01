use super::app::{DaemonOp, Job};
use crossterm::event::KeyEvent;
use std::collections::HashMap;

/// Everything that can change dashboard state. Terminal events, the tick and
/// background work all arrive as a `Msg` and go through `update`.
pub enum Msg {
    Key(KeyEvent),
    /// Sent once per loop pass; drives timed state such as flash expiry and auto-refresh.
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
        manager_key: String,
        name: String,
        result: Result<(), String>,
    },
    LocalPackages(HashMap<String, Vec<String>>),
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
        manager_key: String,
        name: String,
        machine_id: String,
    },
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

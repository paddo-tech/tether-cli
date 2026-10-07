use super::app::{DaemonOp, InstallOp, Job};
use crate::packages::inbox::InboxItem;
use crate::sync::membership::Edit;
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
    /// An uninstall waits for a running sync to release the sync lock.
    UninstallWaiting,
    /// Ok with a message when the package uninstalled but its profiles did not save.
    UninstallDone(Result<Option<String>, String>),
    /// A package's profiles were saved and pushed: the toast text, or the error.
    ProfilesSaved(Result<String, String>),
    InstallDone {
        op: InstallOp,
        result: Result<(), String>,
    },
    /// An install did not run because OSV could not be reached; the user decides.
    OsvUnreachable {
        op: InstallOp,
        error: String,
    },
    /// Inbox items in `unchecked` were not approved because OSV could not be reached; the
    /// user decides. `result` reports the other items.
    ApproveOsvUnreachable {
        op: InstallOp,
        result: Result<(), String>,
        /// Each item with its own OSV error
        unchecked: Vec<(InboxItem, String)>,
    },
    LocalPackages(HashMap<String, Vec<String>>),
    /// Sync commits per day, oldest first, ending today.
    Activity(Vec<u64>),
    RestoreDone {
        dotfile: String,
        short_hash: String,
        result: Result<(), String>,
    },
    /// A machine key was trusted or untrusted, or an item rejected: the toast text, or the
    /// error.
    InboxDone(Result<String, String>),
    /// Another machine's record was removed and committed, and whether this machine
    /// untrusted its key, or the error.
    MachineRemoved {
        machine_id: String,
        result: Result<bool, String>,
    },
}

/// Side effects `update` asks for. The runtime executes them and reports back with a `Msg`.
pub enum Cmd {
    Run(Job),
    Daemon(DaemonOp),
    /// With `leave`, also take this machine's profile out of the package's members.
    Uninstall {
        manager_key: String,
        name: String,
        leave: Option<Edit>,
    },
    /// Apply an edit to a package's members, and push.
    SaveProfiles {
        manager_key: String,
        name: String,
        edit: Edit,
    },
    /// With `osv_required`, an install that OSV cannot check waits for the user.
    Install {
        op: InstallOp,
        osv_required: bool,
    },
    /// Approve inbox items exactly as displayed, then install them one after another.
    /// Reports one `InstallDone`. Inbox writes wait on its lock, so they run off the UI thread.
    /// With `osv_required`, an item that OSV cannot check waits for the user.
    ApprovePackages {
        op: InstallOp,
        items: Vec<InboxItem>,
        osv_required: bool,
    },
    /// Trust the machine key of an inbox item as displayed. `label` names the machine.
    TrustKey {
        item: Box<InboxItem>,
        label: String,
    },
    /// Reject an inbox item.
    Reject(Box<InboxItem>),
    /// Trust the key that signs a machine's record, only while its fingerprint is the one
    /// shown. `label` names the machine.
    TrustMachine {
        machine_id: String,
        fingerprint: String,
        label: String,
    },
    /// Remove a machine's key from the trust store.
    Untrust {
        machine_id: String,
        label: String,
    },
    /// Remove another machine's record as `tether machines remove` does, without the push.
    /// Only while the record is still an old id with this SHA-256, as the confirm showed it.
    RemoveMachine {
        machine_id: String,
        digest: String,
    },
    /// Count sync commits per day; `git log` over the sync repo is too slow for the UI thread.
    LoadActivity,
    CollectPackages {
        config: Box<crate::config::Config>,
        machine_id: String,
    },
    /// Copy a dotfile's backup over the file.
    RestoreBackup {
        path: String,
        timestamp: String,
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

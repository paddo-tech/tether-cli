use crate::config::Config;
use crate::packages::inbox::{self, Inbox, TrustedMachine};
use crate::sync::{signing, ConflictState, MachineState, SyncEngine, SyncState, TeamManifest};

pub struct DashboardState {
    pub config: Option<Config>,
    pub sync_state: Option<SyncState>,
    pub conflicts: ConflictState,
    pub machines: Vec<MachineState>,
    pub team_manifest: TeamManifest,
    pub daemon_pid: Option<u32>,
    pub daemon_running: bool,
    pub activity_lines: Vec<String>,
    pub inbox: Inbox,
    pub trusted: Vec<TrustedMachine>,
    /// Records that are likely an earlier id of this machine.
    pub old_ids: Vec<signing::OldId>,
    /// How a sync reads each record, by machine id
    pub record_status: Vec<(String, signing::RecordStatus)>,
    /// Other machines whose record names a 1.x build or no version
    pub old_builds: Vec<String>,
    /// The profiles each package belongs to, as a sync reads the records
    pub membership: Option<crate::sync::membership::Membership>,
    /// Why the package profiles table does not read. Membership is then unknown, never
    /// implicit
    pub membership_error: Option<String>,
    /// This machine's signing key fingerprint, from the key file
    pub own_fingerprint: Option<String>,
}

impl DashboardState {
    pub fn load() -> Self {
        let config = Config::load().ok();
        let sync_state = SyncState::load().ok();
        let conflicts = ConflictState::load().unwrap_or_default();
        let team_manifest = TeamManifest::load().unwrap_or_default();

        let sync_path = sync_state
            .as_ref()
            .and_then(|_| SyncEngine::sync_path().ok());
        let machines = sync_path
            .as_ref()
            .and_then(|p| MachineState::list_all(p).ok())
            .unwrap_or_default();
        let old_ids = match (&sync_path, &sync_state) {
            (Some(p), Some(s)) => signing::old_ids_of_this_machine(p, &machines, &s.machine_id),
            _ => Vec::new(),
        };
        let statuses = match (&sync_path, &sync_state) {
            (Some(p), Some(s)) => signing::record_statuses(p, &s.machine_id).unwrap_or_default(),
            _ => Vec::new(),
        };
        let old_builds = sync_state
            .as_ref()
            .map(|s| signing::old_builds(&machines, &s.machine_id))
            .unwrap_or_default();
        let record_status: Vec<(String, signing::RecordStatus)> = statuses
            .into_iter()
            .map(|(id, status, _)| (id, status))
            .collect();
        let mut membership_error = None;
        let table = match sync_path
            .as_deref()
            .map(crate::sync::membership::read_table)
        {
            Some(Ok(table)) => Some(table),
            Some(Err(e)) => {
                membership_error = Some(e.to_string());
                None
            }
            None => {
                membership_error = Some(
                    "Tether has no sync repo on this machine. Run 'tether init' first".to_string(),
                );
                None
            }
        };
        let membership = match (&config, &sync_state, &table) {
            (Some(config), Some(s), Some(table)) => {
                let this = machines
                    .iter()
                    .find(|m| m.machine_id == s.machine_id)
                    .cloned()
                    .unwrap_or_else(|| MachineState::new(&s.machine_id));
                let others: Vec<(&MachineState, bool)> = machines
                    .iter()
                    .filter(|m| m.machine_id != s.machine_id)
                    .map(|m| {
                        let trusted = record_status.iter().any(|(id, status)| {
                            *id == m.machine_id && *status == signing::RecordStatus::Trusted
                        });
                        (m, trusted)
                    })
                    .collect();
                Some(crate::sync::membership::Membership::new(
                    config, table, &this, &others,
                ))
            }
            _ => None,
        };

        let mut inbox = Inbox::load().unwrap_or_default();
        inbox::sort_by_group(&mut inbox.items);
        let (daemon_pid, daemon_running) = Self::check_daemon();
        let activity_lines = Self::daemon_log_path()
            .map(|path| Self::read_log_tail(&path, 8192, 20))
            .unwrap_or_default();

        Self {
            config,
            sync_state,
            conflicts,
            machines,
            team_manifest,
            daemon_pid,
            daemon_running,
            activity_lines,
            inbox,
            trusted: inbox::trusted_machines().unwrap_or_default(),
            old_ids,
            record_status,
            old_builds,
            membership,
            membership_error,
            own_fingerprint: signing::own_fingerprint(),
        }
    }

    fn check_daemon() -> (Option<u32>, bool) {
        // Try PID file first
        if let Ok(dir) = Config::config_dir() {
            let pid_path = dir.join("daemon.pid");
            if let Ok(contents) = std::fs::read_to_string(&pid_path) {
                if let Ok(pid) = contents.trim().parse::<u32>() {
                    if pid > 0 {
                        let running = unsafe { libc::kill(pid as libc::pid_t, 0) == 0 };
                        if running {
                            return (Some(pid), true);
                        }
                    }
                }
            }
        }

        // Fallback: check launchd (handles missing/stale PID file)
        #[cfg(target_os = "macos")]
        {
            if let Ok(output) = std::process::Command::new("launchctl")
                .args(["list", "com.tether.daemon"])
                .output()
            {
                if output.status.success() {
                    // Parse PID from first line: "PID\tStatus\tLabel" or "{" for JSON
                    let stdout = String::from_utf8_lossy(&output.stdout);
                    if let Some(first) = stdout.lines().next() {
                        // launchctl list <label> outputs: <pid>\t<status>\t<label>
                        // pid is "-" if not running
                        let first_field = first.split('\t').next().unwrap_or("-").trim();
                        if first_field != "-" {
                            if let Ok(pid) = first_field.parse::<u32>() {
                                return (Some(pid), true);
                            }
                        }
                    }
                }
            }
        }

        // Fallback: check the systemd user service
        #[cfg(target_os = "linux")]
        {
            if let Ok(output) = std::process::Command::new("systemctl")
                .args([
                    "--user",
                    "show",
                    "-p",
                    "MainPID",
                    "--value",
                    "tether.service",
                ])
                .output()
            {
                // MainPID is 0 when the service is not running
                let stdout = String::from_utf8_lossy(&output.stdout);
                if let Ok(pid) = stdout.trim().parse::<u32>() {
                    if pid > 0 {
                        return (Some(pid), true);
                    }
                }
            }
        }

        (None, false)
    }

    pub fn daemon_log_path() -> Option<std::path::PathBuf> {
        Config::config_dir().ok().map(|d| d.join("daemon.log"))
    }

    /// The last `max_lines` whole lines in the last `max_bytes` of the log at `log_path`.
    pub fn read_log_tail(
        log_path: &std::path::Path,
        max_bytes: u64,
        max_lines: usize,
    ) -> Vec<String> {
        use std::io::{BufRead, BufReader, Seek, SeekFrom};

        let file = match std::fs::File::open(log_path) {
            Ok(f) => f,
            Err(_) => return Vec::new(),
        };

        let metadata = match file.metadata() {
            Ok(m) => m,
            Err(_) => return Vec::new(),
        };

        let file_size = metadata.len();
        if file_size == 0 {
            return Vec::new();
        }

        let read_size = max_bytes.min(file_size);
        let mut reader = BufReader::new(file);
        if reader.seek(SeekFrom::End(-(read_size as i64))).is_err() {
            return Vec::new();
        }

        // If we seeked into the middle of a line, skip the partial first line
        if read_size < file_size {
            let mut partial = String::new();
            let _ = reader.read_line(&mut partial);
        }

        let lines: Vec<String> = reader.lines().map_while(Result::ok).collect();
        let start = lines.len().saturating_sub(max_lines);
        lines[start..].to_vec()
    }
}

use crate::config::Config;
use crate::sync::{ConflictState, MachineState, SyncEngine, SyncState, TeamManifest};

pub struct DashboardState {
    pub config: Option<Config>,
    pub sync_state: Option<SyncState>,
    pub conflicts: ConflictState,
    pub machines: Vec<MachineState>,
    pub team_manifest: TeamManifest,
    pub daemon_pid: Option<u32>,
    pub daemon_running: bool,
    pub activity_lines: Vec<String>,
}

impl DashboardState {
    pub fn load() -> Self {
        let config = Config::load().ok();
        let sync_state = SyncState::load().ok();
        let conflicts = ConflictState::load().unwrap_or_default();
        let team_manifest = TeamManifest::load().unwrap_or_default();

        let machines = sync_state
            .as_ref()
            .and_then(|_| SyncEngine::sync_path().ok())
            .and_then(|p| MachineState::list_all(&p).ok())
            .unwrap_or_default();

        let (daemon_pid, daemon_running) = Self::check_daemon();
        let activity_lines = Self::read_activity_log();

        Self {
            config,
            sync_state,
            conflicts,
            machines,
            team_manifest,
            daemon_pid,
            daemon_running,
            activity_lines,
        }
    }

    fn check_daemon() -> (Option<u32>, bool) {
        let pid_path = match Config::config_dir() {
            Ok(d) => d.join("daemon.pid"),
            Err(_) => return (None, false),
        };

        if !pid_path.exists() {
            return (None, false);
        }

        let contents = match std::fs::read_to_string(&pid_path) {
            Ok(c) => c,
            Err(_) => return (None, false),
        };

        match contents.trim().parse::<u32>() {
            Ok(pid) if pid > 0 => {
                let running = unsafe { libc::kill(pid as libc::pid_t, 0) == 0 };
                (Some(pid), running)
            }
            _ => (None, false),
        }
    }

    fn read_activity_log() -> Vec<String> {
        use std::io::{BufRead, BufReader, Seek, SeekFrom};

        let log_path = match Config::config_dir() {
            Ok(d) => d.join("daemon.log"),
            Err(_) => return Vec::new(),
        };

        let file = match std::fs::File::open(&log_path) {
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

        let read_size = 8192u64.min(file_size);
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
        let start = lines.len().saturating_sub(20);
        lines[start..].to_vec()
    }
}

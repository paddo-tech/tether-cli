use crate::cli::Output;
use crate::config::Config;
use crate::daemon::DaemonServer;
use anyhow::Result;
use std::fs::{self, OpenOptions};
use std::io;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use tokio::time::{sleep, Duration};

struct DaemonPaths {
    dir: PathBuf,
    pid: PathBuf,
    log: PathBuf,
}

impl DaemonPaths {
    fn new() -> Result<Self> {
        let dir = Config::config_dir()?;
        Ok(Self {
            pid: dir.join("daemon.pid"),
            log: dir.join("daemon.log"),
            dir,
        })
    }
}

pub async fn start() -> Result<()> {
    let paths = DaemonPaths::new()?;
    fs::create_dir_all(&paths.dir)?;

    if let Some(pid) = read_daemon_pid()? {
        if is_process_running(pid) {
            Output::info(&format!("Daemon already running (PID {pid})"));
            return Ok(());
        } else {
            let _ = cleanup_pid_file(Some(pid));
        }
    }

    let exe = std::env::current_exe()?;

    let stdout = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&paths.log)?;
    let stderr = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&paths.log)?;

    let child = Command::new(exe)
        .arg("daemon")
        .arg("run")
        .stdin(Stdio::null())
        .stdout(Stdio::from(stdout))
        .stderr(Stdio::from(stderr))
        .spawn()?;

    let pid = child.id();
    fs::write(&paths.pid, pid.to_string())?;
    Output::success(&format!("Daemon started (PID {pid})"));
    Ok(())
}

pub async fn stop() -> Result<()> {
    let paths = DaemonPaths::new()?;
    let pid = match read_daemon_pid()? {
        Some(pid) => pid,
        None => {
            Output::info("Daemon is not running");
            return Ok(());
        }
    };

    if !is_process_running(pid) {
        Output::info("Daemon is not running");
        let _ = cleanup_pid_file(Some(pid));
        return Ok(());
    }

    let signal_result = unsafe { libc::kill(pid as libc::pid_t, libc::SIGTERM) };
    if signal_result != 0 {
        let err = io::Error::last_os_error();
        if err.raw_os_error() != Some(libc::ESRCH) {
            return Err(anyhow::anyhow!("Failed to stop daemon: {}", err));
        }
    }

    // Graceful: wait up to 10 seconds
    for _ in 0..50 {
        if !is_process_running(pid) {
            break;
        }
        sleep(Duration::from_millis(200)).await;
    }

    // Force kill if still running
    if is_process_running(pid) {
        log::debug!("Daemon did not exit gracefully, sending SIGKILL");
        unsafe { libc::kill(pid as libc::pid_t, libc::SIGKILL) };

        // Wait for forced termination
        for _ in 0..10 {
            if !is_process_running(pid) {
                break;
            }
            sleep(Duration::from_millis(200)).await;
        }
    }

    // Final check
    if is_process_running(pid) {
        return Err(anyhow::anyhow!(
            "Daemon did not exit after SIGKILL. Check logs: {}",
            paths.log.display()
        ));
    }

    cleanup_pid_file(Some(pid))?;
    Output::success("Daemon stopped");
    Ok(())
}

pub async fn restart() -> Result<()> {
    Output::info("Restarting daemon...");
    stop().await?;
    sleep(Duration::from_millis(500)).await;
    start().await
}

pub async fn status() -> Result<()> {
    let paths = DaemonPaths::new()?;
    let running = match read_daemon_pid()? {
        Some(pid) if is_process_running(pid) => format!("running (PID {pid})"),
        _ => "not running. Start it with 'tether daemon start'".to_string(),
    };
    Output::key_value("Daemon", &running);
    let service = match service_path()? {
        Some(path) if path.exists() => format!("installed ({})", path.display()),
        Some(_) => "not installed. Install it with 'tether daemon install'".to_string(),
        None => "not available on this OS".to_string(),
    };
    Output::key_value("Login service", &service);
    let state = crate::sync::SyncState::load()?;
    Output::key_value(
        "Last sync",
        &crate::cli::output::relative_time(state.last_sync),
    );
    Output::key_value(
        "Last upgrade",
        &state
            .last_upgrade
            .map_or("never".to_string(), crate::cli::output::relative_time),
    );
    Output::key_value("Log", &paths.log.display().to_string());
    Ok(())
}

#[cfg(target_os = "macos")]
fn service_path() -> Result<Option<PathBuf>> {
    launchd_plist_path().map(Some)
}

#[cfg(target_os = "linux")]
fn service_path() -> Result<Option<PathBuf>> {
    systemd_unit_path().map(Some)
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn service_path() -> Result<Option<PathBuf>> {
    Ok(None)
}

/// Print the last `lines` lines of the log, then, with `follow`, each line the daemon adds
/// until Ctrl-C. See [`LogFollower`] for rotation.
pub async fn logs(lines: usize, follow: bool) -> Result<()> {
    let log_path = DaemonPaths::new()?.log;
    if !log_path.exists() && !follow {
        Output::info("No daemon logs yet");
        return Ok(());
    }

    let mut follower = LogFollower::open(log_path);
    let text = follower.poll()?;
    let all: Vec<&str> = text.lines().collect();
    for line in &all[all.len().saturating_sub(lines)..] {
        println!("{line}");
    }
    if !follow {
        return Ok(());
    }

    loop {
        sleep(Duration::from_millis(500)).await;
        let added = follower.poll()?;
        if !added.is_empty() {
            print!("{added}");
            io::Write::flush(&mut io::stdout())?;
        }
    }
}

/// Bytes before the read position that a poll compares, to see that the file was not
/// truncated and filled again between two polls.
const ANCHOR_LEN: usize = 64;

/// Reads what is added to a log, as `tail -F` does. The daemon rotates its log by copy and
/// truncate, so the file keeps its identity: a file shorter than the read position, or whose
/// bytes before that position changed, was rotated and is read from the start. A log
/// replaced by another file (another inode) is read to its end, then the new file from the
/// start. A UTF-8 character split across two reads is kept until it is complete.
struct LogFollower {
    path: PathBuf,
    file: Option<fs::File>,
    identity: Option<(u64, u64)>,
    /// The last bytes read, up to [`ANCHOR_LEN`]
    anchor: Vec<u8>,
    /// An incomplete UTF-8 sequence at the end of the last read
    carry: Vec<u8>,
}

#[cfg(unix)]
fn file_identity(meta: &fs::Metadata) -> Option<(u64, u64)> {
    use std::os::unix::fs::MetadataExt;
    Some((meta.dev(), meta.ino()))
}

#[cfg(not(unix))]
fn file_identity(_meta: &fs::Metadata) -> Option<(u64, u64)> {
    None
}

impl LogFollower {
    fn open(path: PathBuf) -> Self {
        let mut follower = Self {
            path,
            file: None,
            identity: None,
            anchor: Vec::new(),
            carry: Vec::new(),
        };
        follower.reopen();
        follower
    }

    fn reopen(&mut self) {
        self.file = fs::File::open(&self.path).ok();
        self.identity = self
            .file
            .as_ref()
            .and_then(|f| f.metadata().ok())
            .and_then(|m| file_identity(&m));
        self.anchor.clear();
    }

    /// The text added since the last poll.
    fn poll(&mut self) -> Result<String> {
        let mut text = self.read_current()?;
        let replaced = fs::metadata(&self.path)
            .ok()
            .is_some_and(|m| self.file.is_none() || file_identity(&m) != self.identity);
        if replaced {
            text.push_str(&String::from_utf8_lossy(&std::mem::take(&mut self.carry)));
            self.reopen();
            text.push_str(&self.read_current()?);
        }
        Ok(text)
    }

    fn read_current(&mut self) -> Result<String> {
        use std::io::{Read, Seek, SeekFrom};

        let Some(file) = self.file.as_mut() else {
            return Ok(String::new());
        };
        let pos = file.stream_position()?;
        let len = file.metadata()?.len();
        let rotated = len < pos || {
            let mut before = vec![0; self.anchor.len()];
            file.seek(SeekFrom::Start(pos - before.len() as u64))?;
            file.read_exact(&mut before)?;
            before != self.anchor
        };
        if rotated {
            file.seek(SeekFrom::Start(0))?;
            self.anchor.clear();
            self.carry.clear();
        }
        let mut added = Vec::new();
        file.read_to_end(&mut added)?;
        self.anchor.extend_from_slice(&added);
        let excess = self.anchor.len().saturating_sub(ANCHOR_LEN);
        self.anchor.drain(..excess);
        Ok(decode(&mut self.carry, &added))
    }
}

/// Decode `carry` and `bytes` as UTF-8, and keep an incomplete sequence at the end in `carry`
/// for the next read. Invalid bytes become U+FFFD.
fn decode(carry: &mut Vec<u8>, bytes: &[u8]) -> String {
    carry.extend_from_slice(bytes);
    let complete = match std::str::from_utf8(carry) {
        Ok(_) => carry.len(),
        Err(e) if e.error_len().is_none() => e.valid_up_to(),
        Err(_) => {
            // An invalid byte: keep only a trailing incomplete sequence, at most 3 bytes
            let tail = (1..=3.min(carry.len()))
                .find(|&n| {
                    let start = carry.len() - n;
                    std::str::from_utf8(&carry[start..])
                        .err()
                        .is_some_and(|e| e.valid_up_to() == 0 && e.error_len().is_none())
                })
                .unwrap_or(0);
            carry.len() - tail
        }
    };
    let text = String::from_utf8_lossy(&carry[..complete]).into_owned();
    carry.drain(..complete);
    text
}

pub async fn run_daemon() -> Result<()> {
    let mut server = DaemonServer::new();
    let pid = std::process::id();
    log::info!("Daemon process starting (PID {pid})");

    // Write PID file so dashboard/CLI can detect the running daemon
    if let Ok(paths) = DaemonPaths::new() {
        let _ = fs::write(&paths.pid, pid.to_string());
    }

    let result = server.run().await;
    if let Err(err) = cleanup_pid_file(Some(pid)) {
        log::warn!("Failed to clean up daemon pid file: {err}");
    }
    result
}

fn read_daemon_pid() -> Result<Option<u32>> {
    let pid_path = DaemonPaths::new()?.pid;
    if !pid_path.exists() {
        return Ok(None);
    }

    let contents = fs::read_to_string(&pid_path)?;
    match contents.trim().parse::<u32>() {
        Ok(pid) if pid > 0 => Ok(Some(pid)),
        _ => Ok(None),
    }
}

fn is_process_running(pid: u32) -> bool {
    unsafe {
        if libc::kill(pid as libc::pid_t, 0) == 0 {
            true
        } else {
            // ESRCH = no such process
            io::Error::last_os_error().raw_os_error() != Some(libc::ESRCH)
        }
    }
}

fn cleanup_pid_file(expected_pid: Option<u32>) -> Result<()> {
    let paths = DaemonPaths::new()?;
    if !paths.pid.exists() {
        return Ok(());
    }

    let contents = fs::read_to_string(&paths.pid)?;
    if expected_pid
        .map(|pid| contents.trim() == pid.to_string())
        .unwrap_or(true)
    {
        let _ = fs::remove_file(&paths.pid);
    }

    Ok(())
}

#[cfg(target_os = "macos")]
const LAUNCHD_LABEL: &str = "com.tether.daemon";

#[cfg(target_os = "macos")]
fn launchd_plist_path() -> Result<PathBuf> {
    let home = crate::home_dir()?;
    Ok(home
        .join("Library")
        .join("LaunchAgents")
        .join(format!("{LAUNCHD_LABEL}.plist")))
}

/// launchd runs this copy of the installed binary. macOS stores an App Management grant per
/// executable path, and Homebrew puts each version in a new Cellar path.
#[cfg(target_os = "macos")]
fn launchd_binary_path() -> Result<PathBuf> {
    Ok(Config::config_dir()?.join("bin").join("tether"))
}

/// launchd and systemd start services with a minimal PATH, which hides Homebrew and
/// version-managed tools, so the installing shell's environment is baked in.
#[cfg(any(target_os = "macos", target_os = "linux"))]
fn service_env() -> Vec<(&'static str, String)> {
    ["PATH", "GEM_HOME", "GEM_PATH"]
        .into_iter()
        .filter_map(|key| std::env::var(key).ok().map(|value| (key, value)))
        .collect()
}

#[cfg(target_os = "macos")]
fn generate_plist(
    exe: &std::path::Path,
    source: &std::path::Path,
    service_env: &[(String, String)],
) -> Result<String> {
    let paths = DaemonPaths::new()?;

    let mut env = String::new();
    let source = (
        crate::daemon::server::SOURCE_ENV.to_string(),
        source.display().to_string(),
    );
    for (key, value) in service_env.iter().cloned().chain([source]) {
        let value = value
            .replace('&', "&amp;")
            .replace('<', "&lt;")
            .replace('>', "&gt;");
        env.push_str(&format!(
            "        <key>{key}</key>\n        <string>{value}</string>\n"
        ));
    }

    Ok(format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key>
    <string>{LAUNCHD_LABEL}</string>
    <key>ProgramArguments</key>
    <array>
        <string>{}</string>
        <string>daemon</string>
        <string>run</string>
    </array>
    <key>EnvironmentVariables</key>
    <dict>
{env}    </dict>
    <key>RunAtLoad</key>
    <true/>
    <key>KeepAlive</key>
    <true/>
    <key>StandardOutPath</key>
    <string>{}</string>
    <key>StandardErrorPath</key>
    <string>{}</string>
    <key>ProcessType</key>
    <string>Background</string>
</dict>
</plist>
"#,
        exe.display(),
        paths.log.display(),
        paths.log.display()
    ))
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
pub async fn install() -> Result<()> {
    Err(anyhow::anyhow!(
        "Launchd is only available on macOS. Use 'tether daemon start' instead."
    ))
}

#[cfg(target_os = "linux")]
const SYSTEMD_UNIT: &str = "tether.service";

#[cfg(target_os = "linux")]
fn systemd_unit_path() -> Result<PathBuf> {
    let config = match std::env::var_os("XDG_CONFIG_HOME") {
        Some(dir) if !dir.is_empty() => PathBuf::from(dir),
        _ => crate::home_dir()?.join(".config"),
    };
    Ok(config.join("systemd").join("user").join(SYSTEMD_UNIT))
}

/// systemd expands `%` specifiers in every value and C-unescapes quoted words. ExecStart
/// expands `$` variables only in arguments, not in the executable path, so `$` stays as is.
#[cfg(any(target_os = "linux", test))]
fn systemd_quote(value: &str) -> String {
    let escaped = value
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('%', "%%");
    format!("\"{escaped}\"")
}

/// A unit file line ends at a newline, so no value may contain one.
#[cfg(any(target_os = "linux", test))]
fn generate_unit(
    exe: &std::path::Path,
    log: &std::path::Path,
    env: &[(&str, String)],
) -> Result<String> {
    let exe = exe.display().to_string();
    let log = log.display().to_string();
    // systemd rejects these in an executable path even when escaped
    if exe
        .chars()
        .any(|c| c.is_control() || matches!(c, '"' | '\'' | '\\'))
    {
        anyhow::bail!(
            "systemd cannot run {exe:?}: the path has a quote, backslash or control character. \
             Install tether in a path without them"
        );
    }
    if log.contains('\n') {
        anyhow::bail!("systemd cannot log to {log:?}: the path has a newline");
    }
    if let Some((key, _)) = env.iter().find(|(_, value)| value.contains('\n')) {
        anyhow::bail!("{key} contains a newline, which a systemd unit cannot hold");
    }
    let environment: String = env
        .iter()
        .map(|(key, value)| {
            let pair = systemd_quote(&format!("{key}={value}"));
            format!("Environment={pair}\n")
        })
        .collect();
    // systemd reads an append: path literally to the end of the line: spaces need no
    // escape, and quotes would become part of the path
    let log = log.replace('%', "%%");
    Ok(format!(
        "[Unit]\n\
         Description=Tether dotfile and package sync\n\
         \n\
         [Service]\n\
         ExecStart={} daemon run\n\
         {environment}\
         Restart=always\n\
         RestartSec=10\n\
         StandardOutput=append:{log}\n\
         StandardError=append:{log}\n\
         \n\
         [Install]\n\
         WantedBy=default.target\n",
        systemd_quote(&exe),
    ))
}

/// The user's systemd manager answers only when systemd runs this login session.
#[cfg(target_os = "linux")]
fn systemd_user_available() -> bool {
    Command::new("systemctl")
        .args(["--user", "show-environment"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

#[cfg(target_os = "linux")]
fn systemctl_user(args: &[&str]) -> Result<()> {
    let output = Command::new("systemctl")
        .arg("--user")
        .args(args)
        .output()?;
    if !output.status.success() {
        return Err(anyhow::anyhow!(
            "systemctl --user {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    Ok(())
}

#[cfg(target_os = "linux")]
pub async fn install() -> Result<()> {
    if !systemd_user_available() {
        return Err(anyhow::anyhow!(
            "No systemd user session found. Use 'tether daemon start' instead."
        ));
    }
    let unit_path = systemd_unit_path()?;

    // Stop existing daemon if running via manual start
    if let Some(pid) = read_daemon_pid()? {
        if is_process_running(pid) {
            Output::info("Stopping existing daemon...");
            stop().await?;
        }
    }

    if let Some(parent) = unit_path.parent() {
        fs::create_dir_all(parent)?;
    }
    let unit = generate_unit(
        &std::env::current_exe()?,
        &DaemonPaths::new()?.log,
        &service_env(),
    )?;
    fs::write(&unit_path, unit)?;

    systemctl_user(&["daemon-reload"])?;
    systemctl_user(&["enable", SYSTEMD_UNIT])?;
    // restart also starts it, and picks up a changed unit on reinstall
    systemctl_user(&["restart", SYSTEMD_UNIT])?;

    Output::success("systemd user service installed");
    Output::info("Daemon will now start automatically on login and restart if it exits");
    Output::dim("  To keep it running after you log out, run 'loginctl enable-linger'");
    Ok(())
}

#[cfg(target_os = "macos")]
pub async fn install() -> Result<()> {
    let plist_path = launchd_plist_path()?;

    // Stop existing daemon if running via manual start
    if let Some(pid) = read_daemon_pid()? {
        if is_process_running(pid) {
            Output::info("Stopping existing daemon...");
            stop().await?;
        }
    }

    // Unload if already loaded
    let _ = Command::new("launchctl")
        .args(["unload", "-w"])
        .arg(&plist_path)
        .output();

    // Create LaunchAgents directory if needed
    if let Some(parent) = plist_path.parent() {
        fs::create_dir_all(parent)?;
    }

    let source = std::env::current_exe()?;
    let exe = launchd_binary_path()?;
    crate::daemon::server::refresh_binary_copy(&source, &exe)?;
    let env: Vec<(String, String)> = service_env()
        .into_iter()
        .map(|(key, value)| (key.to_string(), value))
        .collect();
    fs::write(&plist_path, generate_plist(&exe, &source, &env)?)?;

    // Load the service
    let output = Command::new("launchctl")
        .args(["load", "-w"])
        .arg(&plist_path)
        .output()?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(anyhow::anyhow!(
            "Failed to load launchd service: {}",
            stderr
        ));
    }

    Output::success("Launchd service installed");
    Output::info("Daemon will now start automatically on login and restart if it exits");
    Ok(())
}

/// A value from the plist, read with `plutil -extract`. None when the plist lacks it.
#[cfg(target_os = "macos")]
fn plist_value(
    plist_path: &std::path::Path,
    key_path: &str,
    format: &str,
) -> Result<Option<String>> {
    let output = Command::new("plutil")
        .args(["-extract", key_path, format, "-o", "-"])
        .arg(plist_path)
        .output()?;
    if !output.status.success() {
        return Ok(None);
    }
    Ok(Some(String::from_utf8(output.stdout)?.trim().to_string()))
}

/// Plists written before 1.12.1 have no EnvironmentVariables, so launchd runs the
/// daemon with its default PATH and package commands resolve to system tools
/// (e.g. macOS Ruby 2.6 `gem`). Plists written before 2.0 run the installed binary, so each
/// upgrade asks for App Management again. Rewrite such a plist to run the copy, keeping
/// the installed binary as the source, the baked environment and whether the service is loaded.
#[cfg(target_os = "macos")]
fn launchd_loaded() -> Result<bool> {
    Ok(Command::new("launchctl")
        .args(["list", LAUNCHD_LABEL])
        .output()?
        .status
        .success())
}

#[cfg(target_os = "macos")]
pub async fn refresh_stale_launchd_service() -> Result<()> {
    let plist_path = launchd_plist_path()?;
    if !plist_path.exists() {
        return Ok(());
    }
    let exe = launchd_binary_path()?;
    let source_key = format!("EnvironmentVariables.{}", crate::daemon::server::SOURCE_ENV);
    if let Some(source) = plist_value(&plist_path, &source_key, "raw")? {
        // A 1.x daemon, run after a downgrade, ignores the source and keeps its binary in the copy
        if crate::daemon::server::refresh_binary_copy(&PathBuf::from(source), &exe)?
            && launchd_loaded()?
        {
            let target = format!("gui/{}/{LAUNCHD_LABEL}", unsafe { libc::getuid() });
            let output = Command::new("launchctl")
                .args(["kickstart", "-k", &target])
                .output()?;
            if !output.status.success() {
                return Err(anyhow::anyhow!(
                    "Failed to restart launchd service: {}",
                    String::from_utf8_lossy(&output.stderr)
                ));
            }
        }
        return Ok(());
    }

    let source = plist_value(&plist_path, "ProgramArguments.0", "raw")?
        .map(PathBuf::from)
        .ok_or_else(|| anyhow::anyhow!("cannot read the daemon path from the plist"))?;
    // A binary removed since the install leaves only this one to follow
    let source = if source.exists() {
        source
    } else {
        std::env::current_exe()?
    };
    let env: Vec<(String, String)> = match plist_value(&plist_path, "EnvironmentVariables", "json")?
    {
        Some(json) => serde_json::from_str::<std::collections::BTreeMap<String, String>>(&json)?
            .into_iter()
            .collect(),
        None => service_env()
            .into_iter()
            .map(|(key, value)| (key.to_string(), value))
            .collect(),
    };
    crate::daemon::server::refresh_binary_copy(&source, &exe)?;
    let loaded = launchd_loaded()?;

    Output::info("Updating the daemon service...");
    // Write only once the old job is gone: a still-loaded job keeps its cached
    // environment, and the new key would stop later syncs from retrying.
    if loaded {
        let output = Command::new("launchctl")
            .arg("unload")
            .arg(&plist_path)
            .output()?;
        if !output.status.success() {
            return Err(anyhow::anyhow!(
                "Failed to unload launchd service: {}",
                String::from_utf8_lossy(&output.stderr)
            ));
        }
    }
    fs::write(&plist_path, generate_plist(&exe, &source, &env)?)?;
    if loaded {
        let output = Command::new("launchctl")
            .arg("load")
            .arg(&plist_path)
            .output()?;
        if !output.status.success() {
            return Err(anyhow::anyhow!(
                "Failed to reload launchd service: {}",
                String::from_utf8_lossy(&output.stderr)
            ));
        }
    }
    Ok(())
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
pub async fn uninstall() -> Result<()> {
    Err(anyhow::anyhow!("Launchd is only available on macOS"))
}

#[cfg(target_os = "linux")]
pub async fn uninstall() -> Result<()> {
    let unit_path = systemd_unit_path()?;
    if !unit_path.exists() {
        Output::info("systemd user service is not installed");
        return Ok(());
    }

    if let Err(e) = systemctl_user(&["disable", "--now", SYSTEMD_UNIT]) {
        Output::warning(&e.to_string());
    }
    fs::remove_file(&unit_path)?;
    if let Err(e) = systemctl_user(&["daemon-reload"]) {
        Output::warning(&e.to_string());
    }

    Output::success("systemd user service uninstalled");
    Ok(())
}

#[cfg(target_os = "macos")]
pub async fn uninstall() -> Result<()> {
    let plist_path = launchd_plist_path()?;
    let exe = launchd_binary_path()?;
    if exe.exists() {
        fs::remove_file(exe)?;
    }

    if !plist_path.exists() {
        Output::info("Launchd service is not installed");
        return Ok(());
    }

    // Unload the service
    let output = Command::new("launchctl")
        .args(["unload", "-w"])
        .arg(&plist_path)
        .output()?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        Output::warning(&format!("launchctl unload warning: {}", stderr));
    }

    // Remove the plist file
    fs::remove_file(&plist_path)?;

    Output::success("Launchd service uninstalled");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn systemd_unit_runs_the_daemon_with_the_shell_environment() {
        let unit = generate_unit(
            Path::new("/home/me/.cargo/bin/tether"),
            Path::new("/home/me/.tether/daemon.log"),
            &[
                ("PATH", "/home/me/.local/bin:/usr/bin".to_string()),
                ("GEM_HOME", "/home/me/.gem".to_string()),
            ],
        )
        .unwrap();
        assert_eq!(
            unit,
            "[Unit]\n\
             Description=Tether dotfile and package sync\n\
             \n\
             [Service]\n\
             ExecStart=\"/home/me/.cargo/bin/tether\" daemon run\n\
             Environment=\"PATH=/home/me/.local/bin:/usr/bin\"\n\
             Environment=\"GEM_HOME=/home/me/.gem\"\n\
             Restart=always\n\
             RestartSec=10\n\
             StandardOutput=append:/home/me/.tether/daemon.log\n\
             StandardError=append:/home/me/.tether/daemon.log\n\
             \n\
             [Install]\n\
             WantedBy=default.target\n"
        );
    }

    #[test]
    fn systemd_unit_escapes_specifiers_and_quotes() {
        let unit = generate_unit(
            Path::new("/opt/$X/100% y/tether"),
            Path::new("/home/a%b c/daemon.log"),
            &[("PATH", "/x$HOME:/y\\z\"".to_string())],
        )
        .unwrap();
        assert!(unit.contains("ExecStart=\"/opt/$X/100%% y/tether\" daemon run\n"));
        assert!(unit.contains("Environment=\"PATH=/x$HOME:/y\\\\z\\\"\"\n"));
        assert!(unit.contains("StandardOutput=append:/home/a%%b c/daemon.log\n"));
    }

    #[test]
    fn log_follower_survives_rotation_and_split_characters() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("daemon.log");
        let append = |bytes: &[u8]| {
            use std::io::Write;
            let mut f = OpenOptions::new()
                .append(true)
                .create(true)
                .open(&path)
                .unwrap();
            f.write_all(bytes).unwrap();
        };
        append(b"first\n");
        let mut follower = LogFollower::open(path.clone());
        assert_eq!(follower.poll().unwrap(), "first\n");

        // "é" is two bytes, split across two writes
        append(b"caf\xc3");
        assert_eq!(follower.poll().unwrap(), "caf");
        append(b"\xa9\n");
        assert_eq!(follower.poll().unwrap(), "\u{e9}\n");

        // Copy and truncate, then more lines than before: the size alone does not show it
        fs::write(&path, b"").unwrap();
        append(b"rotated and now much longer\n");
        assert_eq!(follower.poll().unwrap(), "rotated and now much longer\n");

        // Replaced by a new file: the old file's last lines come first
        append(b"last old line\n");
        fs::rename(&path, dir.path().join("daemon.log.1")).unwrap();
        append(b"new file\n");
        assert_eq!(follower.poll().unwrap(), "last old line\nnew file\n");
        assert_eq!(follower.poll().unwrap(), "");
    }

    #[test]
    fn decode_keeps_only_an_incomplete_tail() {
        let mut carry = Vec::new();
        assert_eq!(decode(&mut carry, b"a\xff b\xe2\x82"), "a\u{fffd} b");
        assert_eq!(carry, b"\xe2\x82");
        assert_eq!(decode(&mut carry, b"\xac"), "\u{20ac}");
        assert!(carry.is_empty());
    }

    #[test]
    fn systemd_unit_refuses_values_it_cannot_hold() {
        let exe = Path::new("/usr/bin/tether");
        let log = Path::new("/home/me/.tether/daemon.log");
        for bad in [
            "/opt/a\nb/tether",
            "/opt/a\"b/tether",
            "/opt/a'b/tether",
            "/a\\b/tether",
        ] {
            assert!(generate_unit(Path::new(bad), log, &[]).is_err(), "{bad}");
        }
        assert!(generate_unit(exe, Path::new("/home/a\nb/daemon.log"), &[]).is_err());
        assert!(generate_unit(exe, log, &[("PATH", "/a\n/b".to_string())]).is_err());
    }
}

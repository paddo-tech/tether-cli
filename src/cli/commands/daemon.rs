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

pub async fn logs() -> Result<()> {
    let log_path = DaemonPaths::new()?.log;
    if !log_path.exists() {
        Output::info("No daemon logs yet");
        return Ok(());
    }

    Output::info(&format!("Showing daemon logs ({})", log_path.display()));
    let content = fs::read_to_string(&log_path)?;
    let lines: Vec<&str> = content.lines().collect();
    let start = lines.len().saturating_sub(50);

    for line in &lines[start..] {
        println!("{line}");
    }

    Ok(())
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
fn generate_plist(exe: &std::path::Path) -> Result<String> {
    let paths = DaemonPaths::new()?;

    let mut env = String::new();
    for (key, value) in service_env() {
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

/// systemd expands `%` specifiers in every value. ExecStart also expands `$` variables,
/// so its caller escapes those.
#[cfg(any(target_os = "linux", test))]
fn systemd_quote(value: &str) -> String {
    let escaped = value
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('%', "%%");
    format!("\"{escaped}\"")
}

#[cfg(any(target_os = "linux", test))]
fn generate_unit(exe: &std::path::Path, log: &std::path::Path, env: &[(&str, String)]) -> String {
    let environment: String = env
        .iter()
        .map(|(key, value)| {
            let pair = systemd_quote(&format!("{key}={value}"));
            format!("Environment={pair}\n")
        })
        .collect();
    let log = log.display().to_string().replace('%', "%%");
    format!(
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
        systemd_quote(&exe.display().to_string().replace('$', "$$")),
    )
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
    );
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

    // Write plist
    let plist = generate_plist(&std::env::current_exe()?)?;
    fs::write(&plist_path, plist)?;

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

/// Plists written before 1.12.1 have no EnvironmentVariables, so launchd runs the
/// daemon with its default PATH and package commands resolve to system tools
/// (e.g. macOS Ruby 2.6 `gem`). Rewrite such a plist from the caller's shell, keeping
/// the installed binary and whether the user has the service loaded.
#[cfg(target_os = "macos")]
pub async fn refresh_stale_launchd_service() -> Result<()> {
    let plist_path = launchd_plist_path()?;
    match fs::read_to_string(&plist_path) {
        Ok(plist) if !plist.contains("<key>EnvironmentVariables</key>") => {}
        _ => return Ok(()),
    }

    let output = Command::new("plutil")
        .args(["-extract", "ProgramArguments.0", "raw"])
        .arg(&plist_path)
        .output()?;
    if !output.status.success() {
        return Err(anyhow::anyhow!(
            "cannot read the daemon path from the plist"
        ));
    }
    let exe = PathBuf::from(String::from_utf8(output.stdout)?.trim());
    let loaded = Command::new("launchctl")
        .args(["list", LAUNCHD_LABEL])
        .output()?
        .status
        .success();

    Output::info("Updating the daemon service with your shell PATH...");
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
    fs::write(&plist_path, generate_plist(&exe)?)?;
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
        );
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
            Path::new("/opt/$X/100%/te\"ther"),
            Path::new("/home/a%b/daemon.log"),
            &[("PATH", "/x$HOME:/y\\z".to_string())],
        );
        assert!(unit.contains("ExecStart=\"/opt/$$X/100%%/te\\\"ther\" daemon run\n"));
        assert!(unit.contains("Environment=\"PATH=/x$HOME:/y\\\\z\"\n"));
        assert!(unit.contains("StandardOutput=append:/home/a%%b/daemon.log\n"));
    }
}

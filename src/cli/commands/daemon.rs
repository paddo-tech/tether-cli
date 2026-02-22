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

const LAUNCHD_LABEL: &str = "com.tether.daemon";

fn launchd_plist_path() -> Result<PathBuf> {
    let home = crate::home_dir()?;
    Ok(home
        .join("Library")
        .join("LaunchAgents")
        .join(format!("{LAUNCHD_LABEL}.plist")))
}

fn generate_plist() -> Result<String> {
    let exe = std::env::current_exe()?;
    let paths = DaemonPaths::new()?;

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

pub async fn install() -> Result<()> {
    #[cfg(not(target_os = "macos"))]
    {
        return Err(anyhow::anyhow!(
            "Launchd is only available on macOS. Use 'tether daemon start' instead."
        ));
    }

    #[cfg(target_os = "macos")]
    {
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
        let plist = generate_plist()?;
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
}

pub async fn uninstall() -> Result<()> {
    #[cfg(not(target_os = "macos"))]
    {
        return Err(anyhow::anyhow!("Launchd is only available on macOS"));
    }

    #[cfg(target_os = "macos")]
    {
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
}

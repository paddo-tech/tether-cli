use crate::cli::Output;
use crate::config::Config;
use crate::daemon::pid::{is_process_running, read_daemon_pid};
use crate::daemon::DaemonServer;
use anyhow::Result;
use std::fs::{self, OpenOptions};
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

    let mut cmd = Command::new(exe);
    cmd.arg("daemon")
        .arg("run")
        .stdin(Stdio::null())
        .stdout(Stdio::from(stdout))
        .stderr(Stdio::from(stderr));

    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x00000008 | 0x00000200); // DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP
    }

    let child = cmd.spawn()?;

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

    terminate_process(pid)?;

    // On Windows, terminate_process already force-kills (no graceful signal available),
    // so only do the extended wait on Unix where SIGTERM allows graceful shutdown.
    let wait_rounds = if cfg!(windows) { 10 } else { 50 };
    for _ in 0..wait_rounds {
        if !is_process_running(pid) {
            break;
        }
        sleep(Duration::from_millis(200)).await;
    }

    // Force kill if still running (Unix only — redundant on Windows)
    if is_process_running(pid) {
        log::debug!("Daemon did not exit gracefully, force killing");
        force_kill_process(pid);

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
            "Daemon did not exit after force kill. Check logs: {}",
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
    let result = server.run().await;
    if let Err(err) = cleanup_pid_file(Some(pid)) {
        log::warn!("Failed to clean up daemon pid file: {err}");
    }
    result
}

#[cfg(unix)]
fn terminate_process(pid: u32) -> Result<()> {
    let result = unsafe { libc::kill(pid as libc::pid_t, libc::SIGTERM) };
    if result != 0 {
        let err = std::io::Error::last_os_error();
        if err.raw_os_error() != Some(libc::ESRCH) {
            return Err(anyhow::anyhow!("Failed to stop daemon: {}", err));
        }
    }
    Ok(())
}

#[cfg(windows)]
fn terminate_process(pid: u32) -> Result<()> {
    use std::process::Command;
    // Detached/console-less processes can't receive WM_CLOSE, so use /F directly.
    let output = Command::new("taskkill")
        .args(["/F", "/PID", &pid.to_string()])
        .output()?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        if !stderr.contains("not found") {
            log::debug!("taskkill failed: {}", stderr.trim());
        }
    }
    Ok(())
}

#[cfg(unix)]
fn force_kill_process(pid: u32) {
    unsafe { libc::kill(pid as libc::pid_t, libc::SIGKILL) };
}

#[cfg(windows)]
fn force_kill_process(pid: u32) {
    use std::process::Command;
    let _ = Command::new("taskkill")
        .args(["/F", "/PID", &pid.to_string()])
        .output();
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

#[cfg(target_os = "macos")]
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
    #[cfg(windows)]
    {
        if let Some(pid) = read_daemon_pid()? {
            if is_process_running(pid) {
                Output::info("Stopping existing daemon...");
                stop().await?;
            }
        }

        let exe = std::env::current_exe()?;
        let exe_escaped = exe
            .display()
            .to_string()
            .replace('&', "&amp;")
            .replace('<', "&lt;")
            .replace('>', "&gt;")
            .replace('"', "&quot;");
        // XML task definition enables RestartOnFailure (equivalent to macOS KeepAlive)
        let task_xml = format!(
            r#"<?xml version="1.0" encoding="UTF-16"?>
<Task version="1.2" xmlns="http://schemas.microsoft.com/windows/2004/02/mit/task">
  <Triggers>
    <LogonTrigger><Enabled>true</Enabled></LogonTrigger>
  </Triggers>
  <Settings>
    <RestartOnFailure>
      <Interval>PT1M</Interval>
      <Count>999</Count>
    </RestartOnFailure>
    <ExecutionTimeLimit>PT0S</ExecutionTimeLimit>
    <DisallowStartIfOnBatteries>false</DisallowStartIfOnBatteries>
    <StopIfGoingOnBatteries>false</StopIfGoingOnBatteries>
  </Settings>
  <Actions Context="Author">
    <Exec>
      <Command>{exe_escaped}</Command>
      <Arguments>daemon run</Arguments>
    </Exec>
  </Actions>
</Task>"#
        );

        // schtasks expects UTF-16 LE with BOM; use random temp file to avoid symlink attacks
        let mut xml_file = tempfile::Builder::new().suffix(".xml").tempfile()?;
        let xml_path = xml_file.path().to_path_buf();
        let utf16: Vec<u16> = task_xml.encode_utf16().collect();
        let mut bytes = vec![0xFF, 0xFE]; // UTF-16 LE BOM
        for word in &utf16 {
            bytes.extend_from_slice(&word.to_le_bytes());
        }
        std::io::Write::write_all(&mut xml_file, &bytes)?;

        // Remove existing task if present
        let _ = Command::new("schtasks")
            .args(["/Delete", "/TN", "TetherDaemon", "/F"])
            .output();

        let output = Command::new("schtasks")
            .args(["/Create", "/TN", "TetherDaemon", "/XML"])
            .arg(&xml_path)
            .output()?;

        drop(xml_file);

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(anyhow::anyhow!(
                "Failed to create scheduled task: {}",
                stderr
            ));
        }

        start().await?;
        Output::success("Scheduled task installed");
        Output::info("Daemon will now start automatically on login and restart if it exits");
        Ok(())
    }

    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        Err(anyhow::anyhow!(
            "Daemon auto-start not supported on this platform. Use 'tether daemon start' instead."
        ))
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
    #[cfg(windows)]
    {
        let output = Command::new("schtasks")
            .args(["/Delete", "/TN", "TetherDaemon", "/F"])
            .output()?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            if stderr.contains("does not exist") || stderr.contains("cannot find") {
                Output::info("Scheduled task is not installed");
                return Ok(());
            }
            return Err(anyhow::anyhow!(
                "Failed to delete scheduled task: {}",
                stderr
            ));
        }

        stop().await.ok();
        Output::success("Scheduled task uninstalled");
        Ok(())
    }

    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        Err(anyhow::anyhow!(
            "Daemon auto-start not supported on this platform"
        ))
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

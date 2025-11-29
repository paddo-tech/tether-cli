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
        if err.kind() != io::ErrorKind::NotFound {
            return Err(anyhow::anyhow!("Failed to stop daemon: {}", err));
        }
    }

    for _ in 0..20 {
        if !is_process_running(pid) {
            break;
        }
        sleep(Duration::from_millis(200)).await;
    }

    if is_process_running(pid) {
        return Err(anyhow::anyhow!(
            "Daemon did not exit after signaling. Check logs: {}",
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
            let err = io::Error::last_os_error();
            err.kind() != io::ErrorKind::NotFound
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

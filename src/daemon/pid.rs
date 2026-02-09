use crate::config::Config;
use anyhow::Result;

pub fn read_daemon_pid() -> Result<Option<u32>> {
    let pid_path = Config::config_dir()?.join("daemon.pid");
    if !pid_path.exists() {
        return Ok(None);
    }

    let contents = std::fs::read_to_string(&pid_path)?;
    match contents.trim().parse::<u32>() {
        Ok(pid) if pid > 0 => Ok(Some(pid)),
        _ => Ok(None),
    }
}

#[cfg(unix)]
pub fn is_process_running(pid: u32) -> bool {
    unsafe {
        if libc::kill(pid as libc::pid_t, 0) == 0 {
            true
        } else {
            let err = std::io::Error::last_os_error();
            err.kind() != std::io::ErrorKind::NotFound
        }
    }
}

#[cfg(windows)]
pub fn is_process_running(pid: u32) -> bool {
    use std::process::Command;
    Command::new("tasklist")
        .args(["/FI", &format!("PID eq {pid}"), "/FO", "CSV", "/NH"])
        .output()
        .map(|o| {
            let out = String::from_utf8_lossy(&o.stdout);
            // CSV format quotes the PID: "name","1234",... — exact match avoids substring false positives
            out.contains(&format!("\"{pid}\""))
        })
        .unwrap_or(false)
}

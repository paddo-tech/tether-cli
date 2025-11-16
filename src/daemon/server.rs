use anyhow::Result;
use std::time::Duration;
use tokio::time::Interval;

#[cfg(unix)]
use tokio::signal::unix::{signal, SignalKind};

pub struct DaemonServer {
    heartbeat: Duration,
}

impl DaemonServer {
    pub fn new() -> Self {
        Self {
            heartbeat: Duration::from_secs(60),
        }
    }

    fn heartbeat_interval(&self) -> Interval {
        tokio::time::interval(self.heartbeat)
    }

    pub async fn run(&mut self) -> Result<()> {
        log::info!("Daemon starting (pid {})", std::process::id());

        #[cfg(unix)]
        {
            let mut heartbeat = self.heartbeat_interval();
            let mut sigterm = signal(SignalKind::terminate())?;
            let mut sighup = signal(SignalKind::hangup())?;

            let ctrl_c = tokio::signal::ctrl_c();
            tokio::pin!(ctrl_c);

            loop {
                tokio::select! {
                    _ = heartbeat.tick() => {
                        log::debug!("Daemon heartbeat");
                    },
                    _ = &mut ctrl_c => {
                        log::info!("Received Ctrl+C, stopping daemon");
                        break;
                    },
                    _ = sigterm.recv() => {
                        log::info!("Received SIGTERM, stopping daemon");
                        break;
                    },
                    _ = sighup.recv() => {
                        log::info!("Received SIGHUP, reloading configuration");
                        // Placeholder for future reload logic.
                    },
                };
            }
        }

        #[cfg(not(unix))]
        {
            let mut heartbeat = self.heartbeat_interval();
            let ctrl_c = tokio::signal::ctrl_c();
            tokio::pin!(ctrl_c);

            loop {
                tokio::select! {
                    _ = heartbeat.tick() => {
                        log::debug!("Daemon heartbeat");
                    },
                    _ = &mut ctrl_c => {
                        log::info!("Received Ctrl+C, stopping daemon");
                        break;
                    },
                };
            }
        }

        log::info!("Daemon stopped");
        Ok(())
    }
}

impl Default for DaemonServer {
    fn default() -> Self {
        Self::new()
    }
}

use anyhow::Result;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum DaemonMessage {
    Ping,
    Pong,
    Sync,
    Status,
    Stop,
}

pub struct DaemonClient {
    // TODO: Implement IPC client
}

impl DaemonClient {
    pub fn new() -> Result<Self> {
        Ok(Self {})
    }

    pub async fn send(&self, _message: DaemonMessage) -> Result<DaemonMessage> {
        // TODO: Implement IPC send/receive
        Ok(DaemonMessage::Pong)
    }
}

impl Default for DaemonClient {
    fn default() -> Self {
        Self::new().unwrap()
    }
}

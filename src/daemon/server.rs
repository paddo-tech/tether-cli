use anyhow::Result;

pub struct DaemonServer {
    // TODO: Implement daemon server
}

impl DaemonServer {
    pub fn new() -> Self {
        Self {}
    }

    pub async fn run(&mut self) -> Result<()> {
        // TODO: Implement daemon loop
        Ok(())
    }
}

impl Default for DaemonServer {
    fn default() -> Self {
        Self::new()
    }
}

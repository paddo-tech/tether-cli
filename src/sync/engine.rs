use super::{GitBackend, SyncState};
use crate::config::Config;
use anyhow::Result;
use std::path::PathBuf;

pub struct SyncEngine {
    _config: Config,
    state: SyncState,
    git: GitBackend,
}

impl SyncEngine {
    pub fn new(config: Config) -> Result<Self> {
        let state = SyncState::load()?;
        let sync_path = Self::sync_path()?;
        let git = GitBackend::open(&sync_path)?;

        Ok(Self {
            _config: config,
            state,
            git,
        })
    }

    pub fn sync_path() -> Result<PathBuf> {
        let home =
            home::home_dir().ok_or_else(|| anyhow::anyhow!("Could not find home directory"))?;
        Ok(home.join(".tether").join("sync"))
    }

    pub async fn sync(&mut self) -> Result<()> {
        // Pull latest changes
        self.git.pull()?;

        // TODO: Detect local changes
        // TODO: Merge and resolve conflicts
        // TODO: Push updates
        // TODO: Install missing packages

        self.state.mark_synced();
        self.state.save()?;

        Ok(())
    }

    pub fn get_state(&self) -> &SyncState {
        &self.state
    }
}

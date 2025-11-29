use anyhow::Result;
use std::path::PathBuf;

pub struct SyncEngine;

impl SyncEngine {
    pub fn sync_path() -> Result<PathBuf> {
        let home =
            home::home_dir().ok_or_else(|| anyhow::anyhow!("Could not find home directory"))?;
        Ok(home.join(".tether").join("sync"))
    }
}

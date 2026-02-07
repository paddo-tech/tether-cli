use anyhow::Result;
use std::path::PathBuf;

pub struct SyncEngine;

impl SyncEngine {
    pub fn sync_path() -> Result<PathBuf> {
        let home = crate::home_dir()?;
        Ok(home.join(".tether").join("sync"))
    }
}

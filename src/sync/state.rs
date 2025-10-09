use anyhow::Result;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SyncState {
    pub machine_id: String,
    pub last_sync: DateTime<Utc>,
    pub files: HashMap<String, FileState>,
    pub packages: HashMap<String, PackageState>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileState {
    pub hash: String,
    pub last_modified: DateTime<Utc>,
    pub synced: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PackageState {
    pub last_sync: DateTime<Utc>,
    pub hash: String,
}

impl SyncState {
    pub fn state_path() -> Result<PathBuf> {
        let home =
            home::home_dir().ok_or_else(|| anyhow::anyhow!("Could not find home directory"))?;
        Ok(home.join(".tether").join("state.json"))
    }

    pub fn load() -> Result<Self> {
        let path = Self::state_path()?;
        if !path.exists() {
            return Ok(Self::new());
        }
        let content = std::fs::read_to_string(path)?;
        Ok(serde_json::from_str(&content)?)
    }

    pub fn save(&self) -> Result<()> {
        let path = Self::state_path()?;
        let dir = path
            .parent()
            .ok_or_else(|| anyhow::anyhow!("Invalid path"))?;
        std::fs::create_dir_all(dir)?;

        let content = serde_json::to_string_pretty(self)?;
        std::fs::write(path, content)?;
        Ok(())
    }

    fn new() -> Self {
        Self {
            machine_id: Self::generate_machine_id(),
            last_sync: Utc::now(),
            files: HashMap::new(),
            packages: HashMap::new(),
        }
    }

    fn generate_machine_id() -> String {
        hostname::get()
            .ok()
            .and_then(|h| h.into_string().ok())
            .unwrap_or_else(|| "unknown".to_string())
    }

    pub fn update_file(&mut self, path: &str, hash: String) {
        self.files.insert(
            path.to_string(),
            FileState {
                hash,
                last_modified: Utc::now(),
                synced: false,
            },
        );
    }

    pub fn mark_synced(&mut self) {
        self.last_sync = Utc::now();
        for file in self.files.values_mut() {
            file.synced = true;
        }
    }
}

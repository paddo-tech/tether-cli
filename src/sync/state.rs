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

/// Machine state stored in sync repo for cross-machine comparison
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MachineState {
    pub machine_id: String,
    pub hostname: String,
    pub last_sync: DateTime<Utc>,
    #[serde(default)]
    pub os_version: String,
    /// File paths and their hashes
    pub files: HashMap<String, String>,
    /// Package manager -> list of packages
    pub packages: HashMap<String, Vec<String>>,
}

impl MachineState {
    pub fn new(machine_id: &str) -> Self {
        let hostname = hostname::get()
            .ok()
            .and_then(|h| h.into_string().ok())
            .unwrap_or_else(|| "unknown".to_string());

        Self {
            machine_id: machine_id.to_string(),
            hostname,
            last_sync: Utc::now(),
            os_version: String::new(),
            files: HashMap::new(),
            packages: HashMap::new(),
        }
    }

    /// Load machine state from sync repo
    pub fn load_from_repo(sync_path: &std::path::Path, machine_id: &str) -> Result<Option<Self>> {
        let path = sync_path
            .join("machines")
            .join(format!("{}.json", machine_id));
        if !path.exists() {
            return Ok(None);
        }
        let content = std::fs::read_to_string(path)?;
        Ok(Some(serde_json::from_str(&content)?))
    }

    /// Save machine state to sync repo
    pub fn save_to_repo(&self, sync_path: &std::path::Path) -> Result<()> {
        let machines_dir = sync_path.join("machines");
        std::fs::create_dir_all(&machines_dir)?;

        let path = machines_dir.join(format!("{}.json", self.machine_id));
        let content = serde_json::to_string_pretty(self)?;
        std::fs::write(path, content)?;
        Ok(())
    }

    /// List all machines in sync repo
    pub fn list_all(sync_path: &std::path::Path) -> Result<Vec<Self>> {
        let machines_dir = sync_path.join("machines");
        if !machines_dir.exists() {
            return Ok(Vec::new());
        }

        let mut machines = Vec::new();
        for entry in std::fs::read_dir(&machines_dir)? {
            let entry = entry?;
            let path = entry.path();
            if path.extension().map(|e| e == "json").unwrap_or(false) {
                if let Ok(content) = std::fs::read_to_string(&path) {
                    if let Ok(state) = serde_json::from_str::<MachineState>(&content) {
                        machines.push(state);
                    }
                }
            }
        }
        Ok(machines)
    }
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

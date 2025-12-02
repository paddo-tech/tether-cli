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
    #[serde(default)]
    pub last_upgrade: Option<DateTime<Utc>>,
    #[serde(default)]
    pub last_upgrade_with_updates: Option<DateTime<Utc>>,
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
    /// Package manager -> list of installed packages
    /// Keys: brew_formulae, brew_casks, brew_taps, npm, pnpm, bun, gem
    pub packages: HashMap<String, Vec<String>>,
    /// Package manager -> list of packages explicitly removed on this machine
    /// These won't be reinstalled from the union manifest
    #[serde(default)]
    pub removed_packages: HashMap<String, Vec<String>>,
    /// Dotfiles present on this machine (e.g., ".zshrc", ".gitconfig")
    #[serde(default)]
    pub dotfiles: Vec<String>,
    /// Dotfiles ignored on this machine (won't be overwritten during sync)
    #[serde(default)]
    pub ignored_dotfiles: Vec<String>,
    /// Project configs present on this machine (project_key -> list of relative paths)
    /// project_key is normalized git remote URL (e.g., "github.com/user/repo")
    #[serde(default)]
    pub project_configs: HashMap<String, Vec<String>>,
    /// Project configs ignored on this machine (project_key -> list of relative paths)
    #[serde(default)]
    pub ignored_project_configs: HashMap<String, Vec<String>>,
}

impl Default for MachineState {
    fn default() -> Self {
        Self::new("unknown")
    }
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
            removed_packages: HashMap::new(),
            dotfiles: Vec::new(),
            ignored_dotfiles: Vec::new(),
            project_configs: HashMap::new(),
            ignored_project_configs: HashMap::new(),
        }
    }

    /// Maximum allowed items in deserialized collections (DoS protection)
    const MAX_PACKAGES_PER_MANAGER: usize = 10_000;
    const MAX_FILES: usize = 50_000;

    /// Validate package name is safe for shell usage
    fn is_safe_package_name(name: &str) -> bool {
        // Reject empty, too long, or names with shell metacharacters
        !name.is_empty()
            && name.len() <= 256
            && !name.contains([';', '&', '|', '$', '`', '\'', '"', '\\', '\n', '\r'])
    }

    /// Validate and sanitize machine state after deserialization
    fn validate(&mut self) -> Result<()> {
        // Limit files
        if self.files.len() > Self::MAX_FILES {
            anyhow::bail!(
                "Machine state contains too many files ({})",
                self.files.len()
            );
        }

        // Validate and limit packages
        for (manager, packages) in &mut self.packages {
            if packages.len() > Self::MAX_PACKAGES_PER_MANAGER {
                anyhow::bail!(
                    "Machine state contains too many {} packages ({})",
                    manager,
                    packages.len()
                );
            }
            // Filter out unsafe package names
            packages.retain(|p| Self::is_safe_package_name(p));
        }

        // Validate removed_packages
        for packages in self.removed_packages.values_mut() {
            packages.retain(|p| Self::is_safe_package_name(p));
        }

        Ok(())
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
        let mut state: Self = serde_json::from_str(&content)?;
        state.validate()?;
        Ok(Some(state))
    }

    /// Save machine state to sync repo
    pub fn save_to_repo(&self, sync_path: &std::path::Path) -> Result<()> {
        let machines_dir = sync_path.join("machines");
        let path = machines_dir.join(format!("{}.json", self.machine_id));
        let content = serde_json::to_string_pretty(self)?;
        crate::sync::atomic_write(&path, content.as_bytes())
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
                    if let Ok(mut state) = serde_json::from_str::<MachineState>(&content) {
                        // Skip invalid machine states
                        if state.validate().is_ok() {
                            machines.push(state);
                        }
                    }
                }
            }
        }
        Ok(machines)
    }

    /// Compute the union of packages across all machine states
    /// Returns a HashMap where each key is a package manager and value is all packages
    /// installed on ANY machine
    pub fn compute_union_packages(machines: &[Self]) -> HashMap<String, Vec<String>> {
        use std::collections::HashSet;

        let mut union: HashMap<String, HashSet<String>> = HashMap::new();

        for machine in machines {
            for (manager, packages) in &machine.packages {
                let set = union.entry(manager.clone()).or_default();
                for pkg in packages {
                    set.insert(pkg.clone());
                }
            }
        }

        // Convert HashSet back to sorted Vec for deterministic output
        union
            .into_iter()
            .map(|(k, v)| {
                let mut sorted: Vec<_> = v.into_iter().collect();
                sorted.sort();
                (k, sorted)
            })
            .collect()
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
        let content = serde_json::to_string_pretty(self)?;
        crate::sync::atomic_write(&path, content.as_bytes())
    }

    fn new() -> Self {
        Self {
            machine_id: Self::generate_machine_id(),
            last_sync: Utc::now(),
            files: HashMap::new(),
            packages: HashMap::new(),
            last_upgrade: None,
            last_upgrade_with_updates: None,
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

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    // Package name safety tests
    #[test]
    fn test_safe_package_names() {
        assert!(MachineState::is_safe_package_name("git"));
        assert!(MachineState::is_safe_package_name("node-18.x"));
        assert!(MachineState::is_safe_package_name("@angular/cli"));
        assert!(MachineState::is_safe_package_name("python3.11"));
    }

    #[test]
    fn test_unsafe_package_name_shell_injection() {
        assert!(!MachineState::is_safe_package_name("git; rm -rf /"));
        assert!(!MachineState::is_safe_package_name("$(whoami)"));
        assert!(!MachineState::is_safe_package_name("pkg`id`"));
        assert!(!MachineState::is_safe_package_name("pkg|cat /etc/passwd"));
        assert!(!MachineState::is_safe_package_name("pkg&background"));
    }

    #[test]
    fn test_unsafe_package_name_quotes() {
        assert!(!MachineState::is_safe_package_name("pkg'injection"));
        assert!(!MachineState::is_safe_package_name("pkg\"injection"));
        assert!(!MachineState::is_safe_package_name("pkg\\escape"));
    }

    #[test]
    fn test_unsafe_package_name_newlines() {
        assert!(!MachineState::is_safe_package_name("pkg\nmalicious"));
        assert!(!MachineState::is_safe_package_name("pkg\rmalicious"));
    }

    #[test]
    fn test_unsafe_package_name_empty() {
        assert!(!MachineState::is_safe_package_name(""));
    }

    #[test]
    fn test_unsafe_package_name_too_long() {
        let long_name = "a".repeat(300);
        assert!(!MachineState::is_safe_package_name(&long_name));

        let max_len = "a".repeat(256);
        assert!(MachineState::is_safe_package_name(&max_len));
    }

    // Validation tests
    #[test]
    fn test_validate_filters_unsafe_packages() {
        let mut state = MachineState::new("test");
        state.packages.insert(
            "npm".to_string(),
            vec![
                "safe-pkg".to_string(),
                "unsafe;cmd".to_string(),
                "another-safe".to_string(),
            ],
        );
        state.validate().unwrap();
        let npm_pkgs = state.packages.get("npm").unwrap();
        assert_eq!(npm_pkgs.len(), 2);
        assert!(npm_pkgs.contains(&"safe-pkg".to_string()));
        assert!(npm_pkgs.contains(&"another-safe".to_string()));
    }

    #[test]
    fn test_validate_too_many_files() {
        let mut state = MachineState::new("test");
        for i in 0..60_000 {
            state.files.insert(format!("file{}", i), "hash".to_string());
        }
        assert!(state.validate().is_err());
    }

    #[test]
    fn test_validate_too_many_packages() {
        let mut state = MachineState::new("test");
        let packages: Vec<String> = (0..15_000).map(|i| format!("pkg{}", i)).collect();
        state.packages.insert("npm".to_string(), packages);
        assert!(state.validate().is_err());
    }

    #[test]
    fn test_validate_ok_within_limits() {
        let mut state = MachineState::new("test");
        for i in 0..100 {
            state.files.insert(format!("file{}", i), "hash".to_string());
        }
        state
            .packages
            .insert("npm".to_string(), vec!["typescript".to_string()]);
        assert!(state.validate().is_ok());
    }

    // Union computation tests
    #[test]
    fn test_compute_union_packages_merges() {
        let mut m1 = MachineState::new("m1");
        m1.packages
            .insert("npm".to_string(), vec!["a".to_string(), "b".to_string()]);

        let mut m2 = MachineState::new("m2");
        m2.packages
            .insert("npm".to_string(), vec!["b".to_string(), "c".to_string()]);

        let union = MachineState::compute_union_packages(&[m1, m2]);
        let npm = union.get("npm").unwrap();
        assert_eq!(npm.len(), 3);
        assert!(npm.contains(&"a".to_string()));
        assert!(npm.contains(&"b".to_string()));
        assert!(npm.contains(&"c".to_string()));
    }

    #[test]
    fn test_compute_union_packages_empty() {
        let union = MachineState::compute_union_packages(&[]);
        assert!(union.is_empty());
    }

    #[test]
    fn test_compute_union_packages_sorted() {
        let mut m1 = MachineState::new("m1");
        m1.packages
            .insert("npm".to_string(), vec!["z".to_string(), "a".to_string()]);

        let union = MachineState::compute_union_packages(&[m1]);
        let npm = union.get("npm").unwrap();
        assert_eq!(npm, &vec!["a".to_string(), "z".to_string()]);
    }

    #[test]
    fn test_compute_union_multiple_managers() {
        let mut m1 = MachineState::new("m1");
        m1.packages
            .insert("npm".to_string(), vec!["typescript".to_string()]);
        m1.packages
            .insert("brew_formulae".to_string(), vec!["git".to_string()]);

        let union = MachineState::compute_union_packages(&[m1]);
        assert!(union.contains_key("npm"));
        assert!(union.contains_key("brew_formulae"));
    }

    // Roundtrip tests
    #[test]
    fn test_machine_state_roundtrip() {
        let temp = TempDir::new().unwrap();
        let sync_path = temp.path();
        std::fs::create_dir_all(sync_path.join("machines")).unwrap();

        let mut state = MachineState::new("test-machine");
        state
            .packages
            .insert("npm".to_string(), vec!["typescript".to_string()]);
        state
            .files
            .insert(".zshrc".to_string(), "abc123".to_string());

        state.save_to_repo(sync_path).unwrap();

        let loaded = MachineState::load_from_repo(sync_path, "test-machine")
            .unwrap()
            .unwrap();

        assert_eq!(loaded.machine_id, "test-machine");
        assert_eq!(
            loaded.packages.get("npm"),
            Some(&vec!["typescript".to_string()])
        );
        assert_eq!(loaded.files.get(".zshrc"), Some(&"abc123".to_string()));
    }

    #[test]
    fn test_machine_state_load_nonexistent() {
        let temp = TempDir::new().unwrap();
        let result = MachineState::load_from_repo(temp.path(), "nonexistent").unwrap();
        assert!(result.is_none());
    }
}

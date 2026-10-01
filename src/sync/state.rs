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
    /// Casks deferred during daemon sync (require password)
    #[serde(default)]
    pub deferred_casks: Vec<String>,
    /// Hash of deferred_casks for change detection (notify once)
    #[serde(default)]
    pub deferred_casks_hash: Option<String>,
    /// Dotfile paths dismissed when prompted to import from other profiles
    #[serde(default, skip_serializing_if = "std::collections::HashSet::is_empty")]
    pub dismissed_imports: std::collections::HashSet<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileState {
    pub hash: String,
    pub last_modified: DateTime<Utc>,
    pub synced: bool,
    /// Last hash confirmed on the remote while `synced` is false
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pushed_hash: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PackageState {
    /// When we last checked/synced this package manager
    pub last_sync: DateTime<Utc>,
    /// When the manifest content last changed
    #[serde(default)]
    pub last_modified: Option<DateTime<Utc>>,
    /// When packages were last installed/upgraded
    #[serde(default)]
    pub last_upgrade: Option<DateTime<Utc>>,
    pub hash: String,
}

/// Tracks a checkout of a project on this machine
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CheckoutInfo {
    pub path: PathBuf,
    /// Short hash for identification (first 8 chars of SHA256 of canonical path)
    pub checkout_id: String,
}

/// Machine state stored in sync repo for cross-machine comparison
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MachineState {
    pub machine_id: String,
    pub hostname: String,
    pub last_sync: DateTime<Utc>,
    #[serde(default)]
    pub os_version: String,
    #[serde(default)]
    pub cli_version: String,
    /// File paths and their hashes
    pub files: HashMap<String, String>,
    /// Package manager -> list of installed packages
    /// Keys: brew_formulae, brew_casks, brew_taps, npm, pnpm, bun, gem
    pub packages: HashMap<String, Vec<String>>,
    /// Package manager -> package -> installed version, for managers that report one
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub package_versions: HashMap<String, HashMap<String, String>>,
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
    /// Multiple checkouts per project URL (project_key -> list of checkouts)
    #[serde(default)]
    pub checkouts: HashMap<String, Vec<CheckoutInfo>>,
    /// Profile assigned to this machine (if any)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile: Option<String>,
}

impl Default for MachineState {
    fn default() -> Self {
        Self::new("unknown")
    }
}

/// Machine ids name files in the sync repo and entries in the trust store, so only
/// plain names pass.
pub fn valid_machine_id(id: &str) -> bool {
    id.len() <= 64
        && id.chars().next().is_some_and(|c| c.is_ascii_alphanumeric())
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
}

pub fn local_hostname() -> String {
    hostname::get()
        .ok()
        .and_then(|h| h.into_string().ok())
        .unwrap_or_else(|| "unknown".to_string())
}

pub fn local_os_version() -> String {
    if cfg!(target_os = "macos") {
        std::process::Command::new("sw_vers")
            .arg("-productVersion")
            .output()
            .ok()
            .and_then(|o| String::from_utf8(o.stdout).ok())
            .map(|v| format!("macOS {}", v.trim()))
            .unwrap_or_default()
    } else {
        std::fs::read_to_string("/etc/os-release")
            .ok()
            .and_then(|s| {
                s.lines()
                    .find_map(|l| l.strip_prefix("PRETTY_NAME="))
                    .map(|v| v.trim_matches('"').to_string())
            })
            .unwrap_or_default()
    }
}

impl MachineState {
    pub fn new(machine_id: &str) -> Self {
        Self {
            machine_id: machine_id.to_string(),
            hostname: local_hostname(),
            last_sync: Utc::now(),
            os_version: local_os_version(),
            cli_version: env!("CARGO_PKG_VERSION").to_string(),
            files: HashMap::new(),
            packages: HashMap::new(),
            package_versions: HashMap::new(),
            removed_packages: HashMap::new(),
            dotfiles: Vec::new(),
            ignored_dotfiles: Vec::new(),
            project_configs: HashMap::new(),
            ignored_project_configs: HashMap::new(),
            checkouts: HashMap::new(),
            profile: None,
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
    pub(crate) fn validate(&mut self) -> Result<()> {
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

        // Versions become part of install specs on other machines, and a dist-tag would
        // outrank every real version in the manifest union
        for (manager, versions) in self.package_versions.iter_mut() {
            let ecosystem = crate::packages::manager_for_key(manager).map(|m| m.ecosystem());
            versions.retain(|name, version| {
                Self::is_safe_package_name(name)
                    && ecosystem
                        .is_some_and(|eco| crate::packages::validate_version(eco, version).is_ok())
            });
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
        if state.machine_id != machine_id {
            anyhow::bail!(
                "machines/{}.json names machine {}",
                machine_id,
                state.machine_id
            );
        }
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
                let stem = path
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .unwrap_or_default();
                if !valid_machine_id(stem) {
                    crate::cli::Output::warning(&format!(
                        "Ignoring {}: invalid machine id",
                        path.display()
                    ));
                    continue;
                }
                if let Ok(content) = std::fs::read_to_string(&path) {
                    if let Ok(mut state) = serde_json::from_str::<MachineState>(&content) {
                        // A record under another file name could pose as another machine
                        if stem != state.machine_id {
                            crate::cli::Output::warning(&format!(
                                "Ignoring {}: it claims to be machine {}",
                                path.display(),
                                state.machine_id
                            ));
                            continue;
                        }
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

    /// The newest version of each package that any machine runs, so a new machine
    /// installs a release some machine already uses.
    pub fn compute_union_versions(machines: &[Self]) -> HashMap<String, HashMap<String, String>> {
        let mut union: HashMap<String, HashMap<String, String>> = HashMap::new();
        for machine in machines {
            for (manager, versions) in &machine.package_versions {
                let pins = union.entry(manager.clone()).or_default();
                for (name, version) in versions {
                    let newer = pins.get(name).is_none_or(|current| {
                        crate::packages::pin::compare_versions(version, current).is_gt()
                    });
                    if newer {
                        pins.insert(name.clone(), version.clone());
                    }
                }
            }
        }
        union
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
        let home = crate::home_dir()?;
        Ok(home.join(".tether").join("state.json"))
    }

    pub fn load() -> Result<Self> {
        let path = Self::state_path()?;
        if !path.exists() {
            // The random machine id must survive across calls, so persist on first load.
            let state = Self::new();
            state.save()?;
            return Ok(state);
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
            // Random, not the hostname: hostnames are not unique across a fleet.
            machine_id: crate::security::random_hex_id(),
            last_sync: Utc::now(),
            files: HashMap::new(),
            packages: HashMap::new(),
            last_upgrade: None,
            last_upgrade_with_updates: None,
            deferred_casks: Vec::new(),
            deferred_casks_hash: None,
            dismissed_imports: std::collections::HashSet::new(),
        }
    }

    pub fn update_file(&mut self, path: &str, hash: String) {
        let pushed_hash = self.files.get(path).and_then(|f| {
            if f.synced {
                Some(f.hash.clone())
            } else {
                f.pushed_hash.clone()
            }
        });
        self.files.insert(
            path.to_string(),
            FileState {
                hash,
                last_modified: Utc::now(),
                synced: false,
                pushed_hash,
            },
        );
    }

    /// Record a hash imported from a remote. It is already confirmed there, so
    /// discard_unpushed must not roll it back.
    pub fn record_remote_file(&mut self, path: &str, hash: String) {
        self.files.insert(
            path.to_string(),
            FileState {
                hash,
                last_modified: Utc::now(),
                synced: true,
                pushed_hash: None,
            },
        );
    }

    /// Roll unpushed file hashes back to their last pushed value, once a
    /// conflicting rebase has discarded the commit that held them. A stale hash
    /// makes conflict detection treat the local edit as synced.
    pub fn discard_unpushed(&mut self) {
        for file in self.files.values_mut() {
            if !file.synced {
                // Empty hash means the remote never had the file. Removing the
                // entry would take the first-sync path, where the remote wins.
                file.hash = file.pushed_hash.take().unwrap_or_default();
                file.synced = true;
            }
        }
    }

    pub fn mark_synced(&mut self) {
        self.last_sync = Utc::now();
        for file in self.files.values_mut() {
            file.synced = true;
            file.pushed_hash = None;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_safe_package_names() {
        assert!(MachineState::is_safe_package_name("git"));
        assert!(MachineState::is_safe_package_name("node-18.x"));
        assert!(MachineState::is_safe_package_name("@angular/cli"));
        assert!(MachineState::is_safe_package_name("python3.11"));
        assert!(MachineState::is_safe_package_name(&"a".repeat(256)));
    }

    #[test]
    fn test_unsafe_package_names_rejected() {
        // Shell injection
        assert!(!MachineState::is_safe_package_name("git; rm -rf /"));
        assert!(!MachineState::is_safe_package_name("$(whoami)"));
        assert!(!MachineState::is_safe_package_name("pkg`id`"));
        assert!(!MachineState::is_safe_package_name("pkg|cat /etc/passwd"));
        assert!(!MachineState::is_safe_package_name("pkg&background"));
        // Quotes/escapes
        assert!(!MachineState::is_safe_package_name("pkg'injection"));
        assert!(!MachineState::is_safe_package_name("pkg\"injection"));
        assert!(!MachineState::is_safe_package_name("pkg\\escape"));
        // Newlines
        assert!(!MachineState::is_safe_package_name("pkg\nmalicious"));
        assert!(!MachineState::is_safe_package_name("pkg\rmalicious"));
        // Empty / too long
        assert!(!MachineState::is_safe_package_name(""));
        assert!(!MachineState::is_safe_package_name(&"a".repeat(300)));
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
    fn test_compute_union_versions_takes_newest() {
        let versions = |pairs: &[(&str, &str)]| {
            HashMap::from([(
                "npm".to_string(),
                pairs
                    .iter()
                    .map(|(n, v)| (n.to_string(), v.to_string()))
                    .collect(),
            )])
        };
        let mut m1 = MachineState::new("m1");
        m1.package_versions = versions(&[("a", "1.10.0"), ("b", "2.0.0")]);
        let mut m2 = MachineState::new("m2");
        m2.package_versions = versions(&[("a", "1.9.0"), ("c", "0.1.0")]);

        let union = MachineState::compute_union_versions(&[m1, m2]);
        assert_eq!(
            union,
            versions(&[("a", "1.10.0"), ("b", "2.0.0"), ("c", "0.1.0")])
        );
    }

    #[test]
    fn test_validate_drops_unsafe_versions() {
        let mut state = MachineState::new("m");
        state.package_versions.insert(
            "npm".to_string(),
            HashMap::from([
                ("ok".to_string(), "1.0.0".to_string()),
                ("bad".to_string(), "../evil.tgz".to_string()),
            ]),
        );
        state.validate().unwrap();
        assert_eq!(state.package_versions["npm"].len(), 1);
        assert!(state.package_versions["npm"].contains_key("ok"));
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
        assert_eq!(loaded.cli_version, env!("CARGO_PKG_VERSION"));
        assert_eq!(
            loaded.packages.get("npm"),
            Some(&vec!["typescript".to_string()])
        );
        assert_eq!(loaded.files.get(".zshrc"), Some(&"abc123".to_string()));
    }

    #[test]
    fn test_checkout_info_roundtrip() {
        let info = CheckoutInfo {
            path: PathBuf::from("/Users/test/Projects/repo"),
            checkout_id: "abc12345".to_string(),
        };

        let json = serde_json::to_string(&info).unwrap();
        let loaded: CheckoutInfo = serde_json::from_str(&json).unwrap();

        assert_eq!(loaded.path, info.path);
        assert_eq!(loaded.checkout_id, info.checkout_id);
    }

    #[test]
    fn test_machine_state_checkouts_roundtrip() {
        let temp = TempDir::new().unwrap();
        let sync_path = temp.path();
        std::fs::create_dir_all(sync_path.join("machines")).unwrap();

        let mut state = MachineState::new("test-machine");
        state.checkouts.insert(
            "github.com/user/repo".to_string(),
            vec![
                CheckoutInfo {
                    path: PathBuf::from("/home/user/work/repo"),
                    checkout_id: "aabb1122".to_string(),
                },
                CheckoutInfo {
                    path: PathBuf::from("/home/user/personal/repo"),
                    checkout_id: "ccdd3344".to_string(),
                },
            ],
        );

        state.save_to_repo(sync_path).unwrap();

        let loaded = MachineState::load_from_repo(sync_path, "test-machine")
            .unwrap()
            .unwrap();

        let checkouts = loaded.checkouts.get("github.com/user/repo").unwrap();
        assert_eq!(checkouts.len(), 2);
        assert_eq!(checkouts[0].checkout_id, "aabb1122");
        assert_eq!(checkouts[1].checkout_id, "ccdd3344");
    }

    #[test]
    fn test_machine_state_new_has_cli_version() {
        let state = MachineState::new("test");
        assert!(!state.cli_version.is_empty());
        assert_eq!(state.cli_version, env!("CARGO_PKG_VERSION"));
    }

    #[test]
    fn test_machine_state_old_json_defaults_all_optional_fields() {
        let old_json = r#"{
            "machine_id": "test",
            "hostname": "test-host",
            "last_sync": "2024-01-01T00:00:00Z",
            "files": {},
            "packages": {}
        }"#;

        let loaded: MachineState = serde_json::from_str(old_json).unwrap();
        assert_eq!(loaded.cli_version, "");
        assert_eq!(loaded.os_version, "");
        assert!(loaded.checkouts.is_empty());
        assert!(loaded.removed_packages.is_empty());
        assert!(loaded.dotfiles.is_empty());
        assert!(loaded.ignored_dotfiles.is_empty());
        assert!(loaded.project_configs.is_empty());
        assert!(loaded.ignored_project_configs.is_empty());
    }

    #[test]
    fn test_machine_record_must_match_its_file_name() {
        let temp = TempDir::new().unwrap();
        let sync_path = temp.path();
        MachineState::new("real").save_to_repo(sync_path).unwrap();
        let impostor = serde_json::to_string(&MachineState::new("real")).unwrap();
        std::fs::write(sync_path.join("machines/extra.json"), &impostor).unwrap();
        std::fs::write(sync_path.join("machines/other.json"), &impostor).unwrap();
        let spaced = serde_json::to_string(&MachineState::new("a b")).unwrap();
        std::fs::write(sync_path.join("machines/a b.json"), &spaced).unwrap();

        let all = MachineState::list_all(sync_path).unwrap();
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].machine_id, "real");
        assert!(MachineState::load_from_repo(sync_path, "other").is_err());
    }

    #[test]
    fn test_valid_machine_id() {
        for id in ["a1b2c3", "MacBook-Pro.local", "m_1"] {
            assert!(valid_machine_id(id), "{}", id);
        }
        for id in [
            "",
            ".hidden",
            "-x",
            "a b",
            "a/b",
            "zz\nssh-ed25519",
            &"a".repeat(65),
        ] {
            assert!(!valid_machine_id(id), "{}", id);
        }
    }

    #[test]
    fn test_machine_state_load_nonexistent() {
        let temp = TempDir::new().unwrap();
        let result = MachineState::load_from_repo(temp.path(), "nonexistent").unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn test_package_state_timestamps_roundtrip() {
        let now = Utc::now();
        let state = PackageState {
            last_sync: now,
            last_modified: Some(now),
            last_upgrade: Some(now),
            hash: "abc123".to_string(),
        };

        let json = serde_json::to_string(&state).unwrap();
        let loaded: PackageState = serde_json::from_str(&json).unwrap();

        assert_eq!(loaded.last_sync, state.last_sync);
        assert_eq!(loaded.last_modified, state.last_modified);
        assert_eq!(loaded.last_upgrade, state.last_upgrade);
        assert_eq!(loaded.hash, state.hash);
    }

    #[test]
    fn test_package_state_missing_timestamps_defaults() {
        // Simulate old JSON without last_modified/last_upgrade fields
        let old_json = r#"{"last_sync":"2024-01-01T00:00:00Z","hash":"abc123"}"#;
        let loaded: PackageState = serde_json::from_str(old_json).unwrap();

        assert!(loaded.last_modified.is_none());
        assert!(loaded.last_upgrade.is_none());
        assert_eq!(loaded.hash, "abc123");
    }

    #[test]
    fn test_discard_unpushed_restores_pushed_hash() {
        let mut state = SyncState::new();
        state.update_file(".zshrc", "base".to_string());
        state.mark_synced();
        state.update_file(".zshrc", "edit1".to_string());
        state.update_file(".zshrc", "edit2".to_string());

        state.discard_unpushed();

        let file = &state.files[".zshrc"];
        assert_eq!(file.hash, "base");
        assert!(file.synced);
        assert!(file.pushed_hash.is_none());
    }

    #[test]
    fn test_discard_unpushed_empties_never_pushed_file() {
        let mut state = SyncState::new();
        state.update_file(".new", "hash".to_string());
        state.discard_unpushed();
        assert_eq!(state.files[".new"].hash, "");
    }

    #[test]
    fn test_discard_unpushed_keeps_remote_imports() {
        let mut state = SyncState::new();
        state.update_file("project", "base".to_string());
        state.mark_synced();
        state.record_remote_file("project", "remote".to_string());
        state.discard_unpushed();
        assert_eq!(state.files["project"].hash, "remote");
    }

    #[test]
    fn test_discard_unpushed_keeps_synced_files() {
        let mut state = SyncState::new();
        state.update_file(".zshrc", "base".to_string());
        state.mark_synced();
        state.discard_unpushed();
        assert_eq!(state.files[".zshrc"].hash, "base");
    }

    #[test]
    fn test_file_state_old_json_has_no_pushed_hash() {
        let old_json = r#"{"hash":"abc","last_modified":"2024-01-01T00:00:00Z","synced":true}"#;
        let loaded: FileState = serde_json::from_str(old_json).unwrap();
        assert!(loaded.pushed_hash.is_none());
    }
}

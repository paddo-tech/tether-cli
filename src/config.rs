use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    pub sync: SyncConfig,
    pub backend: BackendConfig,
    pub packages: PackagesConfig,
    pub dotfiles: DotfilesConfig,
    pub security: SecurityConfig,
    #[serde(default)]
    pub merge: MergeConfig,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub team: Option<TeamConfig>, // Deprecated: kept for backwards compatibility
    #[serde(skip_serializing_if = "Option::is_none")]
    pub teams: Option<TeamsConfig>,
    #[serde(default)]
    pub project_configs: ProjectConfigSettings,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SyncConfig {
    pub interval: String,
    pub strategy: ConflictStrategy,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ConflictStrategy {
    #[serde(rename = "last-write-wins")]
    LastWriteWins,
    #[serde(rename = "manual")]
    Manual,
    #[serde(rename = "machine-priority")]
    MachinePriority,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BackendConfig {
    #[serde(rename = "type")]
    pub backend_type: BackendType,
    pub url: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum BackendType {
    #[serde(rename = "git")]
    Git,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PackagesConfig {
    #[serde(default)]
    pub remove_unlisted: bool,
    pub brew: BrewConfig,
    pub npm: NpmConfig,
    pub pnpm: PnpmConfig,
    pub bun: BunConfig,
    pub gem: GemConfig,
    #[serde(default)]
    pub uv: UvConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BrewConfig {
    pub enabled: bool,
    pub sync_casks: bool,
    pub sync_taps: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NpmConfig {
    pub enabled: bool,
    pub sync_versions: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PnpmConfig {
    pub enabled: bool,
    pub sync_versions: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BunConfig {
    pub enabled: bool,
    pub sync_versions: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GemConfig {
    pub enabled: bool,
    pub sync_versions: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UvConfig {
    pub enabled: bool,
    pub sync_versions: bool,
}

impl Default for UvConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            sync_versions: false,
        }
    }
}

/// A dotfile entry - either a simple string path or an object with options
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum DotfileEntry {
    /// Simple string path (create_if_missing defaults to true)
    Simple(String),
    /// Object with explicit options
    WithOptions {
        path: String,
        #[serde(default = "default_create_if_missing")]
        create_if_missing: bool,
    },
}

fn default_create_if_missing() -> bool {
    true
}

impl DotfileEntry {
    pub fn path(&self) -> &str {
        match self {
            DotfileEntry::Simple(p) => p,
            DotfileEntry::WithOptions { path, .. } => path,
        }
    }

    pub fn create_if_missing(&self) -> bool {
        match self {
            DotfileEntry::Simple(_) => true,
            DotfileEntry::WithOptions {
                create_if_missing, ..
            } => *create_if_missing,
        }
    }

    /// Validates the path is safe (no path traversal, not absolute)
    pub fn is_safe_path(&self) -> bool {
        is_safe_dotfile_path(self.path())
    }
}

/// Validates a dotfile path is safe from path traversal attacks.
/// Rejects absolute paths and paths containing `..` components.
/// Allows `~` prefix (home-relative paths) as these are expanded safely.
pub fn is_safe_dotfile_path(path: &str) -> bool {
    // Strip leading ~/ for validation (it's expanded to home dir)
    let path_to_check = path.strip_prefix("~/").unwrap_or(path);

    // Reject absolute paths
    if path_to_check.starts_with('/') {
        return false;
    }

    // Reject paths with .. components
    for component in path_to_check.split('/') {
        if component == ".." {
            return false;
        }
    }

    // Reject empty paths
    if path_to_check.is_empty() {
        return false;
    }

    true
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DotfilesConfig {
    pub files: Vec<DotfileEntry>,
    #[serde(default)]
    pub dirs: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SecurityConfig {
    pub encrypt_dotfiles: bool,
    pub scan_secrets: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MergeConfig {
    /// Command to launch for three-way merge (default: opendiff on macOS, vimdiff elsewhere)
    #[serde(default = "default_merge_command")]
    pub command: String,
    /// Arguments for merge command. Use {local}, {remote}, {merged} placeholders.
    #[serde(default = "default_merge_args")]
    pub args: Vec<String>,
}

fn default_merge_command() -> String {
    if cfg!(target_os = "macos") {
        "opendiff".to_string()
    } else {
        "vimdiff".to_string()
    }
}

fn default_merge_args() -> Vec<String> {
    if cfg!(target_os = "macos") {
        vec![
            "{local}".to_string(),
            "{remote}".to_string(),
            "-merge".to_string(),
            "{merged}".to_string(),
        ]
    } else {
        vec![
            "{local}".to_string(),
            "{remote}".to_string(),
            "{merged}".to_string(),
        ]
    }
}

/// Allowed merge tool commands (security: prevents arbitrary command execution via synced config)
const ALLOWED_MERGE_TOOLS: &[&str] = &[
    "opendiff",
    "vimdiff",
    "nvim",
    "vim",
    "gvimdiff",
    "meld",
    "kdiff3",
    "diffmerge",
    "p4merge",
    "araxis",
    "bc",
    "bc3",
    "bc4",
    "beyondcompare",
    "deltawalker",
    "diffuse",
    "ecmerge",
    "emerge",
    "examdiff",
    "guiffy",
    "gvim",
    "idea",
    "intellij",
    "code",
    "vscode",
    "sublime",
    "subl",
    "tkdiff",
    "tortoisemerge",
    "winmerge",
    "xxdiff",
];

impl MergeConfig {
    /// Validates the merge tool command is in the allowlist
    pub fn is_valid_command(&self) -> bool {
        // Extract base command name (without path)
        let cmd = self
            .command
            .rsplit('/')
            .next()
            .unwrap_or(&self.command)
            .to_lowercase();
        ALLOWED_MERGE_TOOLS.contains(&cmd.as_str())
    }
}

impl Default for MergeConfig {
    fn default() -> Self {
        Self {
            command: default_merge_command(),
            args: default_merge_args(),
        }
    }
}

/// Team sync configuration.
///
/// Team repositories are NOT encrypted by Tether for these reasons:
/// - Multiple team members need access (key distribution is complex)
/// - Team repos should only contain non-sensitive shared configs
/// - Git access controls already protect the repository
/// - Sensitive team data should use proper secrets management (1Password, Vault, etc.)
///
/// Secret scanning is performed when adding a team repository to warn about
/// potential sensitive data that shouldn't be in team configs.
///
/// Access modes:
/// - read_only: true - Pull team configs only (regular team members)
/// - read_only: false - Can push updates to team repo (admins/contributors)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TeamConfig {
    pub enabled: bool,
    pub url: String,
    pub auto_inject: bool,
    pub read_only: bool,
}

/// Multi-team sync configuration.
///
/// Supports multiple team repositories with easy switching between them.
/// Only one team can be active at a time, but you can quickly switch between
/// different teams (e.g., different clients, company vs open source).
///
/// Team names are automatically extracted from the Git URL's organization/owner
/// (e.g., git@github.com:acme-corp/dotfiles.git → "acme-corp") but can be overridden.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct TeamsConfig {
    /// Currently active team (if any)
    pub active: Option<String>,
    /// Map of team name -> team configuration
    pub teams: HashMap<String, TeamConfig>,
}

/// Project-local config syncing.
///
/// Syncs gitignored config files from project directories (e.g., .env.local).
/// Files are identified by git remote URL, so the same project on different
/// machines (even in different paths) will sync correctly.
///
/// Safety features:
/// - only_if_gitignored: Only sync files that are in .gitignore
/// - Secret scanning: Warns about potential secrets before syncing
/// - Encryption: All project configs are encrypted like dotfiles
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProjectConfigSettings {
    pub enabled: bool,
    pub search_paths: Vec<String>,
    pub patterns: Vec<String>,
    pub only_if_gitignored: bool,
}

impl Default for ProjectConfigSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            search_paths: vec!["~/Projects".to_string(), "~/Code".to_string()],
            patterns: vec![
                ".env.local".to_string(),
                "appsettings.*.json".to_string(),
                ".vscode/settings.json".to_string(),
            ],
            only_if_gitignored: true,
        }
    }
}

impl Config {
    pub fn config_dir() -> Result<PathBuf> {
        let home =
            home::home_dir().ok_or_else(|| anyhow::anyhow!("Could not find home directory"))?;
        Ok(home.join(".tether"))
    }

    pub fn config_path() -> Result<PathBuf> {
        Ok(Self::config_dir()?.join("config.toml"))
    }

    /// Get team sync directory for a specific team (or legacy single team)
    pub fn team_sync_dir() -> Result<PathBuf> {
        Ok(Self::config_dir()?.join("team-sync")) // Legacy single-team path
    }

    /// Get team directory for a specific named team
    pub fn team_dir(team_name: &str) -> Result<PathBuf> {
        Ok(Self::config_dir()?.join("teams").join(team_name))
    }

    /// Get sync directory for a specific named team
    pub fn team_repo_dir(team_name: &str) -> Result<PathBuf> {
        Ok(Self::team_dir(team_name)?.join("sync"))
    }

    /// Get active team configuration
    pub fn active_team(&self) -> Option<(String, &TeamConfig)> {
        let teams = self.teams.as_ref()?;
        let active_name = teams.active.as_ref()?;
        let team_config = teams.teams.get(active_name)?;
        Some((active_name.clone(), team_config))
    }

    pub fn load() -> Result<Self> {
        let path = Self::config_path()?;
        let content = std::fs::read_to_string(path)?;
        Ok(toml::from_str(&content)?)
    }

    pub fn save(&self) -> Result<()> {
        let path = Self::config_path()?;
        let content = toml::to_string_pretty(self)?;
        crate::sync::atomic_write(&path, content.as_bytes())
    }
}

impl Default for Config {
    fn default() -> Self {
        Self {
            sync: SyncConfig {
                interval: "5m".to_string(),
                strategy: ConflictStrategy::LastWriteWins,
            },
            backend: BackendConfig {
                backend_type: BackendType::Git,
                url: String::new(),
            },
            packages: PackagesConfig {
                remove_unlisted: false,
                brew: BrewConfig {
                    enabled: true,
                    sync_casks: true,
                    sync_taps: true,
                },
                npm: NpmConfig {
                    enabled: true,
                    sync_versions: false,
                },
                pnpm: PnpmConfig {
                    enabled: true,
                    sync_versions: false,
                },
                bun: BunConfig {
                    enabled: true,
                    sync_versions: false,
                },
                gem: GemConfig {
                    enabled: true,
                    sync_versions: false,
                },
                uv: UvConfig::default(),
            },
            dotfiles: DotfilesConfig {
                files: vec![
                    // Shell configs - don't create on machines that don't have them
                    DotfileEntry::WithOptions {
                        path: ".zshrc".to_string(),
                        create_if_missing: false,
                    },
                    DotfileEntry::WithOptions {
                        path: ".zprofile".to_string(),
                        create_if_missing: false,
                    },
                    DotfileEntry::WithOptions {
                        path: ".zshenv".to_string(),
                        create_if_missing: false,
                    },
                    DotfileEntry::WithOptions {
                        path: ".bashrc".to_string(),
                        create_if_missing: false,
                    },
                    DotfileEntry::WithOptions {
                        path: ".bash_profile".to_string(),
                        create_if_missing: false,
                    },
                    DotfileEntry::WithOptions {
                        path: ".profile".to_string(),
                        create_if_missing: false,
                    },
                    // Common configs - create on all machines
                    DotfileEntry::Simple(".gitconfig".to_string()),
                    // Note: .tether/config.toml is always synced (hardcoded in sync logic)
                ],
                dirs: vec![],
            },
            security: SecurityConfig {
                encrypt_dotfiles: true,
                scan_secrets: true,
            },
            merge: MergeConfig::default(),
            team: None,
            teams: None,
            project_configs: ProjectConfigSettings::default(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Path safety tests
    #[test]
    fn test_safe_dotfile_path_simple() {
        assert!(is_safe_dotfile_path(".zshrc"));
        assert!(is_safe_dotfile_path(".config/nvim/init.lua"));
        assert!(is_safe_dotfile_path(".local/share/data"));
    }

    #[test]
    fn test_safe_dotfile_path_with_tilde() {
        assert!(is_safe_dotfile_path("~/.zshrc"));
        assert!(is_safe_dotfile_path("~/.config/zsh"));
    }

    #[test]
    fn test_unsafe_path_traversal() {
        assert!(!is_safe_dotfile_path("../../../etc/passwd"));
        assert!(!is_safe_dotfile_path(".config/../../../etc/passwd"));
        assert!(!is_safe_dotfile_path("foo/bar/../../../etc/passwd"));
    }

    #[test]
    fn test_unsafe_path_traversal_after_tilde() {
        assert!(!is_safe_dotfile_path("~/../etc/passwd"));
        assert!(!is_safe_dotfile_path("~/foo/../../../etc/passwd"));
    }

    #[test]
    fn test_unsafe_absolute_path() {
        assert!(!is_safe_dotfile_path("/etc/passwd"));
        assert!(!is_safe_dotfile_path("/Users/foo/.zshrc"));
    }

    #[test]
    fn test_unsafe_empty_path() {
        assert!(!is_safe_dotfile_path(""));
    }

    #[test]
    fn test_tilde_only_is_valid() {
        // "~" alone is valid - it refers to home directory
        // (strip_prefix("~/") doesn't match "~", so "~" remains as-is)
        assert!(is_safe_dotfile_path("~"));
    }

    // Merge tool validation tests
    #[test]
    fn test_valid_merge_tools() {
        let tools = ["vimdiff", "opendiff", "meld", "code", "nvim", "kdiff3"];
        for tool in tools {
            let config = MergeConfig {
                command: tool.to_string(),
                args: vec![],
            };
            assert!(config.is_valid_command(), "{} should be valid", tool);
        }
    }

    #[test]
    fn test_valid_merge_tool_with_path() {
        let config = MergeConfig {
            command: "/usr/bin/opendiff".to_string(),
            args: vec![],
        };
        assert!(config.is_valid_command());

        let config = MergeConfig {
            command: "/Applications/Visual Studio Code.app/Contents/Resources/app/bin/code"
                .to_string(),
            args: vec![],
        };
        assert!(config.is_valid_command());
    }

    #[test]
    fn test_invalid_merge_tool() {
        let invalid = ["rm", "cat", "bash", "sh", "curl", "wget", "malicious"];
        for tool in invalid {
            let config = MergeConfig {
                command: tool.to_string(),
                args: vec![],
            };
            assert!(!config.is_valid_command(), "{} should be invalid", tool);
        }
    }

    #[test]
    fn test_merge_tool_case_insensitive() {
        let config = MergeConfig {
            command: "VIMDIFF".to_string(),
            args: vec![],
        };
        assert!(config.is_valid_command());
    }

    // DotfileEntry tests
    #[test]
    fn test_dotfile_entry_simple_path() {
        let entry = DotfileEntry::Simple(".zshrc".to_string());
        assert_eq!(entry.path(), ".zshrc");
        assert!(entry.create_if_missing());
    }

    #[test]
    fn test_dotfile_entry_with_options() {
        let entry = DotfileEntry::WithOptions {
            path: ".bashrc".to_string(),
            create_if_missing: false,
        };
        assert_eq!(entry.path(), ".bashrc");
        assert!(!entry.create_if_missing());
    }

    #[test]
    fn test_dotfile_entry_is_safe_path() {
        let safe = DotfileEntry::Simple(".zshrc".to_string());
        assert!(safe.is_safe_path());

        let unsafe_entry = DotfileEntry::Simple("../../../etc/passwd".to_string());
        assert!(!unsafe_entry.is_safe_path());
    }

    // Config default tests
    #[test]
    fn test_config_default_has_gitconfig() {
        let config = Config::default();
        let has_gitconfig = config
            .dotfiles
            .files
            .iter()
            .any(|e| e.path() == ".gitconfig");
        assert!(has_gitconfig);
    }

    #[test]
    fn test_config_default_sync_interval() {
        let config = Config::default();
        assert_eq!(config.sync.interval, "5m");
    }

    // Serialization tests
    #[test]
    fn test_conflict_strategy_in_config() {
        // Test via full config serialization (enum can't be serialized standalone in toml)
        let config = Config::default();
        let toml_str = toml::to_string_pretty(&config).unwrap();
        assert!(toml_str.contains("last-write-wins"));
    }

    #[test]
    fn test_config_toml_roundtrip() {
        let config = Config::default();
        let toml_str = toml::to_string_pretty(&config).unwrap();
        let parsed: Config = toml::from_str(&toml_str).unwrap();
        assert_eq!(config.sync.interval, parsed.sync.interval);
        assert_eq!(config.dotfiles.files.len(), parsed.dotfiles.files.len());
    }
}

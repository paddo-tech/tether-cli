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
        let dir = Self::config_dir()?;
        std::fs::create_dir_all(&dir)?;

        let path = Self::config_path()?;
        let content = toml::to_string_pretty(self)?;
        std::fs::write(path, content)?;
        Ok(())
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
                    DotfileEntry::Simple(".tether/config.toml".to_string()),
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

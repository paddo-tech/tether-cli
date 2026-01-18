use anyhow::{bail, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;

/// Config format version. Bump when making breaking changes that require migration.
///
/// Version history:
/// - v1 (1.0.0+): Initial format. All fields have serde defaults for backwards compat.
///
/// When bumping to v2:
/// 1. Add migration logic in load() before version check
/// 2. Document what changed and why migration is needed
/// 3. Freeze v1 semantics - don't add new defaults to v1 fields
pub const CURRENT_CONFIG_VERSION: u32 = 1;

fn default_config_version() -> u32 {
    1
}

fn is_false(b: &bool) -> bool {
    !*b
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    /// Config format version - prevents older tether from corrupting newer configs
    #[serde(default = "default_config_version")]
    pub config_version: u32,
    /// Team-only mode: no personal dotfiles/packages, only team sync
    /// DEPRECATED: Use features.personal_dotfiles and features.personal_packages instead
    #[serde(default, skip_serializing_if = "is_false")]
    pub team_only: bool,
    /// Feature toggles for what tether should sync
    #[serde(default)]
    pub features: FeaturesConfig,
    pub sync: SyncConfig,
    pub backend: BackendConfig,
    pub packages: PackagesConfig,
    pub dotfiles: DotfilesConfig,
    #[serde(default)]
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

/// Feature toggles - what tether should sync
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FeaturesConfig {
    /// Sync personal dotfiles (.zshrc, .gitconfig, etc.)
    #[serde(default = "default_true")]
    pub personal_dotfiles: bool,

    /// Sync and upgrade packages (brew, npm, etc.)
    #[serde(default = "default_true")]
    pub personal_packages: bool,

    /// Sync team dotfiles (requires team setup)
    #[serde(default)]
    pub team_dotfiles: bool,

    /// Share project secrets with collaborators (GitHub write access)
    #[serde(default)]
    pub collab_secrets: bool,

    /// Merge team + personal dotfiles (experimental, hidden)
    #[serde(default)]
    pub team_layering: bool,
}

impl Default for FeaturesConfig {
    fn default() -> Self {
        Self {
            personal_dotfiles: true,
            personal_packages: true,
            team_dotfiles: false,
            collab_secrets: false,
            team_layering: false,
        }
    }
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
    #[serde(default = "default_brew_config")]
    pub brew: BrewConfig,
    #[serde(default = "default_npm_config")]
    pub npm: NpmConfig,
    #[serde(default = "default_pnpm_config")]
    pub pnpm: PnpmConfig,
    #[serde(default = "default_bun_config")]
    pub bun: BunConfig,
    #[serde(default = "default_gem_config")]
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

fn default_brew_config() -> BrewConfig {
    BrewConfig {
        enabled: true,
        sync_casks: true,
        sync_taps: true,
    }
}

fn default_npm_config() -> NpmConfig {
    NpmConfig {
        enabled: true,
        sync_versions: false,
    }
}

fn default_pnpm_config() -> PnpmConfig {
    PnpmConfig {
        enabled: true,
        sync_versions: false,
    }
}

fn default_bun_config() -> BunConfig {
    BunConfig {
        enabled: true,
        sync_versions: false,
    }
}

fn default_gem_config() -> GemConfig {
    GemConfig {
        enabled: true,
        sync_versions: false,
    }
}

impl Default for SecurityConfig {
    fn default() -> Self {
        Self {
            encrypt_dotfiles: true,
            scan_secrets: true,
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
    /// Organizations that map to this team (full format: "github.com/org-name")
    /// Projects belonging to these orgs will use team secrets instead of personal sync
    #[serde(default)]
    pub orgs: Vec<String>,
}

/// Multi-team sync configuration.
///
/// Supports multiple team repositories active simultaneously.
/// Teams can be layered - e.g., company-wide + project-specific.
///
/// Team names are automatically extracted from the Git URL's organization/owner
/// (e.g., git@github.com:acme-corp/dotfiles.git → "acme-corp") but can be overridden.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct TeamsConfig {
    /// Currently active teams (supports multiple)
    /// Backwards compatible: accepts both "team-name" and ["team1", "team2"]
    #[serde(default, deserialize_with = "deserialize_active_teams")]
    pub active: Vec<String>,
    /// Map of team name -> team configuration
    pub teams: HashMap<String, TeamConfig>,
    /// Allowed GitHub organizations for team repos (empty = no restriction)
    #[serde(default)]
    pub allowed_orgs: Vec<String>,
    /// Collaborator-based project secret sharing (keyed by collab name)
    #[serde(default)]
    pub collabs: HashMap<String, CollabConfig>,
}

/// Collaborator-based project secret sharing configuration.
///
/// Unlike teams which are org-scoped, collabs are repo-scoped.
/// Collaborators are determined by GitHub write access to the project repo.
/// One collab repo can serve multiple project repos if they share collaborators.
///
/// Security note: Collaborator access is cached locally. Run `tether collab refresh`
/// to sync with current GitHub permissions. Revoked users retain access until refresh.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CollabConfig {
    /// Sync repo URL for this collaboration
    pub sync_url: String,
    /// Projects sharing secrets via this collab (normalized URLs like github.com/user/repo)
    #[serde(default)]
    pub projects: Vec<String>,
    /// Cache of collaborator GitHub usernames (for display)
    #[serde(default)]
    pub members_cache: Vec<String>,
    /// Last collaborator refresh timestamp
    #[serde(default)]
    pub last_refresh: Option<DateTime<Utc>>,
    /// Whether this collab is enabled
    #[serde(default = "default_true")]
    pub enabled: bool,
}

fn default_true() -> bool {
    true
}

/// Custom deserializer to handle both old (string) and new (array) formats
fn deserialize_active_teams<'de, D>(deserializer: D) -> Result<Vec<String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    use serde::de::{self, Visitor};
    use std::fmt;

    struct ActiveTeamsVisitor;

    impl<'de> Visitor<'de> for ActiveTeamsVisitor {
        type Value = Vec<String>;

        fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
            formatter.write_str("a string or array of strings")
        }

        fn visit_none<E>(self) -> Result<Self::Value, E>
        where
            E: de::Error,
        {
            Ok(Vec::new())
        }

        fn visit_some<D>(self, deserializer: D) -> Result<Self::Value, D::Error>
        where
            D: serde::Deserializer<'de>,
        {
            deserializer.deserialize_any(self)
        }

        fn visit_str<E>(self, value: &str) -> Result<Self::Value, E>
        where
            E: de::Error,
        {
            Ok(vec![value.to_string()])
        }

        fn visit_string<E>(self, value: String) -> Result<Self::Value, E>
        where
            E: de::Error,
        {
            Ok(vec![value])
        }

        fn visit_seq<A>(self, mut seq: A) -> Result<Self::Value, A::Error>
        where
            A: de::SeqAccess<'de>,
        {
            let mut teams = Vec::new();
            while let Some(team) = seq.next_element::<String>()? {
                teams.push(team);
            }
            Ok(teams)
        }
    }

    deserializer.deserialize_option(ActiveTeamsVisitor)
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
                ".env*".to_string(),              // .env, .env.local, .env.development, etc.
                ".dev.vars".to_string(),          // Cloudflare Workers
                "appsettings.*.json".to_string(), // .NET
                ".vscode/settings.json".to_string(),
                ".idea/**".to_string(),               // JetBrains
                "*.xcconfig".to_string(),             // Xcode
                "*service-account*.json".to_string(), // GCP
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

    /// Get first active team configuration (for backwards compatibility)
    pub fn active_team(&self) -> Option<(String, &TeamConfig)> {
        let teams = self.teams.as_ref()?;
        let active_name = teams.active.first()?;
        let team_config = teams.teams.get(active_name)?;
        Some((active_name.clone(), team_config))
    }

    /// Get all active team configurations
    pub fn active_teams(&self) -> Vec<(String, &TeamConfig)> {
        let Some(teams) = self.teams.as_ref() else {
            return Vec::new();
        };

        teams
            .active
            .iter()
            .filter_map(|name| teams.teams.get(name).map(|cfg| (name.clone(), cfg)))
            .collect()
    }

    /// Check if a team is active
    pub fn is_team_active(&self, team_name: &str) -> bool {
        self.teams
            .as_ref()
            .map(|t| t.active.iter().any(|n| n == team_name))
            .unwrap_or(false)
    }

    /// Check if any personal features are enabled (dotfiles or packages)
    pub fn has_personal_features(&self) -> bool {
        // Legacy team_only flag disables personal features
        if self.team_only {
            return false;
        }
        self.features.personal_dotfiles || self.features.personal_packages
    }

    /// Check if any team features are enabled (team dotfiles or collab secrets)
    pub fn has_team_features(&self) -> bool {
        self.features.team_dotfiles || self.features.collab_secrets
    }

    /// Check if personal repo is configured
    pub fn has_personal_repo(&self) -> bool {
        !self.backend.url.is_empty()
    }

    /// Get collab directory for a specific collab name
    pub fn collab_dir(collab_name: &str) -> Result<PathBuf> {
        // Defense-in-depth: validate collab name to prevent path traversal
        if collab_name.is_empty()
            || collab_name.contains('/')
            || collab_name.contains('\\')
            || collab_name.contains("..")
            || collab_name.starts_with('.')
        {
            anyhow::bail!("Invalid collab name: {}", collab_name);
        }
        Ok(Self::config_dir()?.join("collabs").join(collab_name))
    }

    /// Get sync directory for a specific collab
    pub fn collab_repo_dir(collab_name: &str) -> Result<PathBuf> {
        Ok(Self::collab_dir(collab_name)?.join("sync"))
    }

    /// Get collab config for a project (if any)
    pub fn collab_for_project(&self, normalized_url: &str) -> Option<(String, &CollabConfig)> {
        let teams = self.teams.as_ref()?;
        for (name, collab) in &teams.collabs {
            if collab.enabled && collab.projects.iter().any(|p| p == normalized_url) {
                return Some((name.clone(), collab));
            }
        }
        None
    }

    pub fn load() -> Result<Self> {
        let path = Self::config_path()?;
        let content = std::fs::read_to_string(path)?;
        let mut config: Self = toml::from_str(&content)?;

        if config.config_version > CURRENT_CONFIG_VERSION {
            bail!(
                "Config version {} is newer than this tether version supports (max: {}). \
                 Please upgrade tether: brew upgrade tether",
                config.config_version,
                CURRENT_CONFIG_VERSION
            );
        }

        // Migrate legacy team_only flag to features
        if config.team_only {
            config.features.personal_dotfiles = false;
            config.features.personal_packages = false;
        }

        Ok(config)
    }

    pub fn save(&self) -> Result<()> {
        let mut config = self.clone();
        config.config_version = CURRENT_CONFIG_VERSION;

        let path = Self::config_path()?;
        let content = toml::to_string_pretty(&config)?;
        crate::sync::atomic_write(&path, content.as_bytes())
    }
}

impl Default for Config {
    fn default() -> Self {
        Self {
            config_version: CURRENT_CONFIG_VERSION,
            team_only: false,
            features: FeaturesConfig::default(),
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

    #[test]
    fn test_backwards_compat_minimal_config() {
        // Minimal config from v1.0.0 - missing security, bun, gem, uv, merge, etc.
        let old_config = r#"
[sync]
interval = "5m"
strategy = "last-write-wins"

[backend]
type = "git"
url = "git@github.com:user/dotfiles.git"

[packages.brew]
enabled = true
sync_casks = true
sync_taps = true

[packages.npm]
enabled = true
sync_versions = false

[dotfiles]
files = [".zshrc", ".gitconfig"]
"#;
        let parsed: Config = toml::from_str(old_config).unwrap();
        assert_eq!(parsed.sync.interval, "5m");
        // Missing sections should have defaults
        assert!(parsed.security.encrypt_dotfiles);
        assert!(parsed.security.scan_secrets);
        assert!(parsed.packages.pnpm.enabled);
        assert!(parsed.packages.bun.enabled);
        assert!(parsed.packages.gem.enabled);
        assert!(parsed.packages.uv.enabled);
        assert_eq!(parsed.dotfiles.files.len(), 2);
    }

    #[test]
    fn test_backwards_compat_string_dotfiles() {
        // Old format used Vec<String> for dotfiles, now uses DotfileEntry
        let old_config = r#"
[sync]
interval = "5m"
strategy = "last-write-wins"

[backend]
type = "git"
url = "git@github.com:user/dotfiles.git"

[packages.brew]
enabled = true
sync_casks = true
sync_taps = true

[packages.npm]
enabled = true
sync_versions = false

[dotfiles]
files = [".zshrc", ".gitconfig", ".config/nvim/init.lua"]
"#;
        let parsed: Config = toml::from_str(old_config).unwrap();
        assert_eq!(parsed.dotfiles.files.len(), 3);
        assert_eq!(parsed.dotfiles.files[0].path(), ".zshrc");
        assert!(parsed.dotfiles.files[0].create_if_missing()); // Default for Simple
    }

    #[test]
    fn test_config_version_defaults_to_1() {
        // Config without version field should default to 1
        let old_config = r#"
[sync]
interval = "5m"
strategy = "last-write-wins"

[backend]
type = "git"
url = "git@github.com:user/dotfiles.git"

[packages.brew]
enabled = true
sync_casks = true
sync_taps = true

[packages.npm]
enabled = true
sync_versions = false

[dotfiles]
files = [".zshrc"]
"#;
        let parsed: Config = toml::from_str(old_config).unwrap();
        assert_eq!(parsed.config_version, 1);
    }

    #[test]
    fn test_config_version_preserved() {
        let config = r#"
config_version = 1

[sync]
interval = "5m"
strategy = "last-write-wins"

[backend]
type = "git"
url = "git@github.com:user/dotfiles.git"

[packages.brew]
enabled = true
sync_casks = true
sync_taps = true

[packages.npm]
enabled = true
sync_versions = false

[dotfiles]
files = [".zshrc"]
"#;
        let parsed: Config = toml::from_str(config).unwrap();
        assert_eq!(parsed.config_version, 1);
    }

    #[test]
    fn test_config_default_has_current_version() {
        let config = Config::default();
        assert_eq!(config.config_version, CURRENT_CONFIG_VERSION);
    }

    #[test]
    fn test_team_only_migration_to_features() {
        // Legacy config with team_only = true should disable personal features
        let old_config = r#"
team_only = true

[sync]
interval = "5m"
strategy = "last-write-wins"

[backend]
type = "git"
url = ""

[packages.brew]
enabled = false
sync_casks = true
sync_taps = true

[packages.npm]
enabled = false
sync_versions = false

[dotfiles]
files = []
"#;
        let mut parsed: Config = toml::from_str(old_config).unwrap();

        // Simulate the migration logic from Config::load()
        if parsed.team_only {
            parsed.features.personal_dotfiles = false;
            parsed.features.personal_packages = false;
        }

        // Verify migration worked
        assert!(!parsed.has_personal_features());
        assert!(!parsed.features.personal_dotfiles);
        assert!(!parsed.features.personal_packages);
    }

    #[test]
    fn test_features_default_enabled() {
        // Fresh config should have personal features enabled by default
        let config = Config::default();
        assert!(config.features.personal_dotfiles);
        assert!(config.features.personal_packages);
        assert!(!config.features.team_dotfiles);
        assert!(!config.features.collab_secrets);
        assert!(!config.features.team_layering);
        assert!(config.has_personal_features());
    }

    #[test]
    fn test_has_personal_features_respects_legacy_flag() {
        let mut config = Config::default();
        assert!(config.has_personal_features());

        // Legacy team_only overrides features
        config.team_only = true;
        assert!(!config.has_personal_features());
    }

    #[test]
    fn test_has_team_features() {
        let mut config = Config::default();
        assert!(!config.has_team_features());

        config.features.team_dotfiles = true;
        assert!(config.has_team_features());

        config.features.team_dotfiles = false;
        config.features.collab_secrets = true;
        assert!(config.has_team_features());
    }
}

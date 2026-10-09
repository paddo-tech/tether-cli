use anyhow::{bail, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;

/// Config format version. Bump when making breaking changes that require migration.
///
/// Version history:
/// - v1 (1.0.0+): Initial format. All fields have serde defaults for backwards compat.
/// - v2 (1.11.0+): Profiles become source of truth with per-profile dotfile storage.
///   ProfileConfig gains dotfiles (Vec<ProfileDotfileEntry>), dirs (Vec<String>),
///   packages (Vec<String>). Old ProfilePackagesConfig removed.
///   Migration: creates "dev" profile from global dotfiles/dirs/packages.
pub const CURRENT_CONFIG_VERSION: u32 = 2;
pub const DEFAULT_PROFILE: &str = "dev";
/// Written as `config_writer` by Tether 2.0 and later. 1.x drops keys it does not know when
/// it saves config.toml, so a synced config without it came from 1.x, and a key it lacks
/// was not deleted. A constant, so an upgrade does not change the synced config.
pub const CONFIG_WRITER: u32 = 2;

/// Whether 1.x fails to load a config.toml without the key
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum V1Key {
    Required,
    Optional,
}

/// The config.toml keys that every 1.x release since 1.11.10 reads and writes: the
/// intersection of the `Config` structs of 1.11.10, 1.12.0 and 1.13.1. They are the same,
/// except for `on_conflict` in dotfile entries, which only 1.13 has. The intersection, not the
/// union: a 1.11 or 1.12 save drops `on_conflict`, so a missing one is no deletion. Only a
/// key in this list that a 1.x copy lacks is a deletion or a skipped default.
///
/// A key is a dotted path. `*` matches any key of a map and `[]` any item of a list. A table
/// that holds a listed key is known too: `team` is known through `team.url`. A key is
/// `Required` when 1.x has no default for it: when its table is present, 1.x needs it.
pub const V1_KEYS: &[(&str, V1Key)] = &[
    ("config_version", V1Key::Optional),
    ("team_only", V1Key::Optional),
    ("features.personal_dotfiles", V1Key::Optional),
    ("features.personal_packages", V1Key::Optional),
    ("features.team_dotfiles", V1Key::Optional),
    ("features.collab_secrets", V1Key::Optional),
    ("features.team_layering", V1Key::Optional),
    ("sync", V1Key::Required),
    ("sync.interval", V1Key::Required),
    ("sync.strategy", V1Key::Required),
    ("backend", V1Key::Required),
    ("backend.type", V1Key::Required),
    ("backend.url", V1Key::Required),
    ("packages", V1Key::Required),
    ("packages.remove_unlisted", V1Key::Optional),
    ("packages.brew.enabled", V1Key::Required),
    ("packages.brew.sync_casks", V1Key::Required),
    ("packages.brew.sync_taps", V1Key::Required),
    ("packages.npm.enabled", V1Key::Required),
    ("packages.npm.sync_versions", V1Key::Required),
    ("packages.pnpm.enabled", V1Key::Required),
    ("packages.pnpm.sync_versions", V1Key::Required),
    ("packages.bun.enabled", V1Key::Required),
    ("packages.bun.sync_versions", V1Key::Required),
    ("packages.gem.enabled", V1Key::Required),
    ("packages.gem.sync_versions", V1Key::Required),
    ("packages.uv.enabled", V1Key::Required),
    ("packages.uv.sync_versions", V1Key::Required),
    ("dotfiles", V1Key::Required),
    ("dotfiles.files", V1Key::Required),
    ("dotfiles.files.[].path", V1Key::Required),
    ("dotfiles.files.[].create_if_missing", V1Key::Optional),
    ("dotfiles.dirs", V1Key::Optional),
    ("security.encrypt_dotfiles", V1Key::Required),
    ("security.scan_secrets", V1Key::Required),
    ("merge.command", V1Key::Optional),
    ("merge.args", V1Key::Optional),
    ("team.enabled", V1Key::Required),
    ("team.url", V1Key::Required),
    ("team.auto_inject", V1Key::Required),
    ("team.read_only", V1Key::Required),
    ("team.orgs", V1Key::Optional),
    ("teams.active", V1Key::Optional),
    ("teams.teams", V1Key::Required),
    ("teams.teams.*.enabled", V1Key::Required),
    ("teams.teams.*.url", V1Key::Required),
    ("teams.teams.*.auto_inject", V1Key::Required),
    ("teams.teams.*.read_only", V1Key::Required),
    ("teams.teams.*.orgs", V1Key::Optional),
    ("teams.allowed_orgs", V1Key::Optional),
    ("teams.collabs.*.sync_url", V1Key::Required),
    ("teams.collabs.*.projects", V1Key::Optional),
    ("teams.collabs.*.members_cache", V1Key::Optional),
    ("teams.collabs.*.last_refresh", V1Key::Optional),
    ("teams.collabs.*.enabled", V1Key::Optional),
    ("project_configs.enabled", V1Key::Required),
    ("project_configs.search_paths", V1Key::Required),
    ("project_configs.patterns", V1Key::Required),
    ("project_configs.only_if_gitignored", V1Key::Required),
    ("machine_profiles.*", V1Key::Optional),
    ("profiles.*.dotfiles", V1Key::Optional),
    ("profiles.*.dotfiles.[].path", V1Key::Required),
    ("profiles.*.dotfiles.[].shared", V1Key::Optional),
    ("profiles.*.dotfiles.[].create_if_missing", V1Key::Optional),
    ("profiles.*.dirs", V1Key::Optional),
    ("profiles.*.packages", V1Key::Optional),
];

fn default_config_version() -> u32 {
    1
}

fn is_false(b: &bool) -> bool {
    !*b
}

/// config.toml syncs as bytes, so each save must write a map in the same order. Otherwise
/// machines see a change where there is none, and push it back and forth.
fn sorted<S: serde::Serializer, V: Serialize>(
    map: &HashMap<String, V>,
    serializer: S,
) -> std::result::Result<S::Ok, S::Error> {
    map.iter()
        .collect::<std::collections::BTreeMap<_, _>>()
        .serialize(serializer)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    /// Config format version - prevents older tether from corrupting newer configs
    #[serde(default = "default_config_version")]
    pub config_version: u32,
    /// CONFIG_WRITER when Tether 2.0 or later wrote the file; 0 when 1.x did
    #[serde(default)]
    pub config_writer: u32,
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
    /// Machine-to-profile assignments (machine_id -> profile_name)
    #[serde(
        default,
        skip_serializing_if = "HashMap::is_empty",
        serialize_with = "sorted"
    )]
    pub machine_profiles: HashMap<String, String>,
    /// Named profiles that restrict what a machine syncs
    #[serde(
        default,
        skip_serializing_if = "HashMap::is_empty",
        serialize_with = "sorted"
    )]
    pub profiles: HashMap<String, ProfileConfig>,
    #[serde(default, skip_serializing_if = "DashboardConfig::is_default")]
    pub dashboard: DashboardConfig,
}

/// Dashboard appearance
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct DashboardConfig {
    /// "auto" (default), "mocha", "latte" or "ansi"
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub theme: Option<String>,
}

impl DashboardConfig {
    fn is_default(&self) -> bool {
        *self == Self::default()
    }
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
    /// Skip package releases younger than this many days (0 disables)
    #[serde(default = "default_min_release_age_days")]
    pub min_release_age_days: u32,
    /// Packages whose install scripts may run; scripts are off for all others
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub allow_scripts: Vec<String>,
    /// Install packages from other machines without approval when a trusted machine signed
    /// the commit that added them and every other check passes
    #[serde(default = "default_true")]
    pub auto_install_from_trusted: bool,
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
    /// Third-party taps allowed to install; `homebrew/*` is always allowed
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub trusted_taps: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NpmConfig {
    pub enabled: bool,
    // 1.x builds require this field when they read a synced config; remove in 3.0
    #[serde(default)]
    pub sync_versions: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PnpmConfig {
    pub enabled: bool,
    // 1.x builds require this field when they read a synced config; remove in 3.0
    #[serde(default)]
    pub sync_versions: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BunConfig {
    pub enabled: bool,
    // 1.x builds require this field when they read a synced config; remove in 3.0
    #[serde(default)]
    pub sync_versions: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GemConfig {
    pub enabled: bool,
    // 1.x builds require this field when they read a synced config; remove in 3.0
    #[serde(default)]
    pub sync_versions: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UvConfig {
    pub enabled: bool,
    // 1.x builds require this field when they read a synced config; remove in 3.0
    #[serde(default)]
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

fn default_min_release_age_days() -> u32 {
    7
}

fn default_brew_config() -> BrewConfig {
    BrewConfig {
        enabled: true,
        sync_casks: true,
        sync_taps: true,
        trusted_taps: Vec::new(),
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
        #[serde(default, skip_serializing_if = "OnConflict::is_prompt")]
        on_conflict: OnConflict,
    },
}

fn default_create_if_missing() -> bool {
    true
}

/// How the daemon settles a file changed both locally and remotely.
/// `local`/`remote` suit app-managed files that rewrite themselves (timestamps, caches).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum OnConflict {
    #[default]
    Prompt,
    Local,
    Remote,
}

impl OnConflict {
    fn is_prompt(&self) -> bool {
        *self == OnConflict::Prompt
    }
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

    pub fn on_conflict(&self) -> OnConflict {
        match self {
            DotfileEntry::Simple(_) => OnConflict::Prompt,
            DotfileEntry::WithOptions { on_conflict, .. } => *on_conflict,
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

    // Tether's own directory holds keys, the merge base and state; it never syncs as a dotfile
    !in_tether_dir(path_to_check)
}

/// Whether a home-relative path names Tether's own directory or a path in it. `.` components
/// do not count. The name compares exactly: `TetherDir` decides by file identity, which also
/// covers `~/.Tether` on a volume that ignores case, and symlinks and aliases.
pub fn in_tether_dir(path: &str) -> bool {
    let path = path.strip_prefix("~/").unwrap_or(path);
    path.split('/')
        .find(|c| !c.is_empty() && *c != ".")
        .is_some_and(|first| first == ".tether")
}

/// Tether's own directory, to test the paths that a sync reads or writes. It holds keys, the
/// merge base and state, so nothing in it syncs, also through a symlink or an alias such as
/// /var for /private/var. A path is in it when the path or an ancestor is the same directory
/// (device and inode), so whether `.Tether` is `.tether` follows the volume, not the OS.
pub struct TetherDir {
    path: PathBuf,
    id: Option<(u64, u64)>,
}

impl TetherDir {
    pub fn new(home: &std::path::Path) -> Self {
        let path = resolve(&home.join(".tether"));
        let id = file_id(&path);
        Self { path, id }
    }

    /// Whether `path` resolves to the directory or a path in it
    pub fn contains(&self, path: &std::path::Path) -> bool {
        let path = resolve(path);
        match self.id {
            Some(id) => path.ancestors().any(|a| file_id(a) == Some(id)),
            // Nothing exists in a directory that does not exist yet
            None => path.starts_with(&self.path),
        }
    }
}

#[cfg(unix)]
fn file_id(path: &std::path::Path) -> Option<(u64, u64)> {
    use std::os::unix::fs::MetadataExt;
    std::fs::metadata(path).ok().map(|m| (m.dev(), m.ino()))
}

#[cfg(not(unix))]
fn file_id(_path: &std::path::Path) -> Option<(u64, u64)> {
    None
}

/// The canonical path; for a path that does not exist yet, the canonical path of the nearest
/// ancestor that exists, with the rest appended.
fn resolve(path: &std::path::Path) -> PathBuf {
    let mut rest = Vec::new();
    let mut current = path;
    loop {
        if let Ok(canonical) = std::fs::canonicalize(current) {
            return rest.iter().rev().fold(canonical, |p, name| p.join(name));
        }
        match (current.parent(), current.file_name()) {
            (Some(parent), Some(name)) => {
                rest.push(name.to_owned());
                current = parent;
            }
            _ => return path.to_path_buf(),
        }
    }
}

/// A dotfile entry within a profile — extends DotfileEntry with `shared` flag.
/// Shared dotfiles are stored in `profiles/shared/` and auto-propagate across profiles.
/// Profile-specific dotfiles are stored in `profiles/<profile>/` with independent copies.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ProfileDotfileEntry {
    /// Simple string path (defaults: shared=false, create_if_missing=false)
    Simple(String),
    /// Object with explicit options
    WithOptions {
        path: String,
        #[serde(default)]
        shared: bool,
        #[serde(default)]
        create_if_missing: bool,
        #[serde(default, skip_serializing_if = "OnConflict::is_prompt")]
        on_conflict: OnConflict,
    },
}

impl ProfileDotfileEntry {
    pub fn path(&self) -> &str {
        match self {
            ProfileDotfileEntry::Simple(p) => p,
            ProfileDotfileEntry::WithOptions { path, .. } => path,
        }
    }

    pub fn shared(&self) -> bool {
        match self {
            ProfileDotfileEntry::Simple(_) => false,
            ProfileDotfileEntry::WithOptions { shared, .. } => *shared,
        }
    }

    pub fn create_if_missing(&self) -> bool {
        match self {
            ProfileDotfileEntry::Simple(_) => false,
            ProfileDotfileEntry::WithOptions {
                create_if_missing, ..
            } => *create_if_missing,
        }
    }

    pub fn on_conflict(&self) -> OnConflict {
        match self {
            ProfileDotfileEntry::Simple(_) => OnConflict::Prompt,
            ProfileDotfileEntry::WithOptions { on_conflict, .. } => *on_conflict,
        }
    }

    /// Convert to DotfileEntry (dropping shared flag)
    pub fn to_dotfile_entry(&self) -> DotfileEntry {
        match self {
            ProfileDotfileEntry::Simple(p) => DotfileEntry::WithOptions {
                path: p.clone(),
                create_if_missing: false,
                on_conflict: OnConflict::Prompt,
            },
            ProfileDotfileEntry::WithOptions {
                path,
                create_if_missing,
                on_conflict,
                ..
            } => DotfileEntry::WithOptions {
                path: path.clone(),
                create_if_missing: *create_if_missing,
                on_conflict: *on_conflict,
            },
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
        three_way_args()
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

/// Settings for this machine only, in `~/.tether/local.toml`. Tether never syncs this file,
/// so it overrides the synced config.toml on this machine alone.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LocalConfig {
    #[serde(default)]
    pub packages: LocalPackagesConfig,
    #[serde(default)]
    pub merge: LocalMergeConfig,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LocalPackagesConfig {
    pub min_release_age_days: Option<u32>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LocalMergeConfig {
    pub command: Option<String>,
    /// Defaults to `{local} {remote} {merged}` when only the command is set
    pub args: Option<Vec<String>>,
}

impl LocalConfig {
    pub fn path() -> Result<PathBuf> {
        Ok(Config::config_dir()?.join("local.toml"))
    }

    /// A missing file means no overrides.
    pub fn load() -> Result<Self> {
        match std::fs::read_to_string(Self::path()?) {
            Ok(content) => Ok(toml::from_str(&content)?),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(e.into()),
        }
    }
}

fn three_way_args() -> Vec<String> {
    vec![
        "{local}".to_string(),
        "{remote}".to_string(),
        "{merged}".to_string(),
    ]
}

/// The merge tool for this machine: local.toml overrides the synced tool. The synced
/// default is opendiff, which only macOS has, so a machine without it uses vimdiff.
fn effective_merge(
    synced: &MergeConfig,
    local: &LocalMergeConfig,
    installed: impl Fn(&str) -> bool,
) -> MergeConfig {
    let mut merge = match &local.command {
        Some(command) => MergeConfig {
            command: command.clone(),
            args: local.args.clone().unwrap_or_else(three_way_args),
        },
        None => synced.clone(),
    };
    if merge.command == "opendiff" && !installed("opendiff") {
        merge = MergeConfig {
            command: "vimdiff".to_string(),
            args: three_way_args(),
        };
    }
    merge
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
    #[serde(serialize_with = "sorted")]
    pub teams: HashMap<String, TeamConfig>,
    /// Allowed GitHub organizations for team repos (empty = no restriction)
    #[serde(default)]
    pub allowed_orgs: Vec<String>,
    /// Collaborator-based project secret sharing (keyed by collab name)
    #[serde(default, serialize_with = "sorted")]
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
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum StringOrVec {
        Single(String),
        Multiple(Vec<String>),
    }

    Ok(match Option::<StringOrVec>::deserialize(deserializer)? {
        Some(StringOrVec::Single(s)) => vec![s],
        Some(StringOrVec::Multiple(v)) => v,
        None => Vec::new(),
    })
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

/// A named profile controlling what a machine syncs.
/// Profiles are the source of truth in config v2.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ProfileConfig {
    /// Dotfiles to sync (with optional shared/create_if_missing flags)
    #[serde(default)]
    pub dotfiles: Vec<ProfileDotfileEntry>,
    /// Directories to sync
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub dirs: Vec<String>,
    /// Enabled package managers (e.g., ["brew", "npm", "pnpm"])
    /// Empty = all globally-enabled managers
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub packages: Vec<String>,
}

impl Config {
    pub fn config_dir() -> Result<PathBuf> {
        let home = crate::home_dir()?;
        Ok(home.join(".tether"))
    }

    pub fn config_path() -> Result<PathBuf> {
        Ok(Self::config_dir()?.join("config.toml"))
    }

    /// The merge tool this machine launches.
    pub fn merge_tool(&self) -> Result<MergeConfig> {
        let local = LocalConfig::load()?;
        Ok(effective_merge(&self.merge, &local.merge, |tool| {
            which::which(tool).is_ok()
        }))
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

    /// Get the profile name for a machine. Defaults to "dev" if unassigned.
    pub fn profile_name(&self, machine_id: &str) -> &str {
        self.machine_profiles
            .get(machine_id)
            .map(|s| s.as_str())
            .unwrap_or(DEFAULT_PROFILE)
    }

    /// Get the profile assigned to a machine, if any
    pub fn machine_profile(&self, machine_id: &str) -> Option<&ProfileConfig> {
        self.profiles.get(self.profile_name(machine_id))
    }

    /// Get effective dotfiles for a machine as DotfileEntry vec.
    /// Profile dotfiles take priority; falls back to global dotfiles.files.
    pub fn effective_dotfiles(&self, machine_id: &str) -> Vec<DotfileEntry> {
        if let Some(profile) = self.machine_profile(machine_id) {
            if !profile.dotfiles.is_empty() {
                let mut entries: Vec<DotfileEntry> = profile
                    .dotfiles
                    .iter()
                    .map(|e| e.to_dotfile_entry())
                    .collect();
                for global in &self.dotfiles.files {
                    if !entries.iter().any(|e| e.path() == global.path()) {
                        entries.push(global.clone());
                    }
                }
                return entries;
            }
        }
        self.dotfiles.files.clone()
    }

    /// Get profile dotfile entries (with shared flag) for a machine.
    pub fn profile_dotfiles(&self, machine_id: &str) -> Option<&[ProfileDotfileEntry]> {
        let profile = self.machine_profile(machine_id)?;
        if profile.dotfiles.is_empty() {
            None
        } else {
            Some(&profile.dotfiles)
        }
    }

    /// Get effective dirs for a machine. Profile dirs merge with global dirs;
    /// profile entries take priority on duplicates.
    pub fn effective_dirs(&self, machine_id: &str) -> Vec<String> {
        if let Some(profile) = self.machine_profile(machine_id) {
            if !profile.dirs.is_empty() {
                let mut dirs = profile.dirs.clone();
                for global in &self.dotfiles.dirs {
                    if !dirs.contains(global) {
                        dirs.push(global.clone());
                    }
                }
                return dirs;
            }
        }
        self.dotfiles.dirs.clone()
    }

    /// Check if a package manager is enabled for a machine.
    /// Global config must enable it AND profile must include it (if profile has packages list).
    pub fn is_manager_enabled(&self, machine_id: &str, manager: &str) -> bool {
        let global_enabled = match manager {
            "brew" | "brew_formulae" | "brew_casks" | "brew_taps" => self.packages.brew.enabled,
            "npm" => self.packages.npm.enabled,
            "pnpm" => self.packages.pnpm.enabled,
            "bun" => self.packages.bun.enabled,
            "gem" => self.packages.gem.enabled,
            "uv" => self.packages.uv.enabled,
            _ => true,
        };
        if !global_enabled {
            return false;
        }

        // Normalize brew sub-types to "brew"
        let base = match manager {
            "brew_formulae" | "brew_casks" | "brew_taps" => "brew",
            other => other,
        };

        if let Some(profile) = self.machine_profile(machine_id) {
            if !profile.packages.is_empty() {
                return profile.packages.iter().any(|m| m == base);
            }
        }

        true
    }

    /// Check if a dotfile is shared in the given machine's profile.
    pub fn is_dotfile_shared(&self, machine_id: &str, dotfile_path: &str) -> bool {
        if let Some(entries) = self.profile_dotfiles(machine_id) {
            for entry in entries {
                if entry.path() == dotfile_path {
                    return entry.shared();
                }
            }
        }
        false
    }

    /// Validate a profile name is safe for filesystem use.
    /// Rejects empty, path-traversal, dot-prefixed, and reserved names.
    pub fn is_safe_profile_name(name: &str) -> bool {
        if name.is_empty()
            || name.contains('/')
            || name.contains('\\')
            || name.contains("..")
            || name.starts_with('.')
        {
            return false;
        }
        // Reserved names that conflict with repo structure
        let reserved = [
            "shared",
            "tether",
            "dotfiles",
            "manifests",
            "machines",
            "configs",
            "projects",
            "profiles",
        ];
        if reserved.contains(&name) {
            return false;
        }
        true
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
        let content = std::fs::read_to_string(&path)?;
        let (config, migrated) = Self::parse_migrated(&content)?;
        if migrated {
            // Best-effort save (don't fail load if save fails)
            let _ = config.save();
        }
        Ok(config)
    }

    /// Parses config.toml text as `load` reads it, without saving a migration.
    pub fn parse(content: &str) -> Result<Self> {
        Ok(Self::parse_migrated(content)?.0)
    }

    fn parse_migrated(content: &str) -> Result<(Self, bool)> {
        let mut config: Self = toml::from_str(content)?;

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

        // v1 → v2 migration: create "dev" profile from global dotfiles/dirs/packages
        let migrated = config.config_version < 2 && config.profiles.is_empty();
        if migrated {
            config.migrate_v1_to_v2();
            config.config_version = CURRENT_CONFIG_VERSION;
        }

        Ok((config, migrated))
    }

    /// Migrate v1 config to v2: create "dev" profile from global settings.
    pub fn migrate_v1_to_v2(&mut self) {
        // Build package manager list from global config
        let mut packages = Vec::new();
        if self.packages.brew.enabled {
            packages.push("brew".to_string());
        }
        if self.packages.npm.enabled {
            packages.push("npm".to_string());
        }
        if self.packages.pnpm.enabled {
            packages.push("pnpm".to_string());
        }
        if self.packages.bun.enabled {
            packages.push("bun".to_string());
        }
        if self.packages.gem.enabled {
            packages.push("gem".to_string());
        }
        if self.packages.uv.enabled {
            packages.push("uv".to_string());
        }

        // Convert global dotfiles to ProfileDotfileEntry (preserving create_if_missing)
        let dotfiles: Vec<ProfileDotfileEntry> = self
            .dotfiles
            .files
            .iter()
            .map(|entry| ProfileDotfileEntry::WithOptions {
                path: entry.path().to_string(),
                shared: false,
                create_if_missing: entry.create_if_missing(),
                on_conflict: entry.on_conflict(),
            })
            .collect();

        let dev_profile = ProfileConfig {
            dotfiles,
            dirs: self.dotfiles.dirs.clone(),
            packages,
        };

        self.profiles
            .insert(DEFAULT_PROFILE.to_string(), dev_profile);

        // Assign all unassigned machines to default profile
        // (machines already in machine_profiles keep their existing assignment)
    }

    /// Writes only the settings that changed into the existing file, so comments, layout and
    /// keys from a newer Tether stay.
    pub fn save(&self) -> Result<()> {
        let mut config = self.clone();
        config.config_version = CURRENT_CONFIG_VERSION;
        config.config_writer = CONFIG_WRITER;

        let path = Self::config_path()?;
        let current = match std::fs::read_to_string(&path) {
            Ok(text) => Some(text),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
            Err(e) => return Err(e.into()),
        };
        let content = crate::sync::config_merge::save_text(current.as_deref(), &config)?;
        crate::sync::atomic_write_private(&path, content.as_bytes())
    }
}

impl Default for Config {
    fn default() -> Self {
        Self {
            config_version: CURRENT_CONFIG_VERSION,
            config_writer: CONFIG_WRITER,
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
                min_release_age_days: default_min_release_age_days(),
                allow_scripts: Vec::new(),
                auto_install_from_trusted: true,
                brew: default_brew_config(),
                npm: default_npm_config(),
                pnpm: default_pnpm_config(),
                bun: default_bun_config(),
                gem: default_gem_config(),
                uv: UvConfig::default(),
            },
            dotfiles: DotfilesConfig {
                files: vec![
                    // Shell configs - don't create on machines that don't have them
                    DotfileEntry::WithOptions {
                        path: ".zshrc".to_string(),
                        create_if_missing: false,
                        on_conflict: OnConflict::Prompt,
                    },
                    DotfileEntry::WithOptions {
                        path: ".zprofile".to_string(),
                        create_if_missing: false,
                        on_conflict: OnConflict::Prompt,
                    },
                    DotfileEntry::WithOptions {
                        path: ".zshenv".to_string(),
                        create_if_missing: false,
                        on_conflict: OnConflict::Prompt,
                    },
                    DotfileEntry::WithOptions {
                        path: ".bashrc".to_string(),
                        create_if_missing: false,
                        on_conflict: OnConflict::Prompt,
                    },
                    DotfileEntry::WithOptions {
                        path: ".bash_profile".to_string(),
                        create_if_missing: false,
                        on_conflict: OnConflict::Prompt,
                    },
                    DotfileEntry::WithOptions {
                        path: ".profile".to_string(),
                        create_if_missing: false,
                        on_conflict: OnConflict::Prompt,
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
            machine_profiles: HashMap::new(),
            profiles: HashMap::new(),
            dashboard: DashboardConfig::default(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn saved_config_bytes_do_not_depend_on_map_order() {
        let ids = ["m1", "a7", "zz", "b2", "q9", "c3"];
        let build = |order: &mut dyn Iterator<Item = &&str>| {
            let mut config = Config::default();
            for id in order {
                config
                    .machine_profiles
                    .insert(id.to_string(), "dev".to_string());
                config.profiles.insert(
                    id.to_string(),
                    ProfileConfig {
                        dotfiles: vec![ProfileDotfileEntry::Simple(format!(".{}rc", id))],
                        dirs: vec![format!(".config/{}", id)],
                        packages: vec!["brew".to_string(), "npm".to_string()],
                    },
                );
                let teams = config.teams.get_or_insert_with(Default::default);
                teams.teams.insert(
                    id.to_string(),
                    TeamConfig {
                        enabled: true,
                        url: format!("git@github.com:{}/dotfiles.git", id),
                        auto_inject: false,
                        read_only: true,
                        orgs: vec![format!("github.com/{}", id)],
                    },
                );
                teams.collabs.insert(
                    id.to_string(),
                    CollabConfig {
                        sync_url: format!("git@github.com:{}/collab.git", id),
                        projects: vec![format!("github.com/{}/app", id)],
                        members_cache: vec![id.to_string()],
                        last_refresh: None,
                        enabled: true,
                    },
                );
            }
            toml::to_string_pretty(&config).unwrap()
        };
        let written = build(&mut ids.iter());
        assert_eq!(build(&mut ids.iter().rev()), written);
        let reread: Config = toml::from_str(&written).unwrap();
        assert_eq!(toml::to_string_pretty(&reread).unwrap(), written);
        let listed: Vec<&str> = written
            .lines()
            .skip_while(|l| *l != "[machine_profiles]")
            .skip(1)
            .take_while(|l| !l.is_empty())
            .map(|l| l.split(' ').next().unwrap())
            .collect();
        assert_eq!(listed, ["a7", "b2", "c3", "m1", "q9", "zz"]);
    }

    #[test]
    fn test_saved_config_parses_with_1x_shape() {
        // The fields 1.11.10, 1.12.0 and 1.13.1 require, with their types and names. All
        // three have the same required fields, and none denies unknown fields, so 2.0's
        // new fields must stay optional additions.
        #[derive(Deserialize)]
        #[allow(dead_code)]
        struct OldManager {
            enabled: bool,
            sync_versions: bool,
        }
        #[derive(Deserialize)]
        #[allow(dead_code)]
        struct OldBrew {
            enabled: bool,
            sync_casks: bool,
            sync_taps: bool,
        }
        #[derive(Deserialize)]
        #[allow(dead_code)]
        struct OldPackages {
            remove_unlisted: bool,
            brew: OldBrew,
            npm: OldManager,
            pnpm: OldManager,
            bun: OldManager,
            gem: OldManager,
            uv: OldManager,
        }
        #[derive(Deserialize)]
        #[allow(dead_code)]
        enum OldStrategy {
            #[serde(rename = "last-write-wins")]
            LastWriteWins,
            #[serde(rename = "manual")]
            Manual,
            #[serde(rename = "machine-priority")]
            MachinePriority,
        }
        #[derive(Deserialize)]
        #[allow(dead_code)]
        struct OldSync {
            interval: String,
            strategy: OldStrategy,
        }
        #[derive(Deserialize)]
        #[allow(dead_code)]
        enum OldBackendType {
            #[serde(rename = "git")]
            Git,
        }
        #[derive(Deserialize)]
        #[allow(dead_code)]
        struct OldBackend {
            #[serde(rename = "type")]
            backend_type: OldBackendType,
            url: String,
        }
        #[derive(Deserialize)]
        #[allow(dead_code)]
        struct OldDotfiles {
            files: Vec<toml::Value>,
        }
        #[derive(Deserialize)]
        #[allow(dead_code)]
        struct OldSecurity {
            encrypt_dotfiles: bool,
            scan_secrets: bool,
        }
        #[derive(Deserialize)]
        #[allow(dead_code)]
        struct OldTeam {
            enabled: bool,
            url: String,
            auto_inject: bool,
            read_only: bool,
        }
        #[derive(Deserialize)]
        #[allow(dead_code)]
        struct OldCollab {
            sync_url: String,
        }
        #[derive(Deserialize)]
        #[allow(dead_code)]
        struct OldTeams {
            teams: HashMap<String, OldTeam>,
            #[serde(default)]
            collabs: HashMap<String, OldCollab>,
        }
        #[derive(Deserialize)]
        #[allow(dead_code)]
        struct OldProjectConfigs {
            enabled: bool,
            search_paths: Vec<String>,
            patterns: Vec<String>,
            only_if_gitignored: bool,
        }
        #[derive(Deserialize)]
        #[allow(dead_code)]
        struct OldConfig {
            config_version: u32,
            sync: OldSync,
            backend: OldBackend,
            packages: OldPackages,
            dotfiles: OldDotfiles,
            security: OldSecurity,
            team: Option<OldTeam>,
            teams: Option<OldTeams>,
            project_configs: Option<OldProjectConfigs>,
        }

        let mut config = Config::default();
        config.dashboard.theme = Some("mocha".to_string());
        config.packages.min_release_age_days = 3;
        config.packages.allow_scripts = vec!["esbuild".to_string()];
        config.packages.auto_install_from_trusted = false;
        config.packages.brew.trusted_taps = vec!["azure/kubelogin".to_string()];
        let written = toml::to_string_pretty(&config).unwrap();
        let old = toml::from_str::<OldConfig>(&written).unwrap();
        // 1.x refuses a config_version above its own
        assert!(old.config_version <= 2);

        let saved = toml::to_string_pretty(&Config::default()).unwrap();

        let without_field = saved.replace("sync_versions = false\n", "");
        let config: Config = toml::from_str(&without_field).unwrap();
        assert!(!config.packages.npm.sync_versions);

        // The synced copy of a local file that leaves out fields 1.x requires has them again,
        // also with teams, collabs and project configs, and as written by a merge or save
        let mut config = Config::default();
        let team = TeamConfig {
            enabled: true,
            url: "git@example.com:acme/dotfiles.git".to_string(),
            auto_inject: false,
            read_only: true,
            orgs: vec!["github.com/acme".to_string()],
        };
        config.team = Some(team.clone());
        let mut teams = TeamsConfig::default();
        teams.teams.insert("acme".to_string(), team);
        teams.collabs.insert(
            "c".to_string(),
            CollabConfig {
                sync_url: "git@example.com:acme/collab.git".to_string(),
                projects: vec![],
                members_cache: vec![],
                last_refresh: Some(chrono::Utc::now()),
                enabled: true,
            },
        );
        config.teams = Some(teams);
        config.project_configs.enabled = true;
        config.team_only = true;
        config.dotfiles.files.push(DotfileEntry::WithOptions {
            path: ".vimrc".to_string(),
            create_if_missing: true,
            on_conflict: OnConflict::Local,
        });
        config.profiles.insert(
            "dev".to_string(),
            ProfileConfig {
                dotfiles: vec![ProfileDotfileEntry::WithOptions {
                    path: ".zshrc".to_string(),
                    shared: true,
                    create_if_missing: true,
                    on_conflict: OnConflict::Local,
                }],
                dirs: vec![".config/a".to_string()],
                packages: vec!["npm".to_string()],
            },
        );
        config
            .machine_profiles
            .insert("m".to_string(), "dev".to_string());
        let local = toml::to_string_pretty(&config)
            .unwrap()
            .replace("sync_versions = false\n", "");
        assert!(toml::from_str::<OldConfig>(&local).is_err());
        let exported = crate::sync::config_merge::export_text(&local).unwrap();
        let old = toml::from_str::<OldConfig>(&exported).unwrap();
        assert!(old.config_version <= 2);
        assert!(old.teams.is_some() && old.team.is_some() && old.project_configs.is_some());

        /// The values at a V1_KEYS path
        fn at<'a>(v: &'a toml::Value, path: &[&str]) -> Vec<&'a toml::Value> {
            let Some((key, rest)) = path.split_first() else {
                return vec![v];
            };
            let children: Vec<&toml::Value> = match (*key, v) {
                ("*", toml::Value::Table(t)) => t.values().collect(),
                ("[]", toml::Value::Array(a)) => a.iter().collect(),
                (key, toml::Value::Table(t)) => t.get(key).into_iter().collect(),
                _ => Vec::new(),
            };
            children.into_iter().flat_map(|c| at(c, rest)).collect()
        }
        let full: toml::Value = toml::from_str(&toml::to_string_pretty(&config).unwrap()).unwrap();
        let exported: toml::Value = toml::from_str(&exported).unwrap();
        for (key, kind) in V1_KEYS {
            let path: Vec<&str> = key.split('.').collect();
            // Every key 1.x knows is a key 2.0 writes
            assert!(!at(&full, &path).is_empty(), "{key} is not a 2.0 key");
            // An export has every key 1.x requires, in each table that holds it
            if *kind == V1Key::Required {
                let (last, parent) = path.split_last().unwrap();
                for table in at(&exported, parent).iter().filter_map(|v| v.as_table()) {
                    assert!(table.contains_key(*last), "the export lacks {key}");
                }
            }
        }
    }

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
    fn tether_dir_never_syncs() {
        assert!(!is_safe_dotfile_path(".tether"));
        assert!(!is_safe_dotfile_path("~/.tether/config.base.toml"));
        assert!(!is_safe_dotfile_path(".tether/*"));
        assert!(is_safe_dotfile_path(".tetherrc"));
        assert!(!is_safe_dotfile_path("~/./.tether/*"));
        assert!(!is_safe_dotfile_path(".//.tether/x"));
        assert!(!is_safe_dotfile_path("./././.tether"));
        assert!(is_safe_dotfile_path(".config/.tether"));
        // A distinct ~/.Tether exists on a volume that keeps case; TetherDir decides by identity
        assert!(is_safe_dotfile_path("~/.Tether/*"));
    }

    #[cfg(unix)]
    #[test]
    fn tether_dir_contains_resolved_paths() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("home");
        std::fs::create_dir_all(home.join(".tether")).unwrap();
        std::fs::create_dir_all(home.join(".config")).unwrap();
        std::os::unix::fs::symlink(home.join(".tether"), home.join(".config/link")).unwrap();
        // The home through another name, as /var is /private/var on macOS
        let alias = temp.path().join("alias");
        std::os::unix::fs::symlink(&home, &alias).unwrap();

        let tether = TetherDir::new(&home);
        assert!(tether.contains(&home.join(".tether")));
        assert!(tether.contains(&home.join(".tether/config.toml")));
        assert!(tether.contains(&home.join("./.tether/new/file")));
        assert!(tether.contains(&home.join(".config/link/config.toml")));
        assert!(tether.contains(&alias.join(".tether/state.json")));
        assert!(TetherDir::new(&alias).contains(&home.join(".tether/x")));
        assert!(!tether.contains(&home.join(".config/nvim/init.lua")));
        assert!(!tether.contains(&home.join(".tetherrc")));
        assert!(!tether.contains(&home));
        // On a volume that ignores case, .TETHER is the same directory
        assert_eq!(
            tether.contains(&home.join(".TETHER/x")),
            home.join(".TETHER").exists()
        );
        if cfg!(target_os = "macos") {
            let canonical = std::fs::canonicalize(&home).unwrap();
            assert_ne!(canonical, home, "a macOS temp dir is under /private/var");
            assert!(tether.contains(&canonical.join(".tether/x")));
        }

        for pattern in [".tether/*", "./.tether/config.toml", ".config/link/*"] {
            std::fs::write(home.join(".tether/config.toml"), "x").unwrap();
            assert!(
                crate::sync::expand_dotfile_glob(pattern, &home).is_empty(),
                "{pattern}"
            );
        }
    }

    /// ~/.Tether is Tether's directory only when the volume ignores case
    #[cfg(unix)]
    #[test]
    fn tether_dir_case_follows_the_volume() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path();
        std::fs::create_dir_all(home.join(".tether")).unwrap();
        std::fs::write(home.join(".tether/signing_key"), "secret").unwrap();
        let ignores_case = home.join(".Tether").exists();
        if !ignores_case {
            std::fs::create_dir_all(home.join(".Tether")).unwrap();
            std::fs::write(home.join(".Tether/notes"), "mine").unwrap();
        }
        let tether = TetherDir::new(home);
        assert_eq!(tether.contains(&home.join(".Tether/x")), ignores_case);
        let expanded = crate::sync::expand_dotfile_glob(".Tether/*", home);
        if ignores_case {
            assert!(expanded.is_empty(), "{expanded:?}");
        } else {
            assert_eq!(expanded, vec![".Tether/notes"]);
        }
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
            on_conflict: OnConflict::Prompt,
        };
        assert_eq!(entry.path(), ".bashrc");
        assert!(!entry.create_if_missing());
    }

    #[test]
    fn test_profile_dotfile_on_conflict_parses_and_omits_default() {
        #[derive(Serialize, Deserialize)]
        struct Wrapper {
            dotfiles: Vec<ProfileDotfileEntry>,
        }
        let parsed: Wrapper = toml::from_str(
            r#"
[[dotfiles]]
path = ".claude/plugins/known_marketplaces.json"
on_conflict = "local"

[[dotfiles]]
path = ".zshrc"
shared = true
"#,
        )
        .unwrap();
        assert_eq!(parsed.dotfiles[0].on_conflict(), OnConflict::Local);
        assert_eq!(
            parsed.dotfiles[0].to_dotfile_entry().on_conflict(),
            OnConflict::Local
        );
        assert_eq!(parsed.dotfiles[1].on_conflict(), OnConflict::Prompt);

        let out = toml::to_string(&parsed).unwrap();
        assert_eq!(out.matches("on_conflict").count(), 1);
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
        // Configs from before the supply-chain settings get the secure defaults
        assert_eq!(parsed.packages.min_release_age_days, 7);
        assert!(parsed.packages.allow_scripts.is_empty());
        assert!(parsed.packages.brew.trusted_taps.is_empty());
        assert!(parsed.packages.auto_install_from_trusted);
    }

    #[test]
    fn test_supply_chain_settings_parse() {
        let config = r#"
[sync]
interval = "5m"
strategy = "last-write-wins"

[backend]
type = "git"
url = ""

[packages]
min_release_age_days = 0
allow_scripts = ["esbuild"]
auto_install_from_trusted = false

[packages.brew]
enabled = true
sync_casks = true
sync_taps = true
trusted_taps = ["oven-sh/bun"]

[dotfiles]
files = []
"#;
        let parsed: Config = toml::from_str(config).unwrap();
        assert_eq!(parsed.packages.min_release_age_days, 0);
        assert_eq!(parsed.packages.allow_scripts, vec!["esbuild"]);
        assert!(!parsed.packages.auto_install_from_trusted);
        assert_eq!(parsed.packages.brew.trusted_taps, vec!["oven-sh/bun"]);
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
    fn test_effective_dotfiles_with_profile() {
        let mut config = Config::default();
        config.profiles.insert(
            "server".to_string(),
            ProfileConfig {
                dotfiles: vec![ProfileDotfileEntry::Simple(".zshrc".to_string())],
                dirs: vec![],
                packages: vec![],
            },
        );
        config
            .machine_profiles
            .insert("my-server".to_string(), "server".to_string());

        let files = config.effective_dotfiles("my-server");
        // Profile dotfiles are merged with global — .zshrc is in both so no duplicate
        assert_eq!(files.len(), config.dotfiles.files.len());
        // Profile entry comes first
        assert_eq!(files[0].path(), ".zshrc");

        // Unassigned machines get "dev" profile (which may or may not exist)
        // If "dev" doesn't exist, falls through to global
        let other = config.effective_dotfiles("my-laptop");
        assert_eq!(other.len(), config.dotfiles.files.len());
    }

    #[test]
    fn test_effective_dotfiles_profile_overrides_global() {
        let mut config = Config::default();
        // Global has .zshrc with create_if_missing=false (from default)
        // Profile has .zshrc with create_if_missing=true — profile should win
        config.profiles.insert(
            "server".to_string(),
            ProfileConfig {
                dotfiles: vec![ProfileDotfileEntry::WithOptions {
                    path: ".zshrc".to_string(),
                    shared: false,
                    create_if_missing: true,
                    on_conflict: Default::default(),
                }],
                dirs: vec![],
                packages: vec![],
            },
        );
        config
            .machine_profiles
            .insert("my-server".to_string(), "server".to_string());

        let files = config.effective_dotfiles("my-server");
        let zshrc = files.iter().find(|e| e.path() == ".zshrc").unwrap();
        // Profile version takes priority (create_if_missing=true)
        assert!(zshrc.create_if_missing());
    }

    #[test]
    fn test_effective_dotfiles_disjoint_sets() {
        let mut config = Config::default();
        config.dotfiles.files = vec![DotfileEntry::Simple(".gitconfig".to_string())];
        config.profiles.insert(
            "server".to_string(),
            ProfileConfig {
                dotfiles: vec![ProfileDotfileEntry::Simple(".vimrc".to_string())],
                dirs: vec![],
                packages: vec![],
            },
        );
        config
            .machine_profiles
            .insert("my-server".to_string(), "server".to_string());

        let files = config.effective_dotfiles("my-server");
        // Profile has .vimrc, global has .gitconfig — should get both
        assert_eq!(files.len(), 2);
        assert_eq!(files[0].path(), ".vimrc"); // profile first
        assert_eq!(files[1].path(), ".gitconfig"); // global appended
    }

    #[test]
    fn test_is_manager_enabled_with_profile() {
        let mut config = Config::default();
        config.profiles.insert(
            "server".to_string(),
            ProfileConfig {
                dotfiles: vec![],
                dirs: vec![],
                packages: vec!["brew".to_string()],
            },
        );
        config
            .machine_profiles
            .insert("my-server".to_string(), "server".to_string());

        assert!(config.is_manager_enabled("my-server", "brew"));
        assert!(!config.is_manager_enabled("my-server", "npm"));
        // Unassigned machine defaults to "dev"; no dev profile = all global enabled
        assert!(config.is_manager_enabled("my-laptop", "npm"));

        // Profile with empty packages list = all globally-enabled managers
        config.profiles.insert(
            "minimal".to_string(),
            ProfileConfig {
                dotfiles: vec![],
                dirs: vec![],
                packages: vec![],
            },
        );
        config
            .machine_profiles
            .insert("other-box".to_string(), "minimal".to_string());
        assert!(config.is_manager_enabled("other-box", "brew"));
    }

    #[test]
    fn test_is_dotfile_shared() {
        let mut config = Config::default();
        config.profiles.insert(
            "dev".to_string(),
            ProfileConfig {
                dotfiles: vec![
                    ProfileDotfileEntry::Simple(".zshrc".to_string()),
                    ProfileDotfileEntry::WithOptions {
                        path: ".gitconfig".to_string(),
                        shared: true,
                        create_if_missing: false,
                        on_conflict: Default::default(),
                    },
                ],
                dirs: vec![],
                packages: vec![],
            },
        );
        config
            .machine_profiles
            .insert("my-mac".to_string(), "dev".to_string());

        assert!(!config.is_dotfile_shared("my-mac", ".zshrc"));
        assert!(config.is_dotfile_shared("my-mac", ".gitconfig"));
        assert!(!config.is_dotfile_shared("my-mac", ".bashrc"));
    }

    #[test]
    fn test_config_with_profiles_roundtrip() {
        let toml_str = r#"
config_version = 2

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

[machine_profiles]
my-server = "server"

[profiles.server]
dotfiles = [".zshrc"]
packages = ["brew"]
"#;
        let parsed: Config = toml::from_str(toml_str).unwrap();
        assert_eq!(
            parsed.machine_profiles.get("my-server"),
            Some(&"server".to_string())
        );
        let profile = parsed.profiles.get("server").unwrap();
        assert_eq!(profile.dotfiles.len(), 1);
        assert_eq!(profile.packages, vec!["brew"]);

        // Helpers work — profile dotfiles merge with global (profile has .zshrc, global adds .gitconfig)
        assert_eq!(parsed.effective_dotfiles("my-server").len(), 2);
        assert!(!parsed.is_manager_enabled("my-server", "npm"));
        assert!(parsed.is_manager_enabled("my-server", "brew"));
    }

    #[test]
    fn test_backwards_compat_no_profiles() {
        // Config without profiles should parse fine
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
        assert!(parsed.machine_profiles.is_empty());
        assert!(parsed.profiles.is_empty());
        // effective_dotfiles falls through to global (no "dev" profile)
        assert_eq!(parsed.effective_dotfiles("any").len(), 1);
    }

    #[test]
    fn test_profile_name_defaults_to_dev() {
        let config = Config::default();
        assert_eq!(config.profile_name("any-machine"), DEFAULT_PROFILE);
    }

    #[test]
    fn test_profile_name_respects_assignment() {
        let mut config = Config::default();
        config
            .machine_profiles
            .insert("my-server".to_string(), "server".to_string());
        assert_eq!(config.profile_name("my-server"), "server");
        assert_eq!(config.profile_name("other"), DEFAULT_PROFILE);
    }

    #[test]
    fn test_v1_to_v2_migration() {
        let mut config = Config {
            config_version: 1,
            ..Default::default()
        };
        config.profiles.clear();
        config.dotfiles.files = vec![
            DotfileEntry::Simple(".gitconfig".to_string()),
            DotfileEntry::WithOptions {
                path: ".zshrc".to_string(),
                create_if_missing: false,
                on_conflict: OnConflict::Prompt,
            },
        ];
        config.dotfiles.dirs = vec![".config/karabiner".to_string()];

        config.migrate_v1_to_v2();

        assert!(config.profiles.contains_key("dev"));
        let dev = config.profiles.get("dev").unwrap();
        assert_eq!(dev.dotfiles.len(), 2);
        assert_eq!(dev.dotfiles[0].path(), ".gitconfig");
        assert!(dev.dotfiles[0].create_if_missing()); // Simple -> preserves create_if_missing=true
        assert_eq!(dev.dotfiles[1].path(), ".zshrc");
        assert!(!dev.dotfiles[1].shared());
        assert!(!dev.dotfiles[1].create_if_missing()); // WithOptions preserves false
        assert_eq!(dev.dirs, vec![".config/karabiner"]);
        // All managers enabled in default config
        assert!(dev.packages.contains(&"brew".to_string()));
        assert!(dev.packages.contains(&"npm".to_string()));
    }

    #[test]
    fn test_profile_dotfile_entry_shared() {
        let entry = ProfileDotfileEntry::WithOptions {
            path: ".gitconfig".to_string(),
            shared: true,
            create_if_missing: false,
            on_conflict: Default::default(),
        };
        assert!(entry.shared());
        assert_eq!(entry.path(), ".gitconfig");

        let simple = ProfileDotfileEntry::Simple(".zshrc".to_string());
        assert!(!simple.shared());
    }

    #[test]
    fn test_profile_dotfile_entry_toml_roundtrip() {
        let toml_str = r#"
config_version = 2

[sync]
interval = "5m"
strategy = "last-write-wins"

[backend]
type = "git"
url = ""

[packages.brew]
enabled = true
sync_casks = true
sync_taps = true

[packages.npm]
enabled = true
sync_versions = false

[dotfiles]
files = []

[profiles.dev]
dotfiles = [
    ".zshrc",
    { path = ".gitconfig", shared = true },
    { path = ".config/nvim/init.lua", create_if_missing = true },
]
packages = ["brew", "npm"]
"#;
        let parsed: Config = toml::from_str(toml_str).unwrap();
        let dev = parsed.profiles.get("dev").unwrap();
        assert_eq!(dev.dotfiles.len(), 3);
        assert_eq!(dev.dotfiles[0].path(), ".zshrc");
        assert!(!dev.dotfiles[0].shared());
        assert_eq!(dev.dotfiles[1].path(), ".gitconfig");
        assert!(dev.dotfiles[1].shared());
        assert!(dev.dotfiles[2].create_if_missing());
        assert_eq!(dev.packages, vec!["brew", "npm"]);
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

    #[test]
    fn test_v1_config_toml_migrates_on_parse() {
        let v1_toml = r#"
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
files = [
    ".gitconfig",
    { path = ".zshrc", create_if_missing = false },
]
dirs = [".config/karabiner"]
"#;
        let mut config: Config = toml::from_str(v1_toml).unwrap();
        assert_eq!(config.config_version, 1);
        assert!(config.profiles.is_empty());

        config.migrate_v1_to_v2();

        let dev = config.profiles.get("dev").unwrap();
        assert_eq!(dev.dotfiles.len(), 2);
        assert_eq!(dev.dotfiles[0].path(), ".gitconfig");
        // Simple(".gitconfig") → create_if_missing preserved as true
        assert!(dev.dotfiles[0].create_if_missing());
        assert_eq!(dev.dotfiles[1].path(), ".zshrc");
        // WithOptions{false} → preserved as false
        assert!(!dev.dotfiles[1].create_if_missing());
        assert_eq!(dev.dirs, vec![".config/karabiner"]);
    }

    #[test]
    fn test_v1_with_disabled_managers_migrates_correctly() {
        let v1_toml = r#"
config_version = 1

[sync]
interval = "5m"
strategy = "last-write-wins"

[backend]
type = "git"
url = ""

[packages]
remove_unlisted = false

[packages.brew]
enabled = true
sync_casks = true
sync_taps = true

[packages.npm]
enabled = false
sync_versions = false

[packages.pnpm]
enabled = true
sync_versions = false

[packages.bun]
enabled = true
sync_versions = false

[packages.gem]
enabled = true
sync_versions = false

[packages.uv]
enabled = false
sync_versions = false

[dotfiles]
files = [".zshrc"]
"#;
        let mut config: Config = toml::from_str(v1_toml).unwrap();
        config.migrate_v1_to_v2();

        let dev = config.profiles.get("dev").unwrap();
        assert!(dev.packages.contains(&"brew".to_string()));
        assert!(!dev.packages.contains(&"npm".to_string()));
        assert!(!dev.packages.contains(&"uv".to_string()));
        assert!(dev.packages.contains(&"pnpm".to_string()));
    }

    #[test]
    fn test_v1_with_existing_machine_profiles_preserved() {
        let mut config = Config {
            config_version: 1,
            ..Default::default()
        };
        config.profiles.clear();
        config
            .machine_profiles
            .insert("my-server".to_string(), "server".to_string());
        config.dotfiles.files = vec![DotfileEntry::Simple(".zshrc".to_string())];

        config.migrate_v1_to_v2();

        // "dev" profile created
        assert!(config.profiles.contains_key("dev"));
        // "server" profile NOT auto-created (only "dev" is)
        assert!(!config.profiles.contains_key("server"));
        // machine_profiles unchanged
        assert_eq!(
            config.machine_profiles.get("my-server"),
            Some(&"server".to_string())
        );
        // Dangling profile reference: effective_dotfiles falls through to global
        assert_eq!(
            config.effective_dotfiles("my-server").len(),
            config.dotfiles.files.len()
        );
    }

    #[test]
    fn test_effective_dotfiles_post_migration() {
        let mut config = Config {
            config_version: 1,
            ..Default::default()
        };
        config.profiles.clear();
        config.dotfiles.files = vec![
            DotfileEntry::Simple(".gitconfig".to_string()),
            DotfileEntry::WithOptions {
                path: ".zshrc".to_string(),
                create_if_missing: false,
                on_conflict: OnConflict::Prompt,
            },
        ];

        let original_paths: Vec<String> = config
            .dotfiles
            .files
            .iter()
            .map(|e| e.path().to_string())
            .collect();

        config.migrate_v1_to_v2();

        // Any unassigned machine defaults to "dev" profile
        let effective = config.effective_dotfiles("any-machine");
        let effective_paths: Vec<String> = effective.iter().map(|e| e.path().to_string()).collect();
        assert_eq!(effective_paths, original_paths);
    }

    #[test]
    fn test_is_manager_enabled_post_migration() {
        // All managers enabled
        let mut config = Config {
            config_version: 1,
            ..Default::default()
        };
        config.profiles.clear();
        config.migrate_v1_to_v2();
        assert!(config.is_manager_enabled("any", "brew"));
        assert!(config.is_manager_enabled("any", "npm"));

        // npm disabled globally
        let mut config2 = Config {
            config_version: 1,
            ..Default::default()
        };
        config2.profiles.clear();
        config2.packages.npm.enabled = false;
        config2.migrate_v1_to_v2();
        // Global disable takes precedence even though profile has packages list
        assert!(!config2.is_manager_enabled("any", "npm"));
        assert!(config2.is_manager_enabled("any", "brew"));
    }

    #[test]
    fn test_v1_migration_idempotent() {
        let mut config = Config {
            config_version: 1,
            ..Default::default()
        };
        config.profiles.clear();
        config.dotfiles.files = vec![DotfileEntry::Simple(".zshrc".to_string())];

        config.migrate_v1_to_v2();
        let dev_first = config.profiles.get("dev").unwrap().clone();

        // Calling again overwrites "dev" with same content
        config.migrate_v1_to_v2();
        let dev_second = config.profiles.get("dev").unwrap();

        assert_eq!(config.profiles.len(), 1);
        assert_eq!(dev_first.dotfiles.len(), dev_second.dotfiles.len());
        assert_eq!(dev_first.packages, dev_second.packages);
    }

    #[test]
    fn test_v2_config_with_empty_profiles_no_migration() {
        // v2 config with no profiles should NOT trigger migration in load()
        // (the guard is: config_version < 2 && profiles.is_empty())
        let v2_toml = r#"
config_version = 2

[sync]
interval = "5m"
strategy = "last-write-wins"

[backend]
type = "git"
url = ""

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
        let config: Config = toml::from_str(v2_toml).unwrap();
        assert_eq!(config.config_version, 2);
        // No "dev" profile auto-created — v2 with empty profiles is valid
        assert!(config.profiles.is_empty());
        // effective_dotfiles falls through to global
        assert_eq!(config.effective_dotfiles("any").len(), 1);
    }

    #[test]
    fn test_v1_empty_dotfiles_migration() {
        let mut config = Config {
            config_version: 1,
            ..Default::default()
        };
        config.profiles.clear();
        config.dotfiles.files = vec![];
        config.dotfiles.dirs = vec![];

        config.migrate_v1_to_v2();

        let dev = config.profiles.get("dev").unwrap();
        assert!(dev.dotfiles.is_empty());
        assert!(dev.dirs.is_empty());
        // Packages still populated from global config
        assert!(!dev.packages.is_empty());

        // effective_dotfiles returns empty (profile exists with empty dotfiles → uses profile)
        let effective = config.effective_dotfiles("any");
        assert!(effective.is_empty());
    }

    #[test]
    fn test_is_manager_enabled_brew_subtypes_with_profile() {
        let mut config = Config::default();
        config.profiles.insert(
            "dev".to_string(),
            ProfileConfig {
                dotfiles: vec![],
                dirs: vec![],
                packages: vec!["brew".to_string()],
            },
        );

        // brew sub-types normalize to "brew" for profile matching
        assert!(config.is_manager_enabled("any", "brew_formulae"));
        assert!(config.is_manager_enabled("any", "brew_casks"));
        assert!(config.is_manager_enabled("any", "brew_taps"));
        // npm not in profile packages
        assert!(!config.is_manager_enabled("any", "npm"));
    }

    #[test]
    fn test_profile_name_validation() {
        assert!(!Config::is_safe_profile_name(""));
        assert!(!Config::is_safe_profile_name("../etc"));
        assert!(!Config::is_safe_profile_name("shared"));
        assert!(!Config::is_safe_profile_name("tether"));
        assert!(!Config::is_safe_profile_name(".hidden"));
        assert!(!Config::is_safe_profile_name("a/b"));
        assert!(!Config::is_safe_profile_name("a\\b"));
        // Repo root dir names are reserved
        assert!(!Config::is_safe_profile_name("dotfiles"));
        assert!(!Config::is_safe_profile_name("manifests"));
        assert!(!Config::is_safe_profile_name("machines"));
        assert!(!Config::is_safe_profile_name("configs"));
        assert!(!Config::is_safe_profile_name("projects"));
        assert!(!Config::is_safe_profile_name("profiles"));
        assert!(Config::is_safe_profile_name("dev"));
        assert!(Config::is_safe_profile_name("my-server"));
        assert!(Config::is_safe_profile_name("workstation_01"));
    }

    fn opendiff() -> MergeConfig {
        MergeConfig {
            command: "opendiff".to_string(),
            args: ["{local}", "{remote}", "-merge", "{merged}"]
                .map(String::from)
                .to_vec(),
        }
    }

    #[test]
    fn synced_opendiff_falls_back_to_vimdiff_where_missing() {
        let local = LocalMergeConfig::default();
        let merge = effective_merge(&opendiff(), &local, |_| false);
        assert_eq!(merge.command, "vimdiff");
        assert_eq!(merge.args, three_way_args());

        let merge = effective_merge(&opendiff(), &local, |_| true);
        assert_eq!(merge.command, "opendiff");
        assert_eq!(merge.args, opendiff().args);
    }

    #[test]
    fn local_merge_tool_overrides_the_synced_one() {
        let local: LocalConfig = toml::from_str("[merge]\ncommand = \"meld\"\n").unwrap();
        let merge = effective_merge(&opendiff(), &local.merge, |_| false);
        assert_eq!(merge.command, "meld");
        assert_eq!(merge.args, three_way_args());

        let local: LocalConfig =
            toml::from_str("[merge]\ncommand = \"code\"\nargs = [\"--wait\", \"{merged}\"]\n")
                .unwrap();
        let merge = effective_merge(&opendiff(), &local.merge, |_| false);
        assert_eq!(merge.args, ["--wait", "{merged}"]);
    }

    #[test]
    fn local_config_reads_min_release_age_and_rejects_typos() {
        let local: LocalConfig = toml::from_str("[packages]\nmin_release_age_days = 0\n").unwrap();
        assert_eq!(local.packages.min_release_age_days, Some(0));
        assert!(toml::from_str::<LocalConfig>("[packages]\nmin_release_age = 0\n").is_err());
        assert!(toml::from_str::<LocalConfig>("")
            .unwrap()
            .merge
            .command
            .is_none());
    }
}

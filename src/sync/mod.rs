pub mod backup;
pub mod conflict;
pub mod discovery;
pub mod engine;
pub mod git;
pub mod layers;
pub mod merge;
pub mod packages;
pub mod state;
pub mod team;

pub use backup::{
    backup_file, backups_dir, create_backup_dir, list_backup_files, list_backups,
    prune_old_backups, restore_file,
};
pub use conflict::{
    detect_conflict, notify_conflict, notify_conflicts, notify_deferred_casks, ConflictResolution,
    ConflictState, FileConflict, PendingConflict,
};
pub use discovery::discover_sourced_dirs;
pub use engine::SyncEngine;
pub use git::{checkout_id_from_path, extract_org_from_normalized_url, FileLogEntry, GitBackend};
pub use layers::{
    init_layers, list_team_layer_files, map_team_to_personal_name, merge_layers, remerge_all,
    sync_dotfile_with_layers, sync_team_to_layer, LayerSyncResult,
};
pub use merge::{detect_file_type, merge_files, FileType};
pub use packages::{import_packages, sync_packages};
pub use state::{CheckoutInfo, FileState, MachineState, SyncState};
pub use team::{
    default_local_patterns, discover_symlinkable_dirs, extract_org_from_url,
    extract_team_name_from_url, find_team_for_project, get_project_org, glob_match, is_local_file,
    project_matches_team_orgs, resolve_conflict, TeamManifest,
};

use anyhow::Result;
use std::fs::File;
use std::path::{Path, PathBuf};

pub const CURRENT_SYNC_FORMAT_VERSION: u32 = 1;

/// Map a dotfile path to its repo-relative path in the sync repo (flat layout, legacy).
/// e.g., ".zshrc" -> "dotfiles/zshrc.enc" (encrypted) or "dotfiles/.zshrc" (plain)
pub fn dotfile_to_repo_path(dotfile: &str, encrypted: bool) -> String {
    let name = dotfile.trim_start_matches('.');
    if encrypted {
        format!("dotfiles/{}.enc", name)
    } else {
        format!("dotfiles/{}", dotfile)
    }
}

/// Map a dotfile path to its profile-aware repo path.
/// Shared dotfiles: `profiles/shared/zshrc.enc`
/// Profile-specific: `profiles/<profile>/zshrc.enc`
///
/// # Safety
/// Profile name is validated as defense-in-depth against path traversal.
pub fn dotfile_to_repo_path_profiled(
    dotfile: &str,
    encrypted: bool,
    profile: &str,
    shared: bool,
) -> String {
    // Defense-in-depth: reject unsafe profile names to prevent path traversal
    debug_assert!(
        crate::config::Config::is_safe_profile_name(profile),
        "unsafe profile name: {}",
        profile
    );
    let name = dotfile.trim_start_matches('.');
    let subdir = if shared { "shared" } else { profile };
    if encrypted {
        format!("profiles/{}/{}.enc", subdir, name)
    } else {
        format!("profiles/{}/{}", subdir, dotfile)
    }
}

/// Whether the sync repo is still using the pre-profiles flat layout.
pub fn is_pre_migration_repo(sync_path: &Path) -> bool {
    !sync_path.join("profiles").exists()
}

/// Try to read a dotfile from the sync repo, checking profile path first then flat fallback.
/// Returns the path that exists, or the profile path if neither exists.
/// Only falls back to flat layout if profiles/ dir doesn't exist (pre-migration repo).
pub fn resolve_dotfile_repo_path(
    sync_path: &std::path::Path,
    dotfile: &str,
    encrypted: bool,
    profile: &str,
    shared: bool,
) -> String {
    let profiled = dotfile_to_repo_path_profiled(dotfile, encrypted, profile, shared);
    if sync_path.join(&profiled).exists() {
        return profiled;
    }
    // Only fall back to flat layout for un-migrated repos (no profiles/ dir yet).
    // Once profiles/ exists, flat files are leftovers and shouldn't bleed across profiles.
    if is_pre_migration_repo(sync_path) {
        let flat = dotfile_to_repo_path(dotfile, encrypted);
        if sync_path.join(&flat).exists() {
            return flat;
        }
    }
    // Default to profiled path (for new writes)
    profiled
}

/// Migrate flat dotfiles/ to profiled layout.
/// Called on each sync — copies flat files to profile dirs if they don't exist yet.
/// Each file is checked individually, so multiple machines can migrate independently.
/// Flat files are left in place as fallback for un-upgraded machines.
pub fn migrate_repo_to_profiled(
    sync_path: &std::path::Path,
    config: &crate::config::Config,
    machine_id: &str,
) -> anyhow::Result<bool> {
    let encrypted = config.security.encrypt_dotfiles;
    let profile_name = config.profile_name(machine_id);
    let mut migrated_any = false;

    if let Some(entries) = config.profile_dotfiles(machine_id) {
        let dotfiles_dir = sync_path.join("dotfiles");
        for entry in entries {
            let pattern = entry.path();
            let shared = entry.shared();

            // Expand glob patterns by scanning flat layout
            let expanded = if is_glob_pattern(pattern) && encrypted {
                expand_from_sync_repo(pattern, &dotfiles_dir)
            } else {
                vec![pattern.to_string()]
            };

            for dotfile in &expanded {
                let flat_path = dotfile_to_repo_path(dotfile, encrypted);
                let profiled_path =
                    dotfile_to_repo_path_profiled(dotfile, encrypted, profile_name, shared);

                let flat_full = sync_path.join(&flat_path);
                let profiled_full = sync_path.join(&profiled_path);

                if flat_full.exists() && !profiled_full.exists() {
                    if let Some(parent) = profiled_full.parent() {
                        std::fs::create_dir_all(parent)?;
                    }
                    std::fs::copy(&flat_full, &profiled_full)?;
                    migrated_any = true;
                }
            }
        }
    }

    Ok(migrated_any)
}

/// Minimum CLI version that uses `profiles/` layout.
const PROFILES_MIN_VERSION: &str = "1.11.0";

/// Remove legacy `dotfiles/` tree once all machines are on >= 1.11.0.
/// Safe because: profiled files now live under `profiles/`, tether config under `configs/tether/`.
pub fn cleanup_legacy_dotfiles(sync_path: &std::path::Path) -> anyhow::Result<bool> {
    let dotfiles_dir = sync_path.join("dotfiles");
    if !dotfiles_dir.exists() {
        return Ok(false);
    }

    let machines = crate::sync::state::MachineState::list_all(sync_path)?;
    if machines.is_empty() {
        return Ok(false);
    }
    for m in &machines {
        if m.cli_version.is_empty() || !version_gte(&m.cli_version, PROFILES_MIN_VERSION) {
            return Ok(false);
        }
    }

    // Migrate tether config to configs/tether/ if it only exists in dotfiles/tether/
    let legacy_config = dotfiles_dir.join("tether/config.toml.enc");
    let new_config = sync_path.join("configs/tether/config.toml.enc");
    if legacy_config.exists() && !new_config.exists() {
        if let Some(parent) = new_config.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::copy(&legacy_config, &new_config)?;
    }

    std::fs::remove_dir_all(&dotfiles_dir)?;
    Ok(true)
}

/// Simple semver comparison: is `version` >= `min`?
fn version_gte(version: &str, min: &str) -> bool {
    let parse = |s: &str| -> (u32, u32, u32) {
        // Strip pre-release suffix (e.g., "1.11.0-beta.1" -> "1.11.0")
        let base = s.split('-').next().unwrap_or(s);
        let mut parts = base.split('.').filter_map(|p| p.parse::<u32>().ok());
        (
            parts.next().unwrap_or(0),
            parts.next().unwrap_or(0),
            parts.next().unwrap_or(0),
        )
    };
    parse(version) >= parse(min)
}

/// Check sync repo format version. Creates file if missing, errors if newer than supported.
pub fn check_sync_format_version(sync_path: &Path) -> Result<()> {
    let version_file = sync_path.join("format_version");
    if version_file.exists() {
        let content = std::fs::read_to_string(&version_file)?;
        let version: u32 = content
            .trim()
            .parse()
            .map_err(|_| anyhow::anyhow!("Invalid format_version file"))?;
        if version > CURRENT_SYNC_FORMAT_VERSION {
            anyhow::bail!(
                "Sync repo format version {} is newer than supported ({}). Run: brew upgrade tether",
                version,
                CURRENT_SYNC_FORMAT_VERSION
            );
        }
    } else {
        std::fs::create_dir_all(sync_path)?;
        std::fs::write(&version_file, format!("{}\n", CURRENT_SYNC_FORMAT_VERSION))?;
    }
    Ok(())
}

/// Acquire an exclusive lock on ~/.tether/sync.lock.
/// If `wait` is true (CLI), retries up to 20 times at 100ms intervals.
/// If `wait` is false (daemon), fails immediately.
pub fn acquire_sync_lock(wait: bool) -> Result<File> {
    use fs2::FileExt;

    let lock_path = crate::home_dir()?.join(".tether/sync.lock");
    std::fs::create_dir_all(lock_path.parent().unwrap())?;
    let file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(&lock_path)?;

    if wait {
        for _ in 0..20 {
            if file.try_lock_exclusive().is_ok() {
                return Ok(file);
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
        anyhow::bail!("Could not acquire sync lock after 2 seconds. Another sync may be running.");
    } else {
        file.try_lock_exclusive()
            .map_err(|_| anyhow::anyhow!("Sync already in progress, skipping"))?;
    }
    Ok(file)
}

/// Get the canonical storage path for a project config file.
/// Files are stored at ~/.tether/projects/<normalized_url>/<rel_path>
pub fn canonical_project_file_path(normalized_url: &str, rel_path: &str) -> Result<PathBuf> {
    // Validate no path traversal in inputs
    if normalized_url.contains("..") || rel_path.contains("..") {
        anyhow::bail!("Path traversal not allowed in project path");
    }
    if normalized_url.starts_with('/') || rel_path.starts_with('/') {
        anyhow::bail!("Absolute paths not allowed in project path");
    }

    let home = crate::home_dir()?;
    Ok(home
        .join(".tether/projects")
        .join(normalized_url)
        .join(rel_path))
}

/// Check if a pattern contains glob metacharacters
pub fn is_glob_pattern(pattern: &str) -> bool {
    pattern.contains('*') || pattern.contains('?') || pattern.contains('[')
}

/// Expand glob patterns in dotfile paths.
/// Returns vec of relative paths (e.g., ".config/gcloud/foo.json").
/// If pattern has no glob chars, returns it unchanged.
/// Logs warning if glob pattern matches nothing.
pub fn expand_dotfile_glob(pattern: &str, home: &Path) -> Vec<String> {
    if !is_glob_pattern(pattern) {
        return vec![pattern.to_string()];
    }

    let full_pattern = home.join(pattern);
    match glob::glob(&full_pattern.to_string_lossy()) {
        Ok(paths) => {
            let expanded: Vec<String> = paths
                .filter_map(Result::ok)
                .filter_map(|p| {
                    p.strip_prefix(home)
                        .ok()
                        .map(|r| r.to_string_lossy().to_string())
                })
                .collect();
            if expanded.is_empty() {
                log::warn!("Glob pattern '{}' matched no files", pattern);
                vec![]
            } else {
                expanded
            }
        }
        Err(e) => {
            log::warn!("Invalid glob pattern '{}': {}", pattern, e);
            vec![]
        }
    }
}

/// Expand glob pattern by scanning what exists in the sync repo's dotfiles dir.
/// Used during pull to find .enc files matching a pattern.
/// Logs warning if glob pattern matches nothing.
pub fn expand_from_sync_repo(pattern: &str, dotfiles_dir: &Path) -> Vec<String> {
    if !is_glob_pattern(pattern) {
        return vec![pattern.to_string()];
    }

    // Convert dotfile pattern to enc filename pattern
    // e.g., ".config/gcloud/*.json" -> "config/gcloud/*.json.enc"
    let filename_pattern = pattern.trim_start_matches('.');
    let enc_pattern = format!("{}.enc", filename_pattern);

    let full_pattern = dotfiles_dir.join(&enc_pattern);
    match glob::glob(&full_pattern.to_string_lossy()) {
        Ok(paths) => {
            let expanded: Vec<String> = paths
                .filter_map(Result::ok)
                .filter_map(|p| {
                    p.strip_prefix(dotfiles_dir).ok().and_then(|r| {
                        let s = r.to_string_lossy();
                        // Remove .enc suffix and add leading dot
                        s.strip_suffix(".enc").map(|s| format!(".{}", s))
                    })
                })
                .collect();
            if expanded.is_empty() {
                log::warn!("Glob pattern '{}' matched no files in sync repo", pattern);
                vec![]
            } else {
                expanded
            }
        }
        Err(e) => {
            log::warn!("Invalid glob pattern '{}': {}", pattern, e);
            vec![]
        }
    }
}

/// Atomically write content to a file by writing to a temp file and renaming.
/// This prevents file corruption from interrupted writes.
pub fn atomic_write(path: &Path, content: &[u8]) -> Result<()> {
    use std::io::Write;

    let parent = path
        .parent()
        .ok_or_else(|| anyhow::anyhow!("Invalid path: no parent directory"))?;
    std::fs::create_dir_all(parent)?;

    // Create temp file in same directory (required for atomic rename)
    let mut temp = tempfile::NamedTempFile::new_in(parent)?;
    temp.write_all(content)?;
    temp.flush()?;

    // Persist atomically renames the temp file to the target
    temp.persist(path)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_dotfile_to_repo_path() {
        assert_eq!(dotfile_to_repo_path(".zshrc", true), "dotfiles/zshrc.enc");
        assert_eq!(
            dotfile_to_repo_path(".config/nvim/init.lua", true),
            "dotfiles/config/nvim/init.lua.enc"
        );
        assert_eq!(dotfile_to_repo_path(".zshrc", false), "dotfiles/.zshrc");
        assert_eq!(
            dotfile_to_repo_path(".gitconfig", false),
            "dotfiles/.gitconfig"
        );
    }

    #[test]
    fn test_dotfile_to_repo_path_profiled() {
        assert_eq!(
            dotfile_to_repo_path_profiled(".zshrc", true, "dev", false),
            "profiles/dev/zshrc.enc"
        );
        assert_eq!(
            dotfile_to_repo_path_profiled(".gitconfig", true, "dev", true),
            "profiles/shared/gitconfig.enc"
        );
        assert_eq!(
            dotfile_to_repo_path_profiled(".zshrc", true, "server", false),
            "profiles/server/zshrc.enc"
        );
        assert_eq!(
            dotfile_to_repo_path_profiled(".config/nvim/init.lua", true, "dev", false),
            "profiles/dev/config/nvim/init.lua.enc"
        );
    }

    #[test]
    fn test_resolve_dotfile_repo_path_fallback() {
        let tmp = TempDir::new().unwrap();
        let sync_path = tmp.path();

        // No files exist: returns profiled path
        let result = resolve_dotfile_repo_path(sync_path, ".zshrc", true, "dev", false);
        assert_eq!(result, "profiles/dev/zshrc.enc");

        // Flat file only (no profiles/ dir = pre-migration): should fallback
        let flat_dir = sync_path.join("dotfiles");
        std::fs::create_dir_all(&flat_dir).unwrap();
        std::fs::write(flat_dir.join("zshrc.enc"), "data").unwrap();
        let result = resolve_dotfile_repo_path(sync_path, ".zshrc", true, "dev", false);
        assert_eq!(result, "dotfiles/zshrc.enc");

        // Once profiles/ exists, flat fallback is skipped
        let prof_dir = sync_path.join("profiles/dev");
        std::fs::create_dir_all(&prof_dir).unwrap();
        let result = resolve_dotfile_repo_path(sync_path, ".zshrc", true, "dev", false);
        assert_eq!(result, "profiles/dev/zshrc.enc");

        // Profiled file exists: returns it
        std::fs::write(prof_dir.join("zshrc.enc"), "data").unwrap();
        let result = resolve_dotfile_repo_path(sync_path, ".zshrc", true, "dev", false);
        assert_eq!(result, "profiles/dev/zshrc.enc");
    }

    #[test]
    fn test_file_log_entry_parse() {
        let line =
            "abc123def456|abc1234|2024-01-15T10:30:00Z|macbook-pro|Sync dotfiles and packages";
        let entry = FileLogEntry::parse(line).unwrap();
        assert_eq!(entry.commit_hash, "abc123def456");
        assert_eq!(entry.short_hash, "abc1234");
        assert_eq!(entry.machine_id, "macbook-pro");
        assert_eq!(entry.message, "Sync dotfiles and packages");
    }

    #[test]
    fn test_file_log_entry_parse_invalid() {
        assert!(FileLogEntry::parse("not enough parts").is_none());
        assert!(FileLogEntry::parse("a|b|not-a-date|c|d").is_none());
    }

    #[test]
    fn test_is_glob_pattern() {
        assert!(!is_glob_pattern(".bashrc"));
        assert!(!is_glob_pattern(".config/git/config"));
        assert!(is_glob_pattern("*.json"));
        assert!(is_glob_pattern(".config/gcloud/*.json"));
        assert!(is_glob_pattern("file?.txt"));
        assert!(is_glob_pattern("[abc].txt"));
    }

    #[test]
    fn test_expand_dotfile_glob_no_glob() {
        let tmp = TempDir::new().unwrap();
        let result = expand_dotfile_glob(".bashrc", tmp.path());
        assert_eq!(result, vec![".bashrc"]);
    }

    #[test]
    fn test_expand_dotfile_glob_with_matches() {
        let tmp = TempDir::new().unwrap();
        let config_dir = tmp.path().join(".config/gcloud");
        std::fs::create_dir_all(&config_dir).unwrap();
        std::fs::write(config_dir.join("foo.json"), "{}").unwrap();
        std::fs::write(config_dir.join("bar.json"), "{}").unwrap();
        std::fs::write(config_dir.join("other.txt"), "").unwrap();

        let mut result = expand_dotfile_glob(".config/gcloud/*.json", tmp.path());
        result.sort();
        assert_eq!(
            result,
            vec![".config/gcloud/bar.json", ".config/gcloud/foo.json"]
        );
    }

    #[test]
    fn test_expand_dotfile_glob_no_matches() {
        let tmp = TempDir::new().unwrap();
        let result = expand_dotfile_glob(".config/nonexistent/*.json", tmp.path());
        assert!(result.is_empty());
    }

    #[test]
    fn test_expand_from_sync_repo_no_glob() {
        let tmp = TempDir::new().unwrap();
        let result = expand_from_sync_repo(".bashrc", tmp.path());
        assert_eq!(result, vec![".bashrc"]);
    }

    #[test]
    fn test_expand_from_sync_repo_with_matches() {
        let tmp = TempDir::new().unwrap();
        let config_dir = tmp.path().join("config/gcloud");
        std::fs::create_dir_all(&config_dir).unwrap();
        std::fs::write(config_dir.join("foo.json.enc"), "encrypted").unwrap();
        std::fs::write(config_dir.join("bar.json.enc"), "encrypted").unwrap();

        let mut result = expand_from_sync_repo(".config/gcloud/*.json", tmp.path());
        result.sort();
        assert_eq!(
            result,
            vec![".config/gcloud/bar.json", ".config/gcloud/foo.json"]
        );
    }

    #[test]
    fn test_expand_from_sync_repo_no_matches() {
        let tmp = TempDir::new().unwrap();
        let result = expand_from_sync_repo(".config/nonexistent/*.json", tmp.path());
        assert!(result.is_empty());
    }

    #[test]
    fn test_format_version_creates_file() {
        let tmp = TempDir::new().unwrap();
        check_sync_format_version(tmp.path()).unwrap();
        let content = std::fs::read_to_string(tmp.path().join("format_version")).unwrap();
        assert_eq!(content, format!("{}\n", CURRENT_SYNC_FORMAT_VERSION));
    }

    #[test]
    fn test_format_version_accepts_current() {
        let tmp = TempDir::new().unwrap();
        std::fs::write(
            tmp.path().join("format_version"),
            format!("{}\n", CURRENT_SYNC_FORMAT_VERSION),
        )
        .unwrap();
        check_sync_format_version(tmp.path()).unwrap();
    }

    #[test]
    fn test_format_version_rejects_newer() {
        let tmp = TempDir::new().unwrap();
        std::fs::write(
            tmp.path().join("format_version"),
            format!("{}\n", CURRENT_SYNC_FORMAT_VERSION + 1),
        )
        .unwrap();
        let err = check_sync_format_version(tmp.path()).unwrap_err();
        assert!(err.to_string().contains("brew upgrade tether"));
    }

    #[test]
    fn test_format_version_accepts_older() {
        let tmp = TempDir::new().unwrap();
        std::fs::write(tmp.path().join("format_version"), "0\n").unwrap();
        check_sync_format_version(tmp.path()).unwrap();
    }

    #[test]
    fn test_format_version_rejects_invalid() {
        let tmp = TempDir::new().unwrap();
        std::fs::write(tmp.path().join("format_version"), "abc\n").unwrap();
        let err = check_sync_format_version(tmp.path()).unwrap_err();
        assert!(err.to_string().contains("Invalid format_version"));
    }

    /// Helper: build a v1-style Config then migrate to v2, returning the migrated config.
    fn make_migrated_config(
        dotfiles: Vec<crate::config::DotfileEntry>,
        dirs: Vec<String>,
        packages_override: Option<crate::config::PackagesConfig>,
    ) -> crate::config::Config {
        let mut config = crate::config::Config {
            config_version: 1,
            ..Default::default()
        };
        config.profiles.clear();
        config.dotfiles.files = dotfiles;
        config.dotfiles.dirs = dirs;
        if let Some(pkg) = packages_override {
            config.packages = pkg;
        }
        config.migrate_v1_to_v2();
        config
    }

    #[test]
    fn test_migrate_flat_to_profiled_basic() {
        let tmp = TempDir::new().unwrap();
        let sync_path = tmp.path();

        let config = make_migrated_config(
            vec![
                crate::config::DotfileEntry::Simple(".zshrc".to_string()),
                crate::config::DotfileEntry::Simple(".gitconfig".to_string()),
            ],
            vec![],
            None,
        );

        // Create flat files
        let dotfiles_dir = sync_path.join("dotfiles");
        std::fs::create_dir_all(&dotfiles_dir).unwrap();
        std::fs::write(dotfiles_dir.join("zshrc.enc"), "encrypted-zshrc").unwrap();
        std::fs::write(dotfiles_dir.join("gitconfig.enc"), "encrypted-gitconfig").unwrap();

        let migrated = migrate_repo_to_profiled(sync_path, &config, "my-machine").unwrap();
        assert!(migrated);

        // Profiled files created with correct content
        assert_eq!(
            std::fs::read_to_string(sync_path.join("profiles/dev/zshrc.enc")).unwrap(),
            "encrypted-zshrc"
        );
        assert_eq!(
            std::fs::read_to_string(sync_path.join("profiles/dev/gitconfig.enc")).unwrap(),
            "encrypted-gitconfig"
        );

        // Flat files still exist (not moved, copied)
        assert!(dotfiles_dir.join("zshrc.enc").exists());
        assert!(dotfiles_dir.join("gitconfig.enc").exists());
    }

    #[test]
    fn test_migrate_flat_to_profiled_shared() {
        let tmp = TempDir::new().unwrap();
        let sync_path = tmp.path();

        let mut config = crate::config::Config {
            config_version: 2,
            ..Default::default()
        };
        config.profiles.insert(
            "dev".to_string(),
            crate::config::ProfileConfig {
                dotfiles: vec![crate::config::ProfileDotfileEntry::WithOptions {
                    path: ".gitconfig".to_string(),
                    shared: true,
                    create_if_missing: false,
                }],
                dirs: vec![],
                packages: vec![],
            },
        );

        // Create flat file
        let dotfiles_dir = sync_path.join("dotfiles");
        std::fs::create_dir_all(&dotfiles_dir).unwrap();
        std::fs::write(dotfiles_dir.join("gitconfig.enc"), "encrypted").unwrap();

        let migrated = migrate_repo_to_profiled(sync_path, &config, "my-machine").unwrap();
        assert!(migrated);

        // Shared dotfile goes to profiles/shared/, not profiles/dev/
        assert!(sync_path.join("profiles/shared/gitconfig.enc").exists());
        assert!(!sync_path.join("profiles/dev/gitconfig.enc").exists());
        // Flat file still exists
        assert!(dotfiles_dir.join("gitconfig.enc").exists());
    }

    #[test]
    fn test_migrate_flat_to_profiled_idempotent() {
        let tmp = TempDir::new().unwrap();
        let sync_path = tmp.path();

        let config = make_migrated_config(
            vec![crate::config::DotfileEntry::Simple(".zshrc".to_string())],
            vec![],
            None,
        );

        let dotfiles_dir = sync_path.join("dotfiles");
        std::fs::create_dir_all(&dotfiles_dir).unwrap();
        std::fs::write(dotfiles_dir.join("zshrc.enc"), "encrypted").unwrap();

        // First migration
        let first = migrate_repo_to_profiled(sync_path, &config, "my-machine").unwrap();
        assert!(first);
        let content_after_first =
            std::fs::read_to_string(sync_path.join("profiles/dev/zshrc.enc")).unwrap();

        // Second migration — no-op
        let second = migrate_repo_to_profiled(sync_path, &config, "my-machine").unwrap();
        assert!(!second);
        let content_after_second =
            std::fs::read_to_string(sync_path.join("profiles/dev/zshrc.enc")).unwrap();
        assert_eq!(content_after_first, content_after_second);
    }

    #[test]
    fn test_migrate_two_machines_different_profiles() {
        let tmp = TempDir::new().unwrap();
        let sync_path = tmp.path();

        let mut config = crate::config::Config {
            config_version: 2,
            ..Default::default()
        };
        config.profiles.insert(
            "dev".to_string(),
            crate::config::ProfileConfig {
                dotfiles: vec![
                    crate::config::ProfileDotfileEntry::Simple(".zshrc".to_string()),
                    crate::config::ProfileDotfileEntry::Simple(".gitconfig".to_string()),
                ],
                dirs: vec![],
                packages: vec![],
            },
        );
        config.profiles.insert(
            "server".to_string(),
            crate::config::ProfileConfig {
                dotfiles: vec![crate::config::ProfileDotfileEntry::Simple(
                    ".bashrc".to_string(),
                )],
                dirs: vec![],
                packages: vec![],
            },
        );
        config
            .machine_profiles
            .insert("machine-a".to_string(), "dev".to_string());
        config
            .machine_profiles
            .insert("machine-b".to_string(), "server".to_string());

        // Create flat files (union of both profiles)
        let dotfiles_dir = sync_path.join("dotfiles");
        std::fs::create_dir_all(&dotfiles_dir).unwrap();
        std::fs::write(dotfiles_dir.join("zshrc.enc"), "z").unwrap();
        std::fs::write(dotfiles_dir.join("gitconfig.enc"), "g").unwrap();
        std::fs::write(dotfiles_dir.join("bashrc.enc"), "b").unwrap();

        // Machine A migrates
        let a = migrate_repo_to_profiled(sync_path, &config, "machine-a").unwrap();
        assert!(a);
        assert!(sync_path.join("profiles/dev/zshrc.enc").exists());
        assert!(sync_path.join("profiles/dev/gitconfig.enc").exists());

        // Machine B migrates
        let b = migrate_repo_to_profiled(sync_path, &config, "machine-b").unwrap();
        assert!(b);
        assert!(sync_path.join("profiles/server/bashrc.enc").exists());

        // Cross-profile isolation: dev doesn't have server files, vice versa
        assert!(!sync_path.join("profiles/dev/bashrc.enc").exists());
        assert!(!sync_path.join("profiles/server/zshrc.enc").exists());
        assert!(!sync_path.join("profiles/server/gitconfig.enc").exists());

        // Flat files untouched
        assert!(dotfiles_dir.join("zshrc.enc").exists());
        assert!(dotfiles_dir.join("gitconfig.enc").exists());
        assert!(dotfiles_dir.join("bashrc.enc").exists());
    }

    #[test]
    fn test_migrate_nested_dotfile() {
        let tmp = TempDir::new().unwrap();
        let sync_path = tmp.path();

        let config = make_migrated_config(
            vec![crate::config::DotfileEntry::Simple(
                ".config/nvim/init.lua".to_string(),
            )],
            vec![],
            None,
        );

        // Flat nested: dotfiles/config/nvim/init.lua.enc
        let nested_dir = sync_path.join("dotfiles/config/nvim");
        std::fs::create_dir_all(&nested_dir).unwrap();
        std::fs::write(nested_dir.join("init.lua.enc"), "encrypted").unwrap();

        let migrated = migrate_repo_to_profiled(sync_path, &config, "my-machine").unwrap();
        assert!(migrated);

        // Nested structure preserved under profile dir
        assert!(sync_path
            .join("profiles/dev/config/nvim/init.lua.enc")
            .exists());
    }

    #[test]
    fn test_migrate_no_flat_files_noop() {
        let tmp = TempDir::new().unwrap();
        let sync_path = tmp.path();

        let config = make_migrated_config(
            vec![crate::config::DotfileEntry::Simple(".zshrc".to_string())],
            vec![],
            None,
        );

        // No flat files exist (fresh v2 install)
        std::fs::create_dir_all(sync_path.join("dotfiles")).unwrap();
        let migrated = migrate_repo_to_profiled(sync_path, &config, "my-machine").unwrap();
        assert!(!migrated);
        assert!(!sync_path.join("profiles/dev").exists());
    }

    #[test]
    fn test_migrate_flat_to_profiled_unencrypted() {
        let tmp = TempDir::new().unwrap();
        let sync_path = tmp.path();

        let mut config = make_migrated_config(
            vec![crate::config::DotfileEntry::Simple(".zshrc".to_string())],
            vec![],
            None,
        );
        config.security.encrypt_dotfiles = false;

        // Unencrypted flat: dotfiles/.zshrc (keeps dot prefix)
        let dotfiles_dir = sync_path.join("dotfiles");
        std::fs::create_dir_all(&dotfiles_dir).unwrap();
        std::fs::write(dotfiles_dir.join(".zshrc"), "plain-content").unwrap();

        let migrated = migrate_repo_to_profiled(sync_path, &config, "my-machine").unwrap();
        assert!(migrated);
        assert_eq!(
            std::fs::read_to_string(sync_path.join("profiles/dev/.zshrc")).unwrap(),
            "plain-content"
        );
    }

    #[test]
    fn test_resolve_falls_back_to_flat_when_no_profile_dir() {
        let tmp = TempDir::new().unwrap();
        let sync_path = tmp.path();

        // Only flat file exists (pre-migration state)
        let dotfiles_dir = sync_path.join("dotfiles");
        std::fs::create_dir_all(&dotfiles_dir).unwrap();
        std::fs::write(dotfiles_dir.join("zshrc.enc"), "data").unwrap();

        let result = resolve_dotfile_repo_path(sync_path, ".zshrc", true, "dev", false);
        assert_eq!(result, "dotfiles/zshrc.enc");
    }

    #[test]
    fn test_resolve_prefers_profiled_over_flat() {
        let tmp = TempDir::new().unwrap();
        let sync_path = tmp.path();

        // Both flat and profiled exist
        let flat_dir = sync_path.join("dotfiles");
        std::fs::create_dir_all(&flat_dir).unwrap();
        std::fs::write(flat_dir.join("zshrc.enc"), "old").unwrap();

        let prof_dir = sync_path.join("profiles/dev");
        std::fs::create_dir_all(&prof_dir).unwrap();
        std::fs::write(prof_dir.join("zshrc.enc"), "new").unwrap();

        let result = resolve_dotfile_repo_path(sync_path, ".zshrc", true, "dev", false);
        assert_eq!(result, "profiles/dev/zshrc.enc");
    }

    #[test]
    fn test_version_gte() {
        assert!(version_gte("1.11.0", "1.11.0"));
        assert!(version_gte("1.12.0", "1.11.0"));
        assert!(version_gte("2.0.0", "1.11.0"));
        assert!(!version_gte("1.10.0", "1.11.0"));
        assert!(!version_gte("1.9.9", "1.11.0"));
        assert!(version_gte("1.11.0-beta.1", "1.11.0"));
        assert!(!version_gte("", "1.11.0"));
    }

    #[test]
    fn test_cleanup_legacy_skips_when_old_machine() {
        let tmp = TempDir::new().unwrap();
        let sync_path = tmp.path();

        // Create machine states: one old, one new
        let machines_dir = sync_path.join("machines");
        std::fs::create_dir_all(&machines_dir).unwrap();
        std::fs::write(
            machines_dir.join("new-mac.json"),
            r#"{"machine_id":"new-mac","hostname":"h","last_sync":"2026-01-01T00:00:00Z","cli_version":"1.11.0","files":{},"packages":{}}"#,
        ).unwrap();
        std::fs::write(
            machines_dir.join("old-mac.json"),
            r#"{"machine_id":"old-mac","hostname":"h","last_sync":"2026-01-01T00:00:00Z","cli_version":"1.10.0","files":{},"packages":{}}"#,
        ).unwrap();

        // Create legacy flat files
        let dotfiles_dir = sync_path.join("dotfiles");
        std::fs::create_dir_all(&dotfiles_dir).unwrap();
        std::fs::write(dotfiles_dir.join("zshrc.enc"), "data").unwrap();

        let cleaned = cleanup_legacy_dotfiles(sync_path).unwrap();
        assert!(!cleaned);
        assert!(dotfiles_dir.join("zshrc.enc").exists());
    }

    #[test]
    fn test_cleanup_legacy_runs_when_all_upgraded() {
        let tmp = TempDir::new().unwrap();
        let sync_path = tmp.path();

        // Both machines on >= 1.11.0
        let machines_dir = sync_path.join("machines");
        std::fs::create_dir_all(&machines_dir).unwrap();
        std::fs::write(
            machines_dir.join("mac-a.json"),
            r#"{"machine_id":"mac-a","hostname":"h","last_sync":"2026-01-01T00:00:00Z","cli_version":"1.11.0","files":{},"packages":{}}"#,
        ).unwrap();
        std::fs::write(
            machines_dir.join("mac-b.json"),
            r#"{"machine_id":"mac-b","hostname":"h","last_sync":"2026-01-01T00:00:00Z","cli_version":"1.12.0","files":{},"packages":{}}"#,
        ).unwrap();

        // Create legacy files: flat + old profiled + tether config
        let dotfiles_dir = sync_path.join("dotfiles");
        std::fs::create_dir_all(dotfiles_dir.join("dev")).unwrap();
        std::fs::create_dir_all(dotfiles_dir.join("tether")).unwrap();
        std::fs::write(dotfiles_dir.join("zshrc.enc"), "flat").unwrap();
        std::fs::write(dotfiles_dir.join("dev/zshrc.enc"), "old-profiled").unwrap();
        std::fs::write(dotfiles_dir.join("tether/config.toml.enc"), "config").unwrap();

        let cleaned = cleanup_legacy_dotfiles(sync_path).unwrap();
        assert!(cleaned);

        // Entire dotfiles/ dir removed
        assert!(!dotfiles_dir.exists());
        // Tether config migrated to configs/tether/
        assert_eq!(
            std::fs::read_to_string(sync_path.join("configs/tether/config.toml.enc")).unwrap(),
            "config"
        );
    }

    #[test]
    fn test_cleanup_legacy_skips_tether_config_migration_if_already_exists() {
        let tmp = TempDir::new().unwrap();
        let sync_path = tmp.path();

        let machines_dir = sync_path.join("machines");
        std::fs::create_dir_all(&machines_dir).unwrap();
        std::fs::write(
            machines_dir.join("mac.json"),
            r#"{"machine_id":"mac","hostname":"h","last_sync":"2026-01-01T00:00:00Z","cli_version":"1.11.0","files":{},"packages":{}}"#,
        ).unwrap();

        // Legacy and new config both exist — new should win
        let dotfiles_dir = sync_path.join("dotfiles/tether");
        std::fs::create_dir_all(&dotfiles_dir).unwrap();
        std::fs::write(dotfiles_dir.join("config.toml.enc"), "old").unwrap();

        let new_dir = sync_path.join("configs/tether");
        std::fs::create_dir_all(&new_dir).unwrap();
        std::fs::write(new_dir.join("config.toml.enc"), "new").unwrap();

        cleanup_legacy_dotfiles(sync_path).unwrap();

        assert_eq!(
            std::fs::read_to_string(sync_path.join("configs/tether/config.toml.enc")).unwrap(),
            "new"
        );
    }
}

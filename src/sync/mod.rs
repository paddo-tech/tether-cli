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
pub use git::{checkout_id_from_path, extract_org_from_normalized_url, GitBackend};
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
}

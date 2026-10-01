//! Read-side queries against the sync repo used by the dashboard.

use super::state::DashboardState;
use crate::sync::{FileLogEntry, GitBackend, SyncEngine};
use std::collections::{HashMap, HashSet};

fn open_repo() -> Option<GitBackend> {
    SyncEngine::sync_path()
        .ok()
        .and_then(|p| GitBackend::open(&p).ok())
}

/// Last 10 commits that changed a repo file.
pub fn file_history(repo_path: &str, encrypted: bool) -> Vec<FileLogEntry> {
    open_repo()
        .and_then(|git| git.file_log_changed(repo_path, 10, encrypted).ok())
        .unwrap_or_default()
}

/// Diff of a dotfile at a commit, one entry per line.
pub fn file_diff(commit: &str, repo_path: &str, dotfile: &str, encrypted: bool) -> Vec<String> {
    open_repo()
        .and_then(|git| git.file_diff(commit, repo_path, dotfile, encrypted).ok())
        .unwrap_or_default()
        .lines()
        .map(|l| l.to_string())
        .collect()
}

/// Load manifest commit history for a package manager.
pub fn pkg_history(manager_key: &str) -> Vec<FileLogEntry> {
    let Some(manifest) = crate::sync::packages::manifest_filename(manager_key) else {
        return Vec::new();
    };
    file_history(&format!("manifests/{}", manifest), false)
}

/// Load the manifest diff for a manager at a commit.
pub fn pkg_diff(manager_key: &str, commit: &str) -> Vec<String> {
    let Some(manifest) = crate::sync::packages::manifest_filename(manager_key) else {
        return Vec::new();
    };
    let repo_path = format!("manifests/{}", manifest);
    open_repo()
        .and_then(|git| git.file_diff(commit, &repo_path, &repo_path, false).ok())
        .map(|d| d.lines().map(str::to_string).collect())
        .unwrap_or_default()
}

/// Install/uninstall counts for rolling a manager back to a commit.
pub struct RollbackPlan {
    pub manager: String,
    pub commit: String,
    pub short_hash: String,
    pub install: usize,
    pub uninstall: usize,
}

pub fn rollback_plan(
    state: &DashboardState,
    manager: &str,
    commit: &str,
    short_hash: &str,
) -> Option<RollbackPlan> {
    let manifest = crate::sync::packages::manifest_filename(manager)?;
    let repo_path = format!("manifests/{}", manifest);
    let snapshot = open_repo()?.show_at_commit(commit, &repo_path).ok()?;
    let snapshot = String::from_utf8_lossy(&snapshot);
    let ecosystem = crate::packages::manager_for_key(manager).map(|m| m.ecosystem());
    let target: HashSet<String> = snapshot
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(|l| match ecosystem {
            Some(eco) => crate::packages::pin::parse_pin(eco, l).0,
            None => l.to_string(),
        })
        .collect();

    let current_machine_id = state
        .sync_state
        .as_ref()
        .map(|s| s.machine_id.as_str())
        .unwrap_or("");
    let installed: HashSet<String> = state
        .machines
        .iter()
        .find(|m| m.machine_id == current_machine_id)
        .and_then(|m| m.packages.get(manager))
        .map(|v| v.iter().cloned().collect())
        .unwrap_or_default();

    Some(RollbackPlan {
        manager: manager.to_string(),
        commit: commit.to_string(),
        short_hash: short_hash.to_string(),
        install: target.difference(&installed).count(),
        uninstall: installed.difference(&target).count(),
    })
}

/// Detect files in the sync repo that are no longer tracked locally.
/// Compares repo dotfiles/ against SyncState.files to find deletions.
pub fn load_deleted_files(state: &DashboardState) -> HashMap<String, Vec<String>> {
    let mut deleted: HashMap<String, Vec<String>> = HashMap::new();

    let sync_path = match SyncEngine::sync_path() {
        Ok(p) => p,
        Err(_) => return deleted,
    };
    let git = match GitBackend::open(&sync_path) {
        Ok(g) => g,
        Err(_) => return deleted,
    };

    let mut tracked = git.list_tracked_files("profiles/").unwrap_or_default();
    tracked.extend(git.list_tracked_files("dotfiles/").unwrap_or_default());
    tracked.extend(git.list_tracked_files("configs/").unwrap_or_default());

    let ss = match &state.sync_state {
        Some(s) => s,
        None => return deleted,
    };

    let encrypted = state
        .config
        .as_ref()
        .map(|c| c.security.encrypt_dotfiles)
        .unwrap_or(false);

    // Build set of repo paths from current state
    let machine_id = ss.machine_id.as_str();
    let sync_path_opt = SyncEngine::sync_path().ok();
    let config_ref = state.config.as_ref();
    let profile = config_ref
        .map(|c| c.profile_name(machine_id))
        .unwrap_or(crate::config::DEFAULT_PROFILE);

    let mut state_repo_paths: HashSet<String> = HashSet::new();
    for path in ss.files.keys() {
        if path.starts_with("project:")
            || path.starts_with("team-secret:")
            || path.starts_with("collab-secret:")
            || path.starts_with(".tether/")
        {
            continue;
        }
        let repo_path = if let Some(rel) = path.strip_prefix("~/") {
            if encrypted {
                format!("configs/{}.enc", rel)
            } else {
                format!("configs/{}", rel)
            }
        } else {
            let shared = config_ref
                .map(|c| c.is_dotfile_shared(machine_id, path))
                .unwrap_or(false);
            if let Some(ref sp) = sync_path_opt {
                crate::sync::resolve_dotfile_repo_path(sp, path, encrypted, profile, shared)
            } else {
                crate::sync::dotfile_to_repo_path(path, encrypted)
            }
        };
        state_repo_paths.insert(repo_path);
    }

    for repo_file in &tracked {
        if state_repo_paths.contains(repo_file.as_str()) {
            continue;
        }
        // Reverse map: repo path -> display path
        let display = repo_path_to_dotfile(repo_file, encrypted, state.config.as_ref());
        deleted
            .entry("Personal".to_string())
            .or_default()
            .push(display);
    }

    // Sort each section's deleted files
    for files in deleted.values_mut() {
        files.sort();
    }

    deleted
}

/// Reverse of dotfile_to_repo_path: "dotfiles/zshrc.enc" -> ".zshrc"
/// Also handles profiled paths: "profiles/dev/zshrc.enc" -> ".zshrc"
/// Uses known profile names from config to distinguish profile dirs from dotfile subdirs.
pub fn repo_path_to_dotfile(
    repo_path: &str,
    encrypted: bool,
    config: Option<&crate::Config>,
) -> String {
    let mut names = HashSet::new();
    names.insert("shared".to_string());
    if let Some(config) = config {
        for name in config.profiles.keys() {
            names.insert(name.clone());
        }
    }
    repo_path_to_dotfile_with_profiles(repo_path, encrypted, &names)
}

fn repo_path_to_dotfile_with_profiles(
    repo_path: &str,
    encrypted: bool,
    profile_names: &HashSet<String>,
) -> String {
    if let Some(rest) = repo_path.strip_prefix("configs/") {
        let name = if encrypted {
            rest.strip_suffix(".enc").unwrap_or(rest)
        } else {
            rest
        };
        return format!("~/{}", name);
    }
    let name = repo_path
        .strip_prefix("profiles/")
        .or_else(|| repo_path.strip_prefix("dotfiles/"))
        .unwrap_or(repo_path);
    // Strip profile/shared prefix if present, but only if first component is a known profile
    let name = if let Some((prefix, rest)) = name.split_once('/') {
        if profile_names.contains(prefix) {
            rest
        } else {
            name
        }
    } else {
        name
    };
    if encrypted {
        let name = name.strip_suffix(".enc").unwrap_or(name);
        format!(".{}", name)
    } else if name.starts_with('.') {
        name.to_string()
    } else {
        format!(".{}", name)
    }
}

/// Sync commits per local day, oldest first, ending today. Empty without a sync repo.
pub fn commit_activity(days: usize) -> Vec<u64> {
    let Ok(sync_path) = SyncEngine::sync_path() else {
        return Vec::new();
    };
    let since = format!("--since={}.days", days);
    let Ok(output) = std::process::Command::new("git")
        .args(["log", &since, "--format=%at"])
        .current_dir(&sync_path)
        .output()
    else {
        return Vec::new();
    };
    if !output.status.success() {
        return Vec::new();
    }
    let stamps: Vec<i64> = String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter_map(|l| l.trim().parse().ok())
        .collect();
    daily_counts(&stamps, chrono::Local::now().date_naive(), days)
}

/// Bucket unix timestamps into `days` local-day counts ending on `today`.
pub fn daily_counts(stamps: &[i64], today: chrono::NaiveDate, days: usize) -> Vec<u64> {
    let mut counts = vec![0u64; days];
    for &ts in stamps {
        let Some(dt) = chrono::DateTime::from_timestamp(ts, 0) else {
            continue;
        };
        let date = dt.with_timezone(&chrono::Local).date_naive();
        let ago = (today - date).num_days();
        if (0..days as i64).contains(&ago) {
            counts[days - 1 - ago as usize] += 1;
        }
    }
    counts
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    #[test]
    fn daily_counts_bucket_by_local_day() {
        let today = chrono::NaiveDate::from_ymd_opt(2026, 10, 2).unwrap();
        let at = |d: u32, h: u32| {
            chrono::Local
                .with_ymd_and_hms(2026, 10, d, h, 0, 0)
                .unwrap()
                .timestamp()
        };
        let stamps = [at(2, 9), at(2, 23), at(1, 0), at(1, 1), at(1, 2)];
        assert_eq!(daily_counts(&stamps, today, 3), vec![0, 3, 2]);
        assert_eq!(
            daily_counts(&[at(2, 9) + 86_400 * 5], today, 3),
            vec![0, 0, 0]
        );
    }
}

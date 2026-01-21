use anyhow::Result;
use git2::{Repository, Signature};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

pub struct GitBackend {
    repo_path: PathBuf,
}

impl GitBackend {
    pub fn new(repo_path: PathBuf) -> Self {
        Self { repo_path }
    }

    /// Check if the repository has any commits
    fn has_commits(&self) -> bool {
        let output = Command::new("git")
            .args(["rev-parse", "HEAD"])
            .current_dir(&self.repo_path)
            .output();

        match output {
            Ok(out) => out.status.success(),
            Err(_) => false,
        }
    }

    /// Check if remote branch exists
    fn remote_branch_exists(&self, branch: &str) -> bool {
        let output = Command::new("git")
            .args(["ls-remote", "--heads", "origin", branch])
            .current_dir(&self.repo_path)
            .stdin(Stdio::inherit())
            .output();

        match output {
            Ok(out) => out.status.success() && !out.stdout.is_empty(),
            Err(_) => false,
        }
    }

    pub fn clone(url: &str, path: &Path) -> Result<Self> {
        // Use git CLI for cloning - it handles gh authentication automatically
        let path_str = path
            .to_str()
            .ok_or_else(|| anyhow::anyhow!("Path contains invalid UTF-8"))?;
        let output = Command::new("git")
            .args(["clone", url, path_str])
            .stdin(Stdio::inherit())
            .output()?;

        if !output.status.success() {
            let error = String::from_utf8_lossy(&output.stderr);
            return Err(anyhow::anyhow!("Failed to clone repository: {}", error));
        }

        Ok(Self {
            repo_path: path.to_path_buf(),
        })
    }

    pub fn open(path: &Path) -> Result<Self> {
        Repository::open(path)?;
        Ok(Self {
            repo_path: path.to_path_buf(),
        })
    }

    pub fn commit(&self, message: &str, machine_id: &str) -> Result<()> {
        let repo = Repository::open(&self.repo_path)?;
        let mut index = repo.index()?;
        index.add_all(["*"].iter(), git2::IndexAddOption::DEFAULT, None)?;
        index.write()?;

        let oid = index.write_tree()?;
        let tree = repo.find_tree(oid)?;

        let sig = Signature::now(machine_id, "tether@local")?;

        // Check if this is the first commit
        if self.has_commits() {
            let parent = repo.head()?.peel_to_commit()?;
            repo.commit(Some("HEAD"), &sig, &sig, message, &tree, &[&parent])?;
        } else {
            // Initial commit (no parent)
            repo.commit(Some("HEAD"), &sig, &sig, message, &tree, &[])?;
        }

        Ok(())
    }

    /// Check if a rebase is currently in progress
    fn is_rebase_in_progress(&self) -> bool {
        self.repo_path.join(".git/rebase-merge").exists()
            || self.repo_path.join(".git/rebase-apply").exists()
    }

    /// Abort any in-progress rebase
    fn abort_rebase(&self) -> Result<()> {
        Command::new("git")
            .args(["rebase", "--abort"])
            .current_dir(&self.repo_path)
            .output()?;
        Ok(())
    }

    /// Reset local branch to match remote
    fn reset_to_remote(&self) -> Result<()> {
        let output = Command::new("git")
            .args(["reset", "--hard", "origin/main"])
            .current_dir(&self.repo_path)
            .output()?;

        if !output.status.success() {
            let error = String::from_utf8_lossy(&output.stderr);
            return Err(anyhow::anyhow!("Failed to reset: {}", error));
        }
        Ok(())
    }

    pub fn pull(&self) -> Result<()> {
        // Abort any stale rebase from a previous interrupted sync
        if self.is_rebase_in_progress() {
            self.abort_rebase()?;
        }

        // Skip pull if remote branch doesn't exist (empty repository)
        if !self.remote_branch_exists("main") {
            return Ok(());
        }

        // Fetch first, then rebase explicitly onto origin/main
        // This avoids "Cannot rebase onto multiple branches" errors
        let fetch_output = Command::new("git")
            .args(["fetch", "origin", "main"])
            .current_dir(&self.repo_path)
            .stdin(Stdio::inherit())
            .output()?;

        if !fetch_output.status.success() {
            let error = String::from_utf8_lossy(&fetch_output.stderr);
            return Err(anyhow::anyhow!("Failed to fetch changes: {}", error));
        }

        let rebase_output = Command::new("git")
            .args(["rebase", "origin/main"])
            .current_dir(&self.repo_path)
            .output()?;

        if !rebase_output.status.success() {
            // Conflict - abort and reset to remote
            // Safe because sync will re-export local state afterward
            self.abort_rebase()?;
            self.reset_to_remote()?;
        }

        Ok(())
    }

    pub fn push(&self) -> Result<()> {
        let args = if self.remote_branch_exists("main") {
            vec!["push", "origin", "main"]
        } else {
            vec!["push", "-u", "origin", "main"]
        };

        for attempt in 1..=3 {
            let output = Command::new("git")
                .args(&args)
                .current_dir(&self.repo_path)
                .stdin(Stdio::inherit())
                .output()?;

            if output.status.success() {
                return Ok(());
            }

            let error = String::from_utf8_lossy(&output.stderr);

            // Retry on rejection due to remote changes
            let is_rejection = error.contains("fetch first") || error.contains("non-fast-forward");
            if is_rejection && attempt < 3 {
                self.pull()?;
                continue;
            }

            return Err(anyhow::anyhow!("Failed to push: {}", error));
        }

        Ok(())
    }

    pub fn sync_path(&self) -> &Path {
        &self.repo_path
    }

    /// Check if the current user has write access to the remote repository
    pub fn has_write_access(&self) -> Result<bool> {
        // Try a dry-run push to check write permissions
        let output = Command::new("git")
            .args(["push", "--dry-run", "origin", "HEAD"])
            .current_dir(&self.repo_path)
            .stdin(Stdio::inherit())
            .output()?;

        // If dry-run succeeds or gives specific errors, we have write access
        // If we get "permission denied" or "403", we don't have write access
        if output.status.success() {
            return Ok(true);
        }

        let stderr = String::from_utf8_lossy(&output.stderr).to_lowercase();

        // Check for permission denied errors
        if stderr.contains("permission denied")
            || stderr.contains("403")
            || stderr.contains("forbidden")
            || stderr.contains("not permitted")
            || stderr.contains("access denied")
        {
            return Ok(false);
        }

        // If we get here, assume we have write access
        // (other errors might be network issues, etc.)
        Ok(true)
    }

    /// Check if there are uncommitted changes in the repository
    pub fn has_changes(&self) -> Result<bool> {
        let output = Command::new("git")
            .args(["status", "--porcelain"])
            .current_dir(&self.repo_path)
            .output()?;

        Ok(!output.stdout.is_empty())
    }
}

/// Git utility functions for project config syncing
///
/// Get the git remote URL for a repository
pub fn get_remote_url(repo_path: &Path) -> Result<String> {
    let path_str = repo_path
        .to_str()
        .ok_or_else(|| anyhow::anyhow!("Path contains invalid UTF-8"))?;
    let output = Command::new("git")
        .args(["-C", path_str, "config", "--get", "remote.origin.url"])
        .output()?;

    if !output.status.success() {
        return Err(anyhow::anyhow!(
            "Failed to get remote URL (not a git repo or no remote?)"
        ));
    }

    let url = String::from_utf8(output.stdout)?.trim().to_string();
    Ok(url)
}

/// Normalize a git remote URL to a canonical form
/// Examples:
/// - git@github.com:user/repo.git -> github.com/user/repo
/// - https://github.com/user/repo.git -> github.com/user/repo
/// - https://github.com/user/repo -> github.com/user/repo
pub fn normalize_remote_url(url: &str) -> String {
    let mut normalized = url.to_string();

    // Remove .git suffix
    if let Some(stripped) = normalized.strip_suffix(".git") {
        normalized = stripped.to_string();
    }

    // Convert SSH format (git@host:path) to URL format (host/path)
    if let Some(rest) = normalized.strip_prefix("git@") {
        // git@github.com:user/repo -> github.com/user/repo
        normalized = rest.replace(':', "/");
    } else if let Some(rest) = normalized.strip_prefix("https://") {
        // https://github.com/user/repo -> github.com/user/repo
        normalized = rest.to_string();
    } else if let Some(rest) = normalized.strip_prefix("http://") {
        // http://github.com/user/repo -> github.com/user/repo
        normalized = rest.to_string();
    }

    normalized
}

/// Extract the org portion from a normalized URL
/// Examples:
/// - github.com/acme-corp/repo -> github.com/acme-corp
/// - gitlab.com/group/subgroup/repo -> gitlab.com/group (first level only)
pub fn extract_org_from_normalized_url(normalized_url: &str) -> Option<String> {
    let parts: Vec<&str> = normalized_url.split('/').collect();
    if parts.len() >= 2 {
        // host/org (e.g., github.com/acme-corp), normalized to lowercase
        Some(format!("{}/{}", parts[0], parts[1]).to_lowercase())
    } else {
        None
    }
}

/// Generate a short checkout ID from a path (first 8 chars of SHA256 of canonical path)
pub fn checkout_id_from_path(path: &Path) -> String {
    use sha2::{Digest, Sha256};
    let canonical = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    let hash = Sha256::digest(canonical.to_string_lossy().as_bytes());
    format!("{:x}", hash)[..8].to_string()
}

/// Check if a file is gitignored in its repository
pub fn is_gitignored(file_path: &Path) -> Result<bool> {
    // Get the directory containing the file
    let dir = file_path
        .parent()
        .ok_or_else(|| anyhow::anyhow!("Invalid file path"))?;

    let dir_str = dir
        .to_str()
        .ok_or_else(|| anyhow::anyhow!("Directory path contains invalid UTF-8"))?;
    let file_str = file_path
        .to_str()
        .ok_or_else(|| anyhow::anyhow!("File path contains invalid UTF-8"))?;

    let output = Command::new("git")
        .args(["-C", dir_str, "check-ignore", file_str])
        .output()?;

    // git check-ignore returns 0 if the file is ignored, 1 if not
    Ok(output.status.success())
}

/// Find all git repositories under a given path (recursive, max 3 levels deep)
/// Directories to skip when scanning for git repos or project files.
/// These are typically build artifacts, dependencies, or caches.
pub fn should_skip_dir(name: &str) -> bool {
    // Hidden directories
    if name.starts_with('.') {
        return true;
    }

    matches!(
        name,
        // Node.js
        "node_modules"
            | "bower_components"
            // Rust
            | "target"
            // Python
            | "__pycache__"
            | ".venv"
            | "venv"
            | "env"
            | ".eggs"
            | "*.egg-info"
            // .NET
            | "bin"
            | "obj"
            | "packages"
            // Java/Kotlin
            | "build"
            | "out"
            // Go
            | "vendor"
            // Ruby
            | "bundle"
            // General
            | "dist"
            | "coverage"
            | "tmp"
            | "temp"
            | "cache"
            | ".cache"
    )
}

pub fn find_git_repos(search_path: &Path) -> Result<Vec<PathBuf>> {
    let mut repos = Vec::new();

    if !search_path.exists() {
        return Ok(repos);
    }

    find_git_repos_recursive(search_path, &mut repos, 0, 3)?;
    Ok(repos)
}

fn find_git_repos_recursive(
    path: &Path,
    repos: &mut Vec<PathBuf>,
    depth: usize,
    max_depth: usize,
) -> Result<()> {
    if depth > max_depth {
        return Ok(());
    }

    // If this directory is a git repo, add it and don't recurse into it
    if path.join(".git").exists() {
        repos.push(path.to_path_buf());
        return Ok(());
    }

    // Otherwise, recurse into subdirectories
    if let Ok(entries) = std::fs::read_dir(path) {
        for entry in entries.flatten() {
            let entry_path = entry.path();
            if entry_path.is_dir() {
                if let Some(name) = entry_path.file_name().and_then(|n| n.to_str()) {
                    if should_skip_dir(name) {
                        continue;
                    }
                }
                find_git_repos_recursive(&entry_path, repos, depth + 1, max_depth)?;
            }
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    // URL normalization tests
    #[test]
    fn test_normalize_ssh_url() {
        assert_eq!(
            normalize_remote_url("git@github.com:user/repo.git"),
            "github.com/user/repo"
        );
    }

    #[test]
    fn test_normalize_ssh_url_no_git_suffix() {
        assert_eq!(
            normalize_remote_url("git@github.com:user/repo"),
            "github.com/user/repo"
        );
    }

    #[test]
    fn test_normalize_https_url() {
        assert_eq!(
            normalize_remote_url("https://github.com/user/repo.git"),
            "github.com/user/repo"
        );
    }

    #[test]
    fn test_normalize_https_url_no_git_suffix() {
        assert_eq!(
            normalize_remote_url("https://github.com/user/repo"),
            "github.com/user/repo"
        );
    }

    #[test]
    fn test_normalize_http_url() {
        assert_eq!(
            normalize_remote_url("http://github.com/user/repo"),
            "github.com/user/repo"
        );
    }

    #[test]
    fn test_normalize_gitlab_url() {
        assert_eq!(
            normalize_remote_url("git@gitlab.com:group/subgroup/repo.git"),
            "gitlab.com/group/subgroup/repo"
        );
    }

    #[test]
    fn test_extract_org_github() {
        assert_eq!(
            extract_org_from_normalized_url("github.com/acme-corp/repo"),
            Some("github.com/acme-corp".to_string())
        );
    }

    #[test]
    fn test_extract_org_gitlab() {
        assert_eq!(
            extract_org_from_normalized_url("gitlab.com/group/subgroup/repo"),
            Some("gitlab.com/group".to_string())
        );
    }

    #[test]
    fn test_extract_org_invalid() {
        assert_eq!(extract_org_from_normalized_url("github.com"), None);
        assert_eq!(extract_org_from_normalized_url(""), None);
    }

    #[test]
    fn test_extract_org_case_normalization() {
        assert_eq!(
            extract_org_from_normalized_url("GitHub.com/ACME-Corp/Repo"),
            Some("github.com/acme-corp".to_string())
        );
    }

    // Skip directory tests
    #[test]
    fn test_should_skip_hidden_dirs() {
        assert!(should_skip_dir(".git"));
        assert!(should_skip_dir(".cache"));
        assert!(should_skip_dir(".hidden"));
    }

    #[test]
    fn test_should_skip_node_modules() {
        assert!(should_skip_dir("node_modules"));
        assert!(should_skip_dir("bower_components"));
    }

    #[test]
    fn test_should_skip_build_dirs() {
        assert!(should_skip_dir("target"));
        assert!(should_skip_dir("build"));
        assert!(should_skip_dir("dist"));
        assert!(should_skip_dir("out"));
    }

    #[test]
    fn test_should_skip_python_dirs() {
        assert!(should_skip_dir("__pycache__"));
        assert!(should_skip_dir("venv"));
        assert!(should_skip_dir(".venv"));
    }

    #[test]
    fn test_should_not_skip_src() {
        assert!(!should_skip_dir("src"));
        assert!(!should_skip_dir("lib"));
        assert!(!should_skip_dir("app"));
        assert!(!should_skip_dir("components"));
    }

    #[test]
    fn test_checkout_id_from_path() {
        use std::path::Path;

        // Same path should give same ID
        let id1 = checkout_id_from_path(Path::new("/tmp/test/repo"));
        let id2 = checkout_id_from_path(Path::new("/tmp/test/repo"));
        assert_eq!(id1, id2);

        // Different paths should give different IDs
        let id3 = checkout_id_from_path(Path::new("/tmp/other/repo"));
        assert_ne!(id1, id3);

        // ID should be 8 characters
        assert_eq!(id1.len(), 8);
    }
}

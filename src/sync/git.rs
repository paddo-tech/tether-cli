use anyhow::Result;
use git2::{Repository, Signature};
use std::path::{Path, PathBuf};
use std::process::Command;

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
            .output();

        match output {
            Ok(out) => out.status.success() && !out.stdout.is_empty(),
            Err(_) => false,
        }
    }

    pub fn clone(url: &str, path: &Path) -> Result<Self> {
        // Use git CLI for cloning - it handles gh authentication automatically
        let output = Command::new("git")
            .args(["clone", url, path.to_str().unwrap()])
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

    pub fn pull(&self) -> Result<()> {
        // Skip pull if remote branch doesn't exist (empty repository)
        if !self.remote_branch_exists("main") {
            return Ok(());
        }

        // Use git CLI for pulling - it handles gh authentication automatically
        let output = Command::new("git")
            .args(["pull", "origin", "main"])
            .current_dir(&self.repo_path)
            .output()?;

        if !output.status.success() {
            let error = String::from_utf8_lossy(&output.stderr);
            return Err(anyhow::anyhow!("Failed to pull changes: {}", error));
        }

        Ok(())
    }

    pub fn push(&self) -> Result<()> {
        // Use git CLI for pushing - it handles gh authentication automatically
        // Use -u flag to set upstream tracking on first push
        let args = if self.remote_branch_exists("main") {
            vec!["push", "origin", "main"]
        } else {
            vec!["push", "-u", "origin", "main"]
        };

        let output = Command::new("git")
            .args(&args)
            .current_dir(&self.repo_path)
            .output()?;

        if !output.status.success() {
            let error = String::from_utf8_lossy(&output.stderr);
            return Err(anyhow::anyhow!("Failed to push changes: {}", error));
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
    let output = Command::new("git")
        .args([
            "-C",
            repo_path.to_str().unwrap(),
            "config",
            "--get",
            "remote.origin.url",
        ])
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
    if normalized.ends_with(".git") {
        normalized = normalized[..normalized.len() - 4].to_string();
    }

    // Convert SSH format (git@host:path) to URL format (host/path)
    if normalized.starts_with("git@") {
        // git@github.com:user/repo -> github.com/user/repo
        normalized = normalized.strip_prefix("git@").unwrap().replace(':', "/");
    } else if normalized.starts_with("https://") {
        // https://github.com/user/repo -> github.com/user/repo
        normalized = normalized.strip_prefix("https://").unwrap().to_string();
    } else if normalized.starts_with("http://") {
        // http://github.com/user/repo -> github.com/user/repo
        normalized = normalized.strip_prefix("http://").unwrap().to_string();
    }

    normalized
}

/// Check if a file is gitignored in its repository
pub fn is_gitignored(file_path: &Path) -> Result<bool> {
    // Get the directory containing the file
    let dir = file_path
        .parent()
        .ok_or_else(|| anyhow::anyhow!("Invalid file path"))?;

    let output = Command::new("git")
        .args([
            "-C",
            dir.to_str().unwrap(),
            "check-ignore",
            file_path.to_str().unwrap(),
        ])
        .output()?;

    // git check-ignore returns 0 if the file is ignored, 1 if not
    Ok(output.status.success())
}

/// Find all git repositories under a given path (non-recursive, one level deep)
pub fn find_git_repos(search_path: &Path) -> Result<Vec<PathBuf>> {
    let mut repos = Vec::new();

    if !search_path.exists() {
        return Ok(repos);
    }

    // Check if search_path itself is a git repo
    if search_path.join(".git").exists() {
        repos.push(search_path.to_path_buf());
        return Ok(repos);
    }

    // Otherwise, search immediate subdirectories
    for entry in std::fs::read_dir(search_path)? {
        let entry = entry?;
        let path = entry.path();

        if path.is_dir() && path.join(".git").exists() {
            repos.push(path);
        }
    }

    Ok(repos)
}

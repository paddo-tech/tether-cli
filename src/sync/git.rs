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
}

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
        let parent = repo.head()?.peel_to_commit()?;

        repo.commit(Some("HEAD"), &sig, &sig, message, &tree, &[&parent])?;

        Ok(())
    }

    pub fn pull(&self) -> Result<()> {
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
        let output = Command::new("git")
            .args(["push", "origin", "main"])
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

use anyhow::Result;
use chrono::{DateTime, Utc};
use git2::{Repository, Signature};
use std::collections::hash_map::RandomState;
use std::hash::{BuildHasher, Hasher};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// Config for every git command Tether runs. Rebase and stash write commits with the
/// committer from git config, and a new machine may have none, so Tether gives the
/// identity its git2 commits use. Rebased commits stay unsigned, so a user's
/// `commit.gpgsign` cannot ask for a passphrase or fail a run without a terminal.
pub fn identity_args() -> [String; 8] {
    [
        "-c".to_string(),
        format!("user.name={}", crate::sync::local_hostname()),
        "-c".to_string(),
        "user.email=tether@local".to_string(),
        "-c".to_string(),
        "commit.gpgsign=false".to_string(),
        // A recorded resolution would stage a conflict yet still stop the rebase
        "-c".to_string(),
        "rerere.enabled=false".to_string(),
    ]
}

pub fn git_command() -> Command {
    let mut cmd = Command::new("git");
    cmd.args(identity_args());
    cmd
}

fn write_signed(
    repo: &Repository,
    key: &ssh_key::PrivateKey,
    author: &Signature,
    committer: &Signature,
    message: &str,
    tree: &git2::Tree,
    parents: &[&git2::Commit],
) -> Result<git2::Oid> {
    let buffer = repo.commit_create_buffer(author, committer, message, tree, parents)?;
    let content = buffer
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("Commit is not valid UTF-8"))?;
    let signature = crate::sync::signing::sign_commit(key, content.as_bytes())?;
    Ok(repo.commit_signed(content, &signature, None)?)
}

/// Point the branch HEAD names (or a detached HEAD) at `oid`.
fn set_head(repo: &Repository, oid: git2::Oid, message: &str) -> Result<()> {
    let head = repo.find_reference("HEAD")?;
    let name = head.symbolic_target().unwrap_or("HEAD").to_string();
    repo.reference(&name, oid, true, message)?;
    Ok(())
}

pub struct GitBackend {
    repo_path: PathBuf,
}

impl GitBackend {
    pub fn new(repo_path: PathBuf) -> Self {
        Self { repo_path }
    }

    /// Check if the repository has any commits
    fn has_commits(&self) -> bool {
        let output = git_command()
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
        let output = git_command()
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
        let output = git_command()
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

    /// This machine's signing key when this is the personal sync repo. Commit signatures
    /// there are an audit trail only, so team and collab commits stay unsigned.
    fn signing_key(&self) -> Result<Option<ssh_key::PrivateKey>> {
        if self.repo_path != crate::sync::SyncEngine::sync_path()? {
            return Ok(None);
        }
        let machine_id = crate::sync::SyncState::load()?.machine_id;
        Ok(Some(crate::sync::signing::load_or_create(&machine_id)?))
    }

    pub fn commit(&self, message: &str, author: &str) -> Result<()> {
        self.commit_with_key(message, author, self.signing_key()?.as_ref())
    }

    pub(crate) fn commit_with_key(
        &self,
        message: &str,
        author: &str,
        key: Option<&ssh_key::PrivateKey>,
    ) -> Result<()> {
        let repo = Repository::open(&self.repo_path)?;
        let mut index = repo.index()?;
        index.add_all(["*"].iter(), git2::IndexAddOption::DEFAULT, None)?;
        index.write()?;

        let oid = index.write_tree()?;
        let tree = repo.find_tree(oid)?;

        let sig = Signature::now(author, "tether@local")?;

        // Check if this is the first commit
        let parent = if self.has_commits() {
            let parent = repo.head()?.peel_to_commit()?;
            // Skip empty commits (tree unchanged from parent)
            if parent.tree()?.id() == oid {
                return Ok(());
            }
            Some(parent)
        } else {
            None
        };
        let parents: Vec<&git2::Commit> = parent.iter().collect();

        match key {
            Some(key) => {
                let commit = write_signed(&repo, key, &sig, &sig, message, &tree, &parents)?;
                set_head(&repo, commit, message)?;
            }
            None => {
                repo.commit(Some("HEAD"), &sig, &sig, message, &tree, &parents)?;
            }
        }

        Ok(())
    }

    /// Delete the tracked files at `paths` (relative to the repo) and commit the deletion.
    /// When any step fails, the files come back from HEAD, so a retry starts from the
    /// same tree.
    pub fn remove_and_commit(&self, paths: &[String], message: &str, author: &str) -> Result<()> {
        let removed = (|| {
            let repo = Repository::open(&self.repo_path)?;
            let mut index = repo.index()?;
            for path in paths {
                index.remove_path(Path::new(path))?;
            }
            index.write()?;
            for path in paths {
                std::fs::remove_file(self.repo_path.join(path))?;
            }
            self.commit(message, author)
        })();
        let Err(e) = removed else {
            return Ok(());
        };
        if let Err(restore) = self.restore_from_head(paths) {
            anyhow::bail!(
                "{}. Restoring {} from HEAD also failed: {}",
                e,
                paths.join(", "),
                restore
            );
        }
        Err(e)
    }

    /// Write `paths` back from HEAD's tree. It does not touch the index, so a stale
    /// index lock cannot stop it. Each path is removed and created again, never written
    /// through: a committed symlink such as `machines/old.json -> ../../state.json` comes
    /// back as that symlink, and Tether's own files outside the repo stay untouched.
    fn restore_from_head(&self, paths: &[String]) -> Result<()> {
        use std::io::Write;
        use std::os::unix::ffi::OsStrExt;
        use std::os::unix::fs::OpenOptionsExt;

        let repo = Repository::open(&self.repo_path)?;
        let tree = repo.head()?.peel_to_tree()?;
        let root = self.repo_path.canonicalize()?;
        for path in paths {
            let entry = tree.get_path(Path::new(path))?;
            let blob = entry.to_object(&repo)?.peel_to_blob()?;
            let target = self.repo_path.join(path);
            let parent = target
                .parent()
                .ok_or_else(|| anyhow::anyhow!("{} has no parent directory", path))?;
            if !parent.canonicalize()?.starts_with(&root) {
                anyhow::bail!("{} resolves outside the repository", path);
            }
            match std::fs::symlink_metadata(&target) {
                Ok(_) => std::fs::remove_file(&target)?,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(e.into()),
            }
            let mode = entry.filemode();
            if mode == i32::from(git2::FileMode::Link) {
                std::os::unix::fs::symlink(std::ffi::OsStr::from_bytes(blob.content()), &target)?;
            } else {
                let executable = mode == i32::from(git2::FileMode::BlobExecutable);
                std::fs::OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .mode(if executable { 0o755 } else { 0o644 })
                    .open(&target)?
                    .write_all(blob.content())?;
            }
        }
        Ok(())
    }

    fn git(&self, args: &[&str]) -> Result<()> {
        let output = git_command()
            .args(args)
            .current_dir(&self.repo_path)
            .output()?;
        if !output.status.success() {
            let error = String::from_utf8_lossy(&output.stderr);
            return Err(anyhow::anyhow!("git {} failed: {}", args.join(" "), error));
        }
        Ok(())
    }

    /// Check if a rebase is currently in progress
    fn is_rebase_in_progress(&self) -> bool {
        self.repo_path.join(".git/rebase-merge").exists()
            || self.repo_path.join(".git/rebase-apply").exists()
    }

    /// True when the index has unmerged paths, as a stopped rebase leaves on a conflict
    fn has_conflicts(&self) -> bool {
        Repository::open(&self.repo_path)
            .and_then(|repo| repo.index())
            .is_ok_and(|index| index.has_conflicts())
    }

    /// Abort any in-progress rebase
    fn abort_rebase(&self) -> Result<()> {
        git_command()
            .args(["rebase", "--abort"])
            .current_dir(&self.repo_path)
            .output()?;
        Ok(())
    }

    /// Fetch origin/main without changing the local branch.
    pub fn fetch(&self) -> Result<()> {
        let output = git_command()
            .args(["fetch", "origin", "main"])
            .current_dir(&self.repo_path)
            .stdin(Stdio::inherit())
            .output()?;
        if !output.status.success() {
            let error = String::from_utf8_lossy(&output.stderr);
            return Err(anyhow::anyhow!("Failed to fetch changes: {}", error));
        }
        Ok(())
    }

    /// Push main once. A rejected push fails and leaves the local commits as they are.
    pub fn push_once(&self) -> Result<()> {
        self.git(&["push", "origin", "main"])
    }

    /// Reset local branch to match remote
    pub fn reset_to_remote(&self) -> Result<()> {
        let output = git_command()
            .args(["reset", "--hard", "origin/main"])
            .current_dir(&self.repo_path)
            .output()?;

        if !output.status.success() {
            let error = String::from_utf8_lossy(&output.stderr);
            return Err(anyhow::anyhow!("Failed to reset: {}", error));
        }
        Ok(())
    }

    /// Returns true when a conflicting rebase discarded local commits.
    pub fn pull(&self) -> Result<bool> {
        // Abort any stale rebase from a previous interrupted sync
        if self.is_rebase_in_progress() {
            self.abort_rebase()?;
        }

        // Skip pull if remote branch doesn't exist (empty repository)
        if !self.remote_branch_exists("main") {
            return Ok(false);
        }

        // Fetch first, then rebase explicitly onto origin/main
        // This avoids "Cannot rebase onto multiple branches" errors
        let fetch_output = git_command()
            .args(["fetch", "origin", "main"])
            .current_dir(&self.repo_path)
            .stdin(Stdio::inherit())
            .output()?;

        if !fetch_output.status.success() {
            let error = String::from_utf8_lossy(&fetch_output.stderr);
            return Err(anyhow::anyhow!("Failed to fetch changes: {}", error));
        }

        let rebase_output = git_command()
            .args(["rebase", "--autostash", "origin/main"])
            .current_dir(&self.repo_path)
            .output()?;

        if !rebase_output.status.success() {
            // Only a content conflict discards local commits. Any other failure, such as
            // a hook or a lock, keeps them for the next sync.
            if !self.has_conflicts() {
                let error = String::from_utf8_lossy(&rebase_output.stderr);
                self.abort_rebase()?;
                // Quitting would leave the autostash unapplied, so keep the rebase for a later abort
                if self.is_rebase_in_progress() {
                    anyhow::bail!(
                        "Failed to rebase onto origin/main, and the rebase could not be aborted. Run 'git rebase --abort' in {}: {}",
                        self.repo_path.display(),
                        error.trim()
                    );
                }
                anyhow::bail!("Failed to rebase onto origin/main: {}", error.trim());
            }
            // Conflict: reset to remote. SyncState::discard_unpushed rolls back
            // the discarded files' hashes, so the next sync re-checks them.
            // Team and collab repos have no such re-export, so keep the local
            // commits on a branch and any uncommitted changes in a stash.
            self.abort_rebase()?;
            if self.is_rebase_in_progress() {
                self.git(&["rebase", "--quit"])?;
            }
            // refs/heads/main still holds the local tip even if the abort failed.
            // An empty clone has no local commits to keep.
            if self
                .git(&["rev-parse", "--verify", "refs/heads/main"])
                .is_err()
            {
                self.reset_to_remote()?;
                return Ok(false);
            }
            let branch = Utc::now()
                .format("tether-discarded-%Y%m%d-%H%M%S-%3f")
                .to_string();
            self.git(&["branch", &branch, "refs/heads/main"])?;
            let stashed = self.has_changes()?;
            if stashed {
                self.git(&["stash", "push", "--include-untracked", "-m", &branch])?;
            }
            self.git(&["checkout", "main"])?;
            self.reset_to_remote()?;
            crate::cli::Output::warning(&format!(
                "Local commits in {} conflicted with remote changes. Reset to remote; local commits kept on branch {}{}",
                self.repo_path.display(),
                branch,
                if stashed { ", uncommitted changes in git stash" } else { "" }
            ));
            if !cfg!(test) {
                crate::sync::conflict::notify_discarded_commits(&branch).ok();
            }
            return Ok(true);
        }

        // Rebased commits stay unsigned: commit signatures carry no trust, signed machine records do
        Ok(false)
    }

    /// True when HEAD has commits that origin/main lacks. False when origin/main
    /// does not exist: push only knows how to push main.
    pub fn has_unpushed_commits(&self) -> bool {
        if !self.has_commits() {
            return false;
        }
        let output = git_command()
            .args(["rev-list", "--count", "origin/main..HEAD"])
            .current_dir(&self.repo_path)
            .output();

        match output {
            Ok(out) if out.status.success() => String::from_utf8_lossy(&out.stdout).trim() != "0",
            _ => false,
        }
    }

    pub fn push(&self) -> Result<()> {
        let args = if self.remote_branch_exists("main") {
            vec!["push", "origin", "main"]
        } else {
            vec!["push", "-u", "origin", "main"]
        };

        for attempt in 1..=3 {
            let output = git_command()
                .args(&args)
                .current_dir(&self.repo_path)
                .stdin(Stdio::inherit())
                .output()?;

            if output.status.success() {
                return Ok(());
            }

            let error = String::from_utf8_lossy(&output.stderr);

            // Retry on rejection due to remote changes. GitHub reports a push
            // race as "[remote rejected] ... (cannot lock ref ...)" as well as
            // the usual "fetch first" / "non-fast-forward".
            let is_rejection = error.contains("fetch first")
                || error.contains("non-fast-forward")
                || error.contains("cannot lock ref");
            if is_rejection && attempt < 3 {
                // Sleep before pulling so the later machine rebases onto the
                // earlier machine's push. Jitter is random, not clock-based:
                // machines on the same sync interval fail at the same instant.
                let jitter_ms = RandomState::new().build_hasher().finish() % 400;
                let backoff_ms = 400 + attempt as u64 * 400 + jitter_ms;
                std::thread::sleep(std::time::Duration::from_millis(backoff_ms));
                // A retry after a reset would push nothing and report success
                if self
                    .pull()
                    .map_err(|e| anyhow::anyhow!("Push rejected, and pulling failed: {}", e))?
                {
                    return Err(anyhow::anyhow!(
                        "Push rejected and local changes conflicted with remote changes"
                    ));
                }
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
        let output = git_command()
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
        let output = git_command()
            .args(["status", "--porcelain"])
            .current_dir(&self.repo_path)
            .output()?;

        Ok(!output.stdout.is_empty())
    }

    /// Get commit history for a specific file in the repo
    pub fn file_log(&self, repo_path: &str, limit: usize) -> Result<Vec<FileLogEntry>> {
        let limit_arg = format!("-{}", limit);
        let output = git_command()
            .args([
                "log",
                "--format=%H|%h|%aI|%an|%s",
                &limit_arg,
                "--",
                repo_path,
            ])
            .current_dir(&self.repo_path)
            .output()?;

        if !output.status.success() {
            return Ok(Vec::new());
        }

        let stdout = String::from_utf8_lossy(&output.stdout);
        let mut entries = Vec::new();

        for line in stdout.lines() {
            if let Some(entry) = FileLogEntry::parse(line) {
                entries.push(entry);
            }
        }

        Ok(entries)
    }

    /// Get file contents at a specific commit
    pub fn show_at_commit(&self, commit: &str, repo_path: &str) -> Result<Vec<u8>> {
        if commit.is_empty() || !commit.chars().all(|c| c.is_ascii_hexdigit()) {
            anyhow::bail!("Invalid commit hash: {}", commit);
        }
        let spec = format!("{}:{}", commit, repo_path);
        let output = git_command()
            .args(["show", &spec])
            .current_dir(&self.repo_path)
            .output()?;

        if !output.status.success() {
            let error = String::from_utf8_lossy(&output.stderr);
            return Err(anyhow::anyhow!("Failed to get file at {}: {}", spec, error));
        }

        Ok(output.stdout)
    }

    /// Like file_log, but filters out commits where the file content didn't change.
    /// For encrypted files, this decrypts to compare plaintext.
    pub fn file_log_changed(
        &self,
        repo_path: &str,
        limit: usize,
        encrypted: bool,
    ) -> Result<Vec<FileLogEntry>> {
        // Fetch more than needed since some may be filtered out
        let entries = self.file_log(repo_path, limit * 3)?;
        let mut result = Vec::new();
        for entry in entries {
            if result.len() >= limit {
                break;
            }
            let changed = self
                .resolve_parent(&entry.commit_hash)
                .map(|parent| {
                    let old = self.file_content_at(&parent, repo_path, encrypted).ok();
                    let new = self
                        .file_content_at(&entry.commit_hash, repo_path, encrypted)
                        .ok();
                    old != new
                })
                .unwrap_or(true); // root commit always counts
            if changed {
                result.push(entry);
            }
        }
        Ok(result)
    }

    /// Get unified diff for a file at a specific commit.
    /// Decrypts if needed, diffs commit version against parent version.
    pub fn file_diff(
        &self,
        commit: &str,
        repo_path: &str,
        dotfile_path: &str,
        encrypted: bool,
    ) -> Result<String> {
        let new_text = self.file_content_at(commit, repo_path, encrypted)?;

        // Resolve parent hash; empty content if initial commit
        let old_text = self
            .resolve_parent(commit)
            .and_then(|parent| self.file_content_at(&parent, repo_path, encrypted).ok())
            .unwrap_or_default();

        Ok(text_diff(&old_text, &new_text, dotfile_path))
    }

    /// Resolve the parent commit hash, returning None for root commits.
    fn resolve_parent(&self, commit: &str) -> Option<String> {
        let output = git_command()
            .args(["rev-parse", &format!("{}^", commit)])
            .current_dir(&self.repo_path)
            .output()
            .ok()?;
        if !output.status.success() {
            return None;
        }
        let hash = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if hash.is_empty() {
            None
        } else {
            Some(hash)
        }
    }

    /// Get file content at a commit as a string, decrypting if needed.
    fn file_content_at(&self, commit: &str, repo_path: &str, encrypted: bool) -> Result<String> {
        let raw = self.show_at_commit(commit, repo_path)?;
        if encrypted {
            let key = crate::security::get_encryption_key()?;
            let bytes = crate::security::decrypt(&raw, &key)?;
            Ok(String::from_utf8_lossy(&bytes).into_owned())
        } else {
            Ok(String::from_utf8_lossy(&raw).into_owned())
        }
    }

    /// List all tracked files under a prefix in the repo
    pub fn list_tracked_files(&self, prefix: &str) -> Result<Vec<String>> {
        let output = git_command()
            .args(["ls-tree", "-r", "--name-only", "HEAD", "--", prefix])
            .current_dir(&self.repo_path)
            .output()?;

        if !output.status.success() {
            return Ok(Vec::new());
        }

        let stdout = String::from_utf8_lossy(&output.stdout);
        Ok(stdout.lines().map(|s| s.to_string()).collect())
    }
}

pub struct FileLogEntry {
    pub commit_hash: String,
    pub short_hash: String,
    pub date: DateTime<Utc>,
    pub message: String,
    pub machine_id: String,
}

impl FileLogEntry {
    pub fn parse(line: &str) -> Option<Self> {
        let parts: Vec<&str> = line.splitn(5, '|').collect();
        if parts.len() < 5 {
            return None;
        }
        let date = parts[2].parse::<DateTime<Utc>>().ok()?;
        Some(Self {
            commit_hash: parts[0].to_string(),
            short_hash: parts[1].to_string(),
            date,
            machine_id: parts[3].to_string(),
            message: parts[4].to_string(),
        })
    }
}

/// Generate a unified-style text diff between two strings
fn text_diff(old: &str, new: &str, label: &str) -> String {
    use similar::TextDiff;

    let diff = TextDiff::from_lines(old, new);
    diff.unified_diff()
        .header(&format!("a/{}", label), &format!("b/{}", label))
        .to_string()
}

/// Git utility functions for project config syncing
///
/// Get the git remote URL for a repository
pub fn get_remote_url(repo_path: &Path) -> Result<String> {
    let path_str = repo_path
        .to_str()
        .ok_or_else(|| anyhow::anyhow!("Path contains invalid UTF-8"))?;
    let output = git_command()
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
    let canonical = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    crate::sha256_hex(canonical.to_string_lossy().as_bytes())[..8].to_string()
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

    let output = git_command()
        .args(["-C", dir_str, "check-ignore", file_str])
        .output()?;

    // git check-ignore returns 0 if the file is ignored, 1 if not
    Ok(output.status.success())
}

/// Find all git repositories under a given path (recursive, max 3 levels deep)
/// Directories to skip when scanning for git repos or project files.
/// These are typically build artifacts, dependencies, or caches.
pub fn should_skip_dir(name: &str) -> bool {
    should_skip_dir_inner(name, true)
}

/// Like `should_skip_dir` but allows specific hidden dirs like `.vscode` and `.idea`
/// that project config scanning needs to traverse.
pub fn should_skip_dir_for_project_configs(name: &str) -> bool {
    should_skip_dir_inner(name, false)
}

fn should_skip_dir_inner(name: &str, skip_all_hidden: bool) -> bool {
    if name.starts_with('.') {
        if skip_all_hidden {
            return true;
        }
        // Allow project config dirs, skip everything else
        return !matches!(name, ".vscode" | ".idea" | ".run");
    }

    if name.ends_with(".egg-info") {
        return true;
    }

    matches!(
        name,
        "node_modules"
            | "bower_components"
            | "target"
            | "__pycache__"
            | "venv"
            | "env"
            | "bin"
            | "obj"
            | "packages"
            | "build"
            | "out"
            | "vendor"
            | "bundle"
            | "dist"
            | "coverage"
            | "tmp"
            | "temp"
            | "cache"
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
    fn test_project_config_skip_allows_ide_dirs() {
        // Base function skips all hidden dirs
        assert!(should_skip_dir(".vscode"));
        assert!(should_skip_dir(".idea"));
        assert!(should_skip_dir(".run"));
        // Project config variant allows IDE config dirs
        assert!(!should_skip_dir_for_project_configs(".vscode"));
        assert!(!should_skip_dir_for_project_configs(".idea"));
        assert!(!should_skip_dir_for_project_configs(".run"));
        // But still skips other hidden dirs
        assert!(should_skip_dir_for_project_configs(".git"));
        assert!(should_skip_dir_for_project_configs(".cache"));
        // And still skips non-hidden build dirs
        assert!(should_skip_dir_for_project_configs("node_modules"));
        assert!(should_skip_dir_for_project_configs("target"));
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

    fn git(dir: &Path, args: &[&str]) -> String {
        let out = Command::new("git")
            .args(args)
            .current_dir(dir)
            .output()
            .unwrap();
        assert!(out.status.success(), "git {:?}: {:?}", args, out);
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }

    /// A bare remote and two clones, with `shared` pushed from the first clone.
    fn two_clones(tmp: &Path) -> (GitBackend, GitBackend) {
        let remote = tmp.join("remote.git");
        git(
            tmp,
            &["init", "--bare", "-b", "main", remote.to_str().unwrap()],
        );
        let a = GitBackend::clone(remote.to_str().unwrap(), &tmp.join("a")).unwrap();
        git(&a.repo_path, &["symbolic-ref", "HEAD", "refs/heads/main"]);
        std::fs::write(a.repo_path.join("shared"), "base").unwrap();
        a.commit("base", "a").unwrap();
        a.push().unwrap();
        let b = GitBackend::clone(remote.to_str().unwrap(), &tmp.join("b")).unwrap();
        (a, b)
    }

    const NO_IDENTITY: &str = "TETHER_TEST_NO_GIT_IDENTITY";

    /// Runs the test `name` again in a child process whose git has no identity: no system
    /// config, and a HOME whose gitconfig forbids guessing one from the hostname.
    fn run_without_git_identity(name: &str) {
        let home = tempfile::TempDir::new().unwrap();
        std::fs::write(
            home.path().join(".gitconfig"),
            "[user]\n\tuseConfigOnly = true\n",
        )
        .unwrap();
        let mut cmd = Command::new(std::env::current_exe().unwrap());
        cmd.args(["--exact", name, "--test-threads=1"])
            .env(NO_IDENTITY, "1")
            .env("HOME", home.path())
            .env("XDG_CONFIG_HOME", home.path())
            .env("GIT_CONFIG_NOSYSTEM", "1");
        for var in [
            "GIT_CONFIG_GLOBAL",
            "GIT_AUTHOR_NAME",
            "GIT_AUTHOR_EMAIL",
            "GIT_COMMITTER_NAME",
            "GIT_COMMITTER_EMAIL",
            "EMAIL",
        ] {
            cmd.env_remove(var);
        }
        let out = cmd.output().unwrap();
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(
            out.status.success() && stdout.contains("1 passed"),
            "{}\n{}",
            stdout,
            String::from_utf8_lossy(&out.stderr)
        );
    }

    fn assert_no_git_identity(dir: &Path) {
        let ident = Command::new("git")
            .args(["var", "GIT_COMMITTER_IDENT"])
            .current_dir(dir)
            .output()
            .unwrap();
        assert!(!ident.status.success(), "git has an identity");
    }

    fn discarded_branches(dir: &Path) -> String {
        git(
            dir,
            &[
                "branch",
                "--list",
                "tether-discarded-*",
                "--format=%(refname:short)",
            ],
        )
    }

    #[test]
    fn test_rejected_push_rebases_without_git_identity() {
        if std::env::var_os(NO_IDENTITY).is_none() {
            return run_without_git_identity(
                "sync::git::tests::test_rejected_push_rebases_without_git_identity",
            );
        }
        let tmp = tempfile::TempDir::new().unwrap();
        let (a, b) = two_clones(tmp.path());
        assert_no_git_identity(&b.repo_path);
        std::fs::write(a.repo_path.join("shared"), "from a").unwrap();
        a.commit("a", "a").unwrap();
        a.push().unwrap();

        std::fs::write(b.repo_path.join("other"), "from b").unwrap();
        b.commit("b", "b").unwrap();
        // An uncommitted change makes the rebase write an autostash commit too
        std::fs::write(b.repo_path.join("other"), "uncommitted").unwrap();
        b.push().unwrap();

        assert!(!b.has_unpushed_commits());
        assert_eq!(discarded_branches(&b.repo_path), "");
        let read = |dir: &Path, file: &str| std::fs::read_to_string(dir.join(file)).unwrap();
        assert_eq!(read(&b.repo_path, "shared"), "from a");
        assert_eq!(read(&b.repo_path, "other"), "uncommitted");
        assert!(!a.pull().unwrap());
        assert_eq!(read(&a.repo_path, "other"), "from b");
    }

    #[test]
    fn test_rebase_failure_without_conflict_keeps_local_commits() {
        let tmp = tempfile::TempDir::new().unwrap();
        let (a, b) = two_clones(tmp.path());
        std::fs::write(a.repo_path.join("shared"), "from a").unwrap();
        a.commit("a", "a").unwrap();
        a.push().unwrap();

        std::fs::write(b.repo_path.join("other"), "from b").unwrap();
        b.commit("b", "b").unwrap();
        let local = git(&b.repo_path, &["rev-parse", "HEAD"]);
        let hook = b.repo_path.join(".git/hooks/pre-rebase");
        std::fs::write(&hook, "#!/bin/sh\necho hook says no >&2\nexit 1\n").unwrap();
        std::fs::set_permissions(&hook, std::os::unix::fs::PermissionsExt::from_mode(0o755))
            .unwrap();

        let error = b.pull().unwrap_err().to_string();
        assert!(error.contains("hook says no"), "{error}");
        let error = b.push().unwrap_err().to_string();
        assert!(
            error.contains("Push rejected, and pulling failed"),
            "{error}"
        );
        assert!(error.contains("hook says no"), "{error}");
        assert!(!b.is_rebase_in_progress());
        assert_eq!(git(&b.repo_path, &["rev-parse", "HEAD"]), local);
        assert_eq!(git(&b.repo_path, &["branch", "--show-current"]), "main");
        assert_eq!(discarded_branches(&b.repo_path), "");

        std::fs::remove_file(&hook).unwrap();
        b.push().unwrap();
        assert!(!b.has_unpushed_commits());
        assert!(!a.pull().unwrap());
        assert_eq!(
            std::fs::read_to_string(a.repo_path.join("other")).unwrap(),
            "from b"
        );
    }

    /// Verify HEAD with git itself, trusting `key` through an allowed signers file.
    fn git_verifies_head(dir: &Path, key: &ssh_key::PrivateKey) -> bool {
        let allowed = dir.join("allowed_signers");
        std::fs::write(
            &allowed,
            format!("m1 {}\n", key.public_key().to_openssh().unwrap()),
        )
        .unwrap();
        let out = Command::new("git")
            .args([
                "-c",
                "gpg.format=ssh",
                "-c",
                &format!("gpg.ssh.allowedSignersFile={}", allowed.display()),
                "log",
                "-1",
                "--show-signature",
            ])
            .current_dir(dir)
            .output()
            .unwrap();
        String::from_utf8_lossy(&out.stdout).contains("Good \"git\" signature for m1")
    }

    fn signing_key() -> ssh_key::PrivateKey {
        ssh_key::PrivateKey::random(&mut ssh_key::rand_core::OsRng, ssh_key::Algorithm::Ed25519)
            .unwrap()
    }

    #[test]
    fn test_signed_commits_verify_with_git() {
        let tmp = tempfile::TempDir::new().unwrap();
        let (a, _) = two_clones(tmp.path());
        let key = signing_key();
        std::fs::write(a.repo_path.join("shared"), "signed").unwrap();
        a.commit_with_key("signed", "a", Some(&key)).unwrap();
        assert!(git_verifies_head(&a.repo_path, &key));
        assert!(!git_verifies_head(&a.repo_path, &signing_key()));
    }

    #[test]
    fn test_push_rebases_non_conflicting_changes() {
        let tmp = tempfile::TempDir::new().unwrap();
        let (a, b) = two_clones(tmp.path());
        std::fs::write(a.repo_path.join("shared"), "from a").unwrap();
        a.commit("a", "a").unwrap();
        a.push().unwrap();

        std::fs::write(b.repo_path.join("other"), "from b").unwrap();
        b.commit("b", "b").unwrap();
        b.push().unwrap();

        assert!(!a.pull().unwrap());
        let other = std::fs::read_to_string(a.repo_path.join("other")).unwrap();
        assert_eq!(other, "from b");
    }

    #[test]
    fn test_push_fails_when_rebase_conflict_discards_commit() {
        let tmp = tempfile::TempDir::new().unwrap();
        let (a, b) = two_clones(tmp.path());
        std::fs::write(a.repo_path.join("shared"), "from a").unwrap();
        a.commit("a", "a").unwrap();
        a.push().unwrap();

        std::fs::write(b.repo_path.join("shared"), "from b").unwrap();
        b.commit("b", "b").unwrap();
        std::fs::write(b.repo_path.join("shared"), "uncommitted").unwrap();
        assert!(b.has_unpushed_commits());
        assert!(b.push().is_err());
        assert!(!b.has_unpushed_commits());

        let shared = std::fs::read_to_string(b.repo_path.join("shared")).unwrap();
        assert_eq!(shared, "from a");
        assert_eq!(
            git(&b.repo_path, &["rev-parse", "HEAD"]),
            git(&b.repo_path, &["rev-parse", "origin/main"])
        );

        let branch = git(
            &b.repo_path,
            &[
                "branch",
                "--list",
                "tether-discarded-*",
                "--format=%(refname:short)",
            ],
        );
        let kept = git(&b.repo_path, &["show", &format!("{}:shared", branch)]);
        assert_eq!(kept, "from b");
        let stashed = git(&b.repo_path, &["show", "stash@{0}:shared"]);
        assert_eq!(stashed, "uncommitted");
    }

    #[test]
    fn test_remove_and_commit_restores_files_when_the_commit_fails() {
        let tmp = tempfile::TempDir::new().unwrap();
        let (a, _) = two_clones(tmp.path());
        std::fs::create_dir_all(a.repo_path.join("machines")).unwrap();
        std::fs::write(a.repo_path.join("machines/old.json"), "record").unwrap();
        std::fs::write(a.repo_path.join("machines/old.json.sig"), "sig").unwrap();
        a.commit("add old", "a").unwrap();
        let paths = vec![
            "machines/old.json".to_string(),
            "machines/old.json.sig".to_string(),
        ];

        let lock = a.repo_path.join(".git/index.lock");
        std::fs::write(&lock, "").unwrap();
        assert!(a.remove_and_commit(&paths, "remove old", "a").is_err());
        let record = std::fs::read_to_string(a.repo_path.join("machines/old.json")).unwrap();
        assert_eq!(record, "record");
        assert!(a.repo_path.join("machines/old.json.sig").exists());

        std::fs::remove_file(&lock).unwrap();
        a.remove_and_commit(&paths, "remove old", "a").unwrap();
        assert!(!a.repo_path.join("machines/old.json").exists());
        assert_eq!(
            git(&a.repo_path, &["ls-tree", "-r", "--name-only", "HEAD"]),
            "shared"
        );
        assert_eq!(git(&a.repo_path, &["status", "--porcelain"]), "");
    }

    #[test]
    fn test_restore_after_a_failed_commit_never_writes_through_a_symlink() {
        let tmp = tempfile::TempDir::new().unwrap();
        let (a, _) = two_clones(tmp.path());
        let victim = tmp.path().join("state.json");
        std::fs::write(&victim, "state").unwrap();
        std::fs::create_dir_all(a.repo_path.join("machines")).unwrap();
        let link = a.repo_path.join("machines/old.json");
        std::os::unix::fs::symlink(&victim, &link).unwrap();
        a.commit("add old", "a").unwrap();
        let paths = vec!["machines/old.json".to_string()];

        std::fs::write(a.repo_path.join(".git/index.lock"), "").unwrap();
        assert!(a.remove_and_commit(&paths, "remove old", "a").is_err());
        assert_eq!(std::fs::read_to_string(&victim).unwrap(), "state");
        assert_eq!(std::fs::read_link(&link).unwrap(), victim);
    }
}

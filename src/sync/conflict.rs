use crate::cli::Output;
use crate::config::MergeConfig;
use anyhow::Result;
use chrono::{DateTime, Utc};
use owo_colors::OwoColorize;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::path::Path;

/// Represents a file conflict during sync
#[derive(Debug)]
pub struct FileConflict {
    /// Path relative to home (e.g., ".zshrc")
    pub file_path: String,
    /// Hash of local file
    pub local_hash: String,
    /// Hash from last successful sync (what we think remote has)
    pub last_synced_hash: Option<String>,
    /// Hash of file in remote repo
    pub remote_hash: String,
    /// Local file content
    pub local_content: Vec<u8>,
    /// Remote file content
    pub remote_content: Vec<u8>,
}

/// Result of conflict resolution
#[derive(Debug, Clone, PartialEq)]
pub enum ConflictResolution {
    KeepLocal,
    UseRemote,
    Merged,
    Skip,
}

impl FileConflict {
    /// Check if there's actually a conflict (both sides changed since last sync)
    pub fn is_true_conflict(&self) -> bool {
        match &self.last_synced_hash {
            Some(last) => {
                // Both changed: local != last AND remote != last
                self.local_hash != *last && self.remote_hash != *last
            }
            None => {
                // No sync history - conflict if they differ
                self.local_hash != self.remote_hash
            }
        }
    }

    /// Show unified diff between local and remote
    pub fn show_diff(&self) -> Result<()> {
        let local_str = String::from_utf8_lossy(&self.local_content);
        let remote_str = String::from_utf8_lossy(&self.remote_content);

        println!();
        println!(
            "{} {}",
            "Conflict in:".yellow().bold(),
            self.file_path.cyan()
        );
        println!("{}", "─".repeat(60).bright_black());

        // Simple line-by-line diff
        let local_lines: Vec<&str> = local_str.lines().collect();
        let remote_lines: Vec<&str> = remote_str.lines().collect();

        println!("{}", "--- local".red());
        println!("{}", "+++ remote".green());
        println!("{}", "─".repeat(60).bright_black());

        // Use a simple diff algorithm
        let diff = diff_lines(&local_lines, &remote_lines);
        for line in diff {
            match line {
                DiffLine::Same(s) => println!(" {}", s),
                DiffLine::Removed(s) => println!("{}", format!("-{}", s).red()),
                DiffLine::Added(s) => println!("{}", format!("+{}", s).green()),
            }
        }

        println!("{}", "─".repeat(60).bright_black());
        Ok(())
    }

    /// Prompt user for resolution
    pub fn prompt_resolution(&self) -> Result<ConflictResolution> {
        use inquire::Select;

        let options = vec![
            "Keep local version",
            "Use remote version",
            "Launch merge tool",
            "Skip (decide later)",
        ];

        let choice = Select::new(
            &format!("How do you want to resolve {}?", self.file_path),
            options,
        )
        .prompt()?;

        Ok(match choice {
            "Keep local version" => ConflictResolution::KeepLocal,
            "Use remote version" => ConflictResolution::UseRemote,
            "Launch merge tool" => ConflictResolution::Merged,
            _ => ConflictResolution::Skip,
        })
    }

    /// Launch external merge tool
    pub fn launch_merge_tool(
        &self,
        config: &MergeConfig,
        home: &Path,
    ) -> Result<ConflictResolution> {
        use std::process::Command;
        use tempfile::NamedTempFile;

        // Validate merge tool is in allowlist (security: prevents command injection via synced config)
        if !config.is_valid_command() {
            return Err(anyhow::anyhow!(
                "Merge tool '{}' is not in the allowed list. \
                 Edit ~/.tether/config.toml to use a supported merge tool.",
                config.command
            ));
        }

        // Create temp files for local and remote versions
        let mut local_temp = NamedTempFile::new()?;
        let mut remote_temp = NamedTempFile::new()?;

        std::io::Write::write_all(&mut local_temp, &self.local_content)?;
        std::io::Write::write_all(&mut remote_temp, &self.remote_content)?;

        let merged_path = home.join(&self.file_path);

        // Build command with placeholder substitution
        let args: Vec<String> = config
            .args
            .iter()
            .map(|arg| {
                arg.replace("{local}", local_temp.path().to_str().unwrap_or(""))
                    .replace("{remote}", remote_temp.path().to_str().unwrap_or(""))
                    .replace("{merged}", merged_path.to_str().unwrap_or(""))
            })
            .collect();

        Output::info(&format!("Launching {} for merge...", config.command));

        let status = Command::new(&config.command).args(&args).status()?;

        if status.success() {
            Output::success("Merge tool closed");
            // Check if the file was modified
            if merged_path.exists() {
                let new_content = std::fs::read(&merged_path)?;
                let new_hash = format!("{:x}", Sha256::digest(&new_content));
                if new_hash != self.local_hash {
                    Output::success("File was modified - using merged version");
                    return Ok(ConflictResolution::Merged);
                }
            }
            Output::info("No changes detected - keeping local version");
            Ok(ConflictResolution::KeepLocal)
        } else {
            Output::warning("Merge tool exited with error - keeping local version");
            Ok(ConflictResolution::KeepLocal)
        }
    }
}

/// Simple diff line representation
enum DiffLine<'a> {
    Same(&'a str),
    Removed(&'a str),
    Added(&'a str),
}

/// Simple line diff using longest common subsequence
fn diff_lines<'a>(old: &[&'a str], new: &[&'a str]) -> Vec<DiffLine<'a>> {
    let mut result = Vec::new();

    // Build LCS table
    let m = old.len();
    let n = new.len();
    let mut dp = vec![vec![0usize; n + 1]; m + 1];

    for i in 1..=m {
        for j in 1..=n {
            if old[i - 1] == new[j - 1] {
                dp[i][j] = dp[i - 1][j - 1] + 1;
            } else {
                dp[i][j] = dp[i - 1][j].max(dp[i][j - 1]);
            }
        }
    }

    // Backtrack to find diff
    let mut i = m;
    let mut j = n;
    let mut temp = Vec::new();

    while i > 0 || j > 0 {
        if i > 0 && j > 0 && old[i - 1] == new[j - 1] {
            temp.push(DiffLine::Same(old[i - 1]));
            i -= 1;
            j -= 1;
        } else if j > 0 && (i == 0 || dp[i][j - 1] >= dp[i - 1][j]) {
            temp.push(DiffLine::Added(new[j - 1]));
            j -= 1;
        } else {
            temp.push(DiffLine::Removed(old[i - 1]));
            i -= 1;
        }
    }

    temp.reverse();

    // Limit output to avoid overwhelming the terminal
    let max_lines = 50;
    if temp.len() > max_lines {
        result.extend(temp.into_iter().take(max_lines));
        result.push(DiffLine::Same("... (truncated)"));
    } else {
        result = temp;
    }

    result
}

/// Detect conflicts for a file
pub fn detect_conflict(
    file_path: &str,
    local_path: &Path,
    remote_content: &[u8],
    last_synced_hash: Option<&str>,
) -> Option<FileConflict> {
    let local_content = match std::fs::read(local_path) {
        Ok(c) => c,
        Err(_) => return None, // Local doesn't exist, no conflict
    };

    let local_hash = format!("{:x}", Sha256::digest(&local_content));
    let remote_hash = format!("{:x}", Sha256::digest(remote_content));

    // No conflict if hashes match
    if local_hash == remote_hash {
        return None;
    }

    // Check if it's a true conflict (both changed since last sync)
    let conflict = FileConflict {
        file_path: file_path.to_string(),
        local_hash,
        last_synced_hash: last_synced_hash.map(|s| s.to_string()),
        remote_hash,
        local_content,
        remote_content: remote_content.to_vec(),
    };

    if conflict.is_true_conflict() {
        Some(conflict)
    } else {
        None
    }
}

/// Pending conflict stored to disk for daemon/background detection
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PendingConflict {
    pub file_path: String,
    pub local_hash: String,
    pub remote_hash: String,
    pub detected_at: DateTime<Utc>,
}

/// Conflict state persisted to ~/.tether/conflicts.json
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ConflictState {
    pub conflicts: Vec<PendingConflict>,
}

impl ConflictState {
    pub fn path() -> Result<std::path::PathBuf> {
        let home = crate::home_dir()?;
        Ok(home.join(".tether").join("conflicts.json"))
    }

    pub fn load() -> Result<Self> {
        let path = Self::path()?;
        if !path.exists() {
            return Ok(Self::default());
        }
        let content = std::fs::read_to_string(path)?;
        Ok(serde_json::from_str(&content)?)
    }

    pub fn save(&self) -> Result<()> {
        let path = Self::path()?;
        let content = serde_json::to_string_pretty(self)?;
        crate::sync::atomic_write(&path, content.as_bytes())
    }

    pub fn add_conflict(&mut self, file_path: &str, local_hash: &str, remote_hash: &str) {
        // Remove existing conflict for same file
        self.conflicts.retain(|c| c.file_path != file_path);
        self.conflicts.push(PendingConflict {
            file_path: file_path.to_string(),
            local_hash: local_hash.to_string(),
            remote_hash: remote_hash.to_string(),
            detected_at: Utc::now(),
        });
    }

    pub fn remove_conflict(&mut self, file_path: &str) {
        self.conflicts.retain(|c| c.file_path != file_path);
    }

    pub fn has_conflicts(&self) -> bool {
        !self.conflicts.is_empty()
    }
}

/// Escape a string for safe use in AppleScript
fn escape_applescript(s: &str) -> String {
    // Remove any control characters and limit length for safety
    let sanitized: String = s.chars().filter(|c| !c.is_control()).take(100).collect();
    // Escape backslashes first, then quotes
    sanitized.replace('\\', "\\\\").replace('"', "\\\"")
}

/// Send macOS notification about conflict
pub fn notify_conflict(file_path: &str) -> Result<()> {
    use std::process::Command;

    let safe_path = escape_applescript(file_path);
    let script = format!(
        r#"display notification "Conflict detected in {}" with title "Tether" subtitle "Run 'tether resolve' to fix""#,
        safe_path
    );

    Command::new("osascript").args(["-e", &script]).output()?;

    Ok(())
}

/// Send macOS notification about multiple conflicts
pub fn notify_conflicts(count: usize) -> Result<()> {
    use std::process::Command;

    // count is a usize, no escaping needed
    let script = format!(
        r#"display notification "{} file conflicts detected" with title "Tether" subtitle "Run 'tether resolve' to fix""#,
        count
    );

    Command::new("osascript").args(["-e", &script]).output()?;

    Ok(())
}

/// Send macOS notification about deferred casks
pub fn notify_deferred_casks(casks: &[String]) -> Result<()> {
    use std::process::Command;

    let count = casks.len();
    let script = format!(
        r#"display notification "{} cask{} need{} password" with title "Tether" subtitle "Run 'tether sync' to install""#,
        count,
        if count == 1 { "" } else { "s" },
        if count == 1 { "s" } else { "" }
    );

    Command::new("osascript").args(["-e", &script]).output()?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    // is_true_conflict tests
    #[test]
    fn test_is_true_conflict_both_changed() {
        let conflict = FileConflict {
            file_path: ".zshrc".to_string(),
            local_hash: "aaa".to_string(),
            last_synced_hash: Some("bbb".to_string()),
            remote_hash: "ccc".to_string(),
            local_content: vec![],
            remote_content: vec![],
        };
        assert!(conflict.is_true_conflict());
    }

    #[test]
    fn test_is_not_conflict_only_local_changed() {
        let conflict = FileConflict {
            file_path: ".zshrc".to_string(),
            local_hash: "aaa".to_string(),
            last_synced_hash: Some("bbb".to_string()),
            remote_hash: "bbb".to_string(), // remote unchanged from last sync
            local_content: vec![],
            remote_content: vec![],
        };
        assert!(!conflict.is_true_conflict());
    }

    #[test]
    fn test_is_not_conflict_only_remote_changed() {
        let conflict = FileConflict {
            file_path: ".zshrc".to_string(),
            local_hash: "bbb".to_string(), // local unchanged from last sync
            last_synced_hash: Some("bbb".to_string()),
            remote_hash: "ccc".to_string(),
            local_content: vec![],
            remote_content: vec![],
        };
        assert!(!conflict.is_true_conflict());
    }

    #[test]
    fn test_is_conflict_no_sync_history_different() {
        let conflict = FileConflict {
            file_path: ".zshrc".to_string(),
            local_hash: "aaa".to_string(),
            last_synced_hash: None,
            remote_hash: "bbb".to_string(),
            local_content: vec![],
            remote_content: vec![],
        };
        assert!(conflict.is_true_conflict());
    }

    #[test]
    fn test_is_not_conflict_no_sync_history_same() {
        let conflict = FileConflict {
            file_path: ".zshrc".to_string(),
            local_hash: "aaa".to_string(),
            last_synced_hash: None,
            remote_hash: "aaa".to_string(),
            local_content: vec![],
            remote_content: vec![],
        };
        assert!(!conflict.is_true_conflict());
    }

    // detect_conflict tests
    #[test]
    fn test_detect_conflict_returns_none_when_equal() {
        let temp = TempDir::new().unwrap();
        let local_path = temp.path().join(".zshrc");
        let content = b"same content";
        std::fs::write(&local_path, content).unwrap();

        let result = detect_conflict(".zshrc", &local_path, content, None);
        assert!(result.is_none());
    }

    #[test]
    fn test_detect_conflict_returns_some_when_differ_no_history() {
        let temp = TempDir::new().unwrap();
        let local_path = temp.path().join(".zshrc");
        std::fs::write(&local_path, b"local content").unwrap();

        let result = detect_conflict(".zshrc", &local_path, b"remote content", None);
        assert!(result.is_some());
    }

    #[test]
    fn test_detect_conflict_returns_none_when_local_missing() {
        let temp = TempDir::new().unwrap();
        let local_path = temp.path().join("nonexistent");

        let result = detect_conflict("nonexistent", &local_path, b"remote", None);
        assert!(result.is_none());
    }

    // ConflictState tests
    #[test]
    fn test_conflict_state_add_remove() {
        let mut state = ConflictState::default();
        assert!(!state.has_conflicts());

        state.add_conflict(".zshrc", "aaa", "bbb");
        assert!(state.has_conflicts());
        assert_eq!(state.conflicts.len(), 1);

        state.add_conflict(".zshrc", "aaa", "ccc"); // overwrites
        assert_eq!(state.conflicts.len(), 1);
        assert_eq!(state.conflicts[0].remote_hash, "ccc");

        state.add_conflict(".bashrc", "xxx", "yyy"); // different file
        assert_eq!(state.conflicts.len(), 2);

        state.remove_conflict(".zshrc");
        assert_eq!(state.conflicts.len(), 1);
        assert_eq!(state.conflicts[0].file_path, ".bashrc");

        state.remove_conflict(".bashrc");
        assert!(!state.has_conflicts());
    }

    // escape_applescript tests
    #[test]
    fn test_escape_applescript_plain() {
        assert_eq!(escape_applescript("hello"), "hello");
    }

    #[test]
    fn test_escape_applescript_quotes() {
        assert_eq!(escape_applescript("hello\"world"), "hello\\\"world");
    }

    #[test]
    fn test_escape_applescript_backslashes() {
        assert_eq!(escape_applescript("path\\to\\file"), "path\\\\to\\\\file");
    }

    #[test]
    fn test_escape_applescript_truncates_long() {
        let long = "a".repeat(200);
        let escaped = escape_applescript(&long);
        assert!(escaped.len() <= 100);
    }

    #[test]
    fn test_escape_applescript_removes_control_chars() {
        let with_control = "hello\nworld\ttab";
        let escaped = escape_applescript(with_control);
        assert!(!escaped.contains('\n'));
        assert!(!escaped.contains('\t'));
    }
}

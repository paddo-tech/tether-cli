use anyhow::Result;
use chrono::{DateTime, Utc};
use std::path::{Path, PathBuf};

const MAX_BACKUPS: usize = 5;

/// Get the backups directory
pub fn backups_dir() -> Result<PathBuf> {
    let home = crate::home_dir()?;
    Ok(home.join(".tether/backups"))
}

/// Create a timestamped backup directory and return its path
pub fn create_backup_dir() -> Result<PathBuf> {
    let timestamp = Utc::now().format("%Y-%m-%dT%H-%M-%S").to_string();
    let backup_dir = backups_dir()?.join(&timestamp);
    std::fs::create_dir_all(&backup_dir)?;
    Ok(backup_dir)
}

/// Backup a single file before it gets overwritten
/// Returns true if backup was created, false if skipped (file doesn't exist)
pub fn backup_file(
    backup_dir: &Path,
    category: &str,
    relative_path: &str,
    source: &Path,
) -> Result<bool> {
    if !source.exists() {
        return Ok(false);
    }

    let dest = backup_dir.join(category).join(relative_path);
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent)?;
    }

    std::fs::copy(source, &dest)?;
    Ok(true)
}

/// List all backup timestamps, newest first
pub fn list_backups() -> Result<Vec<String>> {
    let dir = backups_dir()?;
    if !dir.exists() {
        return Ok(Vec::new());
    }

    let mut backups: Vec<String> = std::fs::read_dir(&dir)?
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().map(|t| t.is_dir()).unwrap_or(false))
        .filter_map(|e| e.file_name().to_str().map(|s| s.to_string()))
        .collect();

    // Sort newest first (reverse chronological)
    backups.sort();
    backups.reverse();

    Ok(backups)
}

/// Get files in a specific backup
pub fn list_backup_files(timestamp: &str) -> Result<Vec<(String, String)>> {
    let backup_dir = backups_dir()?.join(timestamp);
    if !backup_dir.exists() {
        anyhow::bail!("Backup '{}' not found", timestamp);
    }

    let mut files = Vec::new();
    collect_files_recursive(&backup_dir, &backup_dir, &mut files)?;
    Ok(files)
}

fn collect_files_recursive(
    base: &Path,
    current: &Path,
    files: &mut Vec<(String, String)>,
) -> Result<()> {
    for entry in std::fs::read_dir(current)? {
        let entry = entry?;
        let path = entry.path();

        if path.is_dir() {
            collect_files_recursive(base, &path, files)?;
        } else if path.is_file() {
            let relative = path.strip_prefix(base)?;
            let components: Vec<_> = relative.components().collect();

            if components.len() >= 2 {
                let category = components[0].as_os_str().to_string_lossy().to_string();
                let file_path = components[1..]
                    .iter()
                    .map(|c| c.as_os_str().to_string_lossy())
                    .collect::<Vec<_>>()
                    .join("/");
                files.push((category, file_path));
            }
        }
    }
    Ok(())
}

/// Restore a file from backup to its original location
pub fn restore_file(timestamp: &str, category: &str, relative_path: &str) -> Result<PathBuf> {
    let backup_file = backups_dir()?
        .join(timestamp)
        .join(category)
        .join(relative_path);
    if !backup_file.exists() {
        anyhow::bail!("Backup file not found: {}/{}", category, relative_path);
    }

    let home = crate::home_dir()?;

    let dest = match category {
        "dotfiles" => home.join(relative_path),
        "projects" => {
            // Project files need to find the actual repo location
            // Format: projects/github.com/user/repo/path/to/file
            // We need to search for the repo in configured search paths
            anyhow::bail!("Project file restore requires specifying destination. Use: tether restore {} {} --to <path>", timestamp, relative_path);
        }
        _ => anyhow::bail!("Unknown backup category: {}", category),
    };

    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent)?;
    }

    std::fs::copy(&backup_file, &dest)?;
    Ok(dest)
}

/// Prune old backups, keeping only the most recent MAX_BACKUPS
pub fn prune_old_backups() -> Result<usize> {
    let backups = list_backups()?;

    if backups.len() <= MAX_BACKUPS {
        return Ok(0);
    }

    let to_remove = &backups[MAX_BACKUPS..];
    let dir = backups_dir()?;

    for backup in to_remove {
        let path = dir.join(backup);
        std::fs::remove_dir_all(&path)?;
    }

    Ok(to_remove.len())
}

/// Parse a backup timestamp string into DateTime
pub fn parse_backup_timestamp(timestamp: &str) -> Option<DateTime<Utc>> {
    chrono::NaiveDateTime::parse_from_str(timestamp, "%Y-%m-%dT%H-%M-%S")
        .ok()
        .map(|dt| dt.and_utc())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_backup_file_copies() {
        let temp = TempDir::new().unwrap();
        let source = temp.path().join("source.txt");
        std::fs::write(&source, "content").unwrap();

        let backup_dir = temp.path().join("backup");
        std::fs::create_dir(&backup_dir).unwrap();

        let result = backup_file(&backup_dir, "dotfiles", ".zshrc", &source).unwrap();
        assert!(result);
        assert!(backup_dir.join("dotfiles/.zshrc").exists());

        let backed_up = std::fs::read_to_string(backup_dir.join("dotfiles/.zshrc")).unwrap();
        assert_eq!(backed_up, "content");
    }

    #[test]
    fn test_backup_file_skips_missing() {
        let temp = TempDir::new().unwrap();
        let backup_dir = temp.path().join("backup");
        std::fs::create_dir(&backup_dir).unwrap();

        let result = backup_file(
            &backup_dir,
            "dotfiles",
            ".zshrc",
            &temp.path().join("nonexistent"),
        )
        .unwrap();
        assert!(!result);
    }

    #[test]
    fn test_backup_file_creates_nested_dirs() {
        let temp = TempDir::new().unwrap();
        let source = temp.path().join("source.txt");
        std::fs::write(&source, "nested").unwrap();

        let backup_dir = temp.path().join("backup");
        std::fs::create_dir(&backup_dir).unwrap();

        let result =
            backup_file(&backup_dir, "dotfiles", ".config/nvim/init.lua", &source).unwrap();
        assert!(result);
        assert!(backup_dir.join("dotfiles/.config/nvim/init.lua").exists());
    }

    #[test]
    fn test_parse_backup_timestamp_valid() {
        let ts = parse_backup_timestamp("2024-01-15T10-30-45");
        assert!(ts.is_some());
        let dt = ts.unwrap();
        assert_eq!(dt.format("%Y-%m-%d").to_string(), "2024-01-15");
    }

    #[test]
    fn test_parse_backup_timestamp_invalid() {
        assert!(parse_backup_timestamp("invalid").is_none());
        assert!(parse_backup_timestamp("2024/01/15").is_none());
        assert!(parse_backup_timestamp("2024-01-15T10:30:45").is_none()); // wrong separators
        assert!(parse_backup_timestamp("").is_none());
    }
}

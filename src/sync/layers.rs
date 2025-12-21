use anyhow::{Context, Result};
use std::fs;
use std::path::{Path, PathBuf};

use crate::sync::merge::{detect_file_type, merge_files, FileType};

/// Get the layers directory (~/.tether/layers)
pub fn layers_dir() -> Result<PathBuf> {
    let home = home::home_dir().context("Could not find home directory")?;
    Ok(home.join(".tether").join("layers"))
}

/// Get the personal layer directory
pub fn personal_layer_dir() -> Result<PathBuf> {
    Ok(layers_dir()?.join("personal"))
}

/// Get a team's layer directory
pub fn team_layer_dir(team_name: &str) -> Result<PathBuf> {
    Ok(layers_dir()?.join("teams").join(team_name))
}

/// Get the merged output directory
pub fn merged_dir() -> Result<PathBuf> {
    let home = home::home_dir().context("Could not find home directory")?;
    Ok(home.join(".tether").join("merged"))
}

/// Initialize layer directories
pub fn init_layers(team_name: &str) -> Result<()> {
    fs::create_dir_all(personal_layer_dir()?)?;
    fs::create_dir_all(team_layer_dir(team_name)?)?;
    fs::create_dir_all(merged_dir()?)?;
    Ok(())
}

/// Map team dotfile name to personal dotfile name
/// e.g., "acme.zshrc" -> ".zshrc" (with team_slug "acme")
/// Also handles legacy "team." prefix for backwards compatibility
pub fn map_team_to_personal_name(team_filename: &str, team_slug: &str) -> String {
    // Try stripping team slug prefix first (new format: "acme.zshrc")
    let slug_prefix = format!("{}.", team_slug);
    if let Some(stripped) = team_filename.strip_prefix(&slug_prefix) {
        return format!(".{}", stripped);
    }

    // Backwards compatibility: handle "team." prefix
    if let Some(stripped) = team_filename.strip_prefix("team.") {
        return format!(".{}", stripped);
    }

    // If already starts with dot, keep as-is
    if team_filename.starts_with('.') {
        return team_filename.to_string();
    }

    // Default: prepend dot
    format!(".{}", team_filename)
}

/// Copy team dotfiles from repo to team layer
/// Renames team.* files to .* (e.g., team.zshrc -> .zshrc)
pub fn sync_team_to_layer(team_name: &str, team_repo_dotfiles: &Path) -> Result<Vec<String>> {
    let team_layer = team_layer_dir(team_name)?;
    fs::create_dir_all(&team_layer)?;

    let mut synced_files = Vec::new();

    if !team_repo_dotfiles.exists() {
        return Ok(synced_files);
    }

    for entry in fs::read_dir(team_repo_dotfiles)? {
        let entry = entry?;
        let path = entry.path();

        if path.is_file() {
            if let Some(orig_name) = entry.file_name().to_str() {
                // Map {slug}.* to .*
                let personal_name = map_team_to_personal_name(orig_name, team_name);
                let dest = team_layer.join(&personal_name);
                fs::copy(&path, &dest)?;
                synced_files.push(personal_name);
            }
        }
    }

    Ok(synced_files)
}

/// Capture personal dotfile to personal layer (if not already captured)
/// Returns true if captured, false if already exists
pub fn capture_personal_to_layer(filename: &str) -> Result<bool> {
    let home = home::home_dir().context("Could not find home directory")?;
    let personal_file = home.join(filename);
    let layer_file = personal_layer_dir()?.join(filename);

    // Only capture if personal file exists and not yet in layer
    if personal_file.exists() && !layer_file.exists() {
        fs::create_dir_all(layer_file.parent().unwrap())?;
        fs::copy(&personal_file, &layer_file)?;
        return Ok(true);
    }

    Ok(false)
}

/// Update personal layer from home directory (for ongoing sync)
pub fn update_personal_layer(filename: &str) -> Result<()> {
    let home = home::home_dir().context("Could not find home directory")?;
    let personal_file = home.join(filename);
    let layer_file = personal_layer_dir()?.join(filename);

    if personal_file.exists() {
        fs::create_dir_all(layer_file.parent().unwrap())?;
        fs::copy(&personal_file, &layer_file)?;
    }

    Ok(())
}

/// Merge team and personal layers, write to merged directory
/// Returns the merged file path
pub fn merge_layers(team_name: &str, filename: &str) -> Result<PathBuf> {
    let team_file = team_layer_dir(team_name)?.join(filename);
    let personal_file = personal_layer_dir()?.join(filename);
    let merged_file = merged_dir()?.join(filename);

    fs::create_dir_all(merged_file.parent().unwrap())?;

    let merged_content = if personal_file.exists() && team_file.exists() {
        // Both exist - merge with personal winning
        merge_files(&team_file, &personal_file)?
    } else if personal_file.exists() {
        // Only personal - use as-is
        fs::read_to_string(&personal_file)?
    } else if team_file.exists() {
        // Only team - use as-is
        fs::read_to_string(&team_file)?
    } else {
        return Err(anyhow::anyhow!(
            "Neither team nor personal file exists for {}",
            filename
        ));
    };

    fs::write(&merged_file, &merged_content)?;
    Ok(merged_file)
}

/// Apply merged file to home directory
pub fn apply_merged_to_home(filename: &str) -> Result<()> {
    let home = home::home_dir().context("Could not find home directory")?;
    let merged_file = merged_dir()?.join(filename);
    let home_file = home.join(filename);

    if merged_file.exists() {
        // Backup existing file if different
        if home_file.exists() {
            let home_content = fs::read_to_string(&home_file)?;
            let merged_content = fs::read_to_string(&merged_file)?;
            if home_content != merged_content {
                // Create backup directory and backup the file
                let backup_dir = crate::sync::create_backup_dir()?;
                crate::sync::backup_file(&backup_dir, "dotfiles", filename, &home_file)?;
            }
        }

        fs::copy(&merged_file, &home_file)?;
    }

    Ok(())
}

/// Full layer sync for a team dotfile:
/// 1. Capture personal to layer (first time)
/// 2. Merge team + personal
/// 3. Apply to home
pub fn sync_dotfile_with_layers(team_name: &str, filename: &str) -> Result<LayerSyncResult> {
    let home = home::home_dir().context("Could not find home directory")?;
    let team_file = team_layer_dir(team_name)?.join(filename);
    let personal_layer_file = personal_layer_dir()?.join(filename);
    let home_file = home.join(filename);

    // Skip if team file doesn't exist
    if !team_file.exists() {
        return Ok(LayerSyncResult::Skipped);
    }

    let file_type = detect_file_type(Path::new(filename));
    let had_personal = home_file.exists();

    // Capture personal file to layer (first time only)
    if had_personal && !personal_layer_file.exists() {
        capture_personal_to_layer(filename)?;
    }

    // Merge and apply
    merge_layers(team_name, filename)?;
    apply_merged_to_home(filename)?;

    if had_personal {
        Ok(LayerSyncResult::Merged { file_type })
    } else {
        Ok(LayerSyncResult::TeamOnly)
    }
}

/// Result of syncing a dotfile with layers
#[derive(Debug)]
pub enum LayerSyncResult {
    /// Team and personal were merged
    Merged { file_type: FileType },
    /// Only team file existed
    TeamOnly,
    /// File was skipped (no team file)
    Skipped,
}

/// Clean up layers for a team
pub fn cleanup_team_layers(team_name: &str) -> Result<()> {
    let team_layer = team_layer_dir(team_name)?;
    if team_layer.exists() {
        fs::remove_dir_all(&team_layer)?;
    }
    Ok(())
}

/// List files in team layer
pub fn list_team_layer_files(team_name: &str) -> Result<Vec<String>> {
    let team_layer = team_layer_dir(team_name)?;
    let mut files = Vec::new();

    if team_layer.exists() {
        for entry in fs::read_dir(&team_layer)? {
            let entry = entry?;
            if entry.path().is_file() {
                if let Some(name) = entry.file_name().to_str() {
                    files.push(name.to_string());
                }
            }
        }
    }

    Ok(files)
}

/// Re-merge all dotfiles for a team (after personal or team changes)
pub fn remerge_all(team_name: &str) -> Result<Vec<String>> {
    let team_layer = team_layer_dir(team_name)?;
    let mut remerged = Vec::new();

    if !team_layer.exists() {
        return Ok(remerged);
    }

    for entry in fs::read_dir(&team_layer)? {
        let entry = entry?;
        if entry.path().is_file() {
            if let Some(filename) = entry.file_name().to_str() {
                merge_layers(team_name, filename)?;
                apply_merged_to_home(filename)?;
                remerged.push(filename.to_string());
            }
        }
    }

    Ok(remerged)
}

/// Reset a file to the team version (clobber local changes)
/// This copies the team version directly to home, bypassing personal layer
pub fn reset_to_team(team_name: &str, filename: &str) -> Result<()> {
    let home = home::home_dir().context("Could not find home directory")?;
    let team_file = team_layer_dir(team_name)?.join(filename);
    let personal_file = personal_layer_dir()?.join(filename);
    let home_file = home.join(filename);

    if !team_file.exists() {
        return Err(anyhow::anyhow!(
            "Team file does not exist: {}",
            team_file.display()
        ));
    }

    // Backup current home file if it exists and differs
    if home_file.exists() {
        let backup_dir = crate::sync::create_backup_dir()?;
        crate::sync::backup_file(&backup_dir, "reset", filename, &home_file)?;
    }

    // Remove personal layer copy (resets user customizations)
    if personal_file.exists() {
        fs::remove_file(&personal_file)?;
    }

    // Copy team version directly to home
    fs::copy(&team_file, &home_file)?;

    Ok(())
}

/// Reset all team files to team versions
pub fn reset_all_to_team(team_name: &str) -> Result<Vec<String>> {
    let team_files = list_team_layer_files(team_name)?;
    let mut reset_files = Vec::new();

    for filename in team_files {
        reset_to_team(team_name, &filename)?;
        reset_files.push(filename);
    }

    Ok(reset_files)
}

/// Promote a local file to the team repository
/// This copies the home version to the team repo's dotfiles directory
pub fn promote_to_team(team_name: &str, filename: &str, team_repo_path: &Path) -> Result<()> {
    let home = home::home_dir().context("Could not find home directory")?;
    let home_file = home.join(filename);

    if !home_file.exists() {
        return Err(anyhow::anyhow!(
            "Local file does not exist: {}",
            home_file.display()
        ));
    }

    // Map personal filename to team filename (e.g., .zshrc -> acme.zshrc)
    let team_filename = map_personal_to_team_name(filename, team_name);
    let team_dotfiles = team_repo_path.join("dotfiles");
    let dest = team_dotfiles.join(&team_filename);

    // Ensure dotfiles directory exists
    fs::create_dir_all(&team_dotfiles)?;

    // Copy local file to team repo
    fs::copy(&home_file, &dest)?;

    // Also update the team layer
    let team_layer_file = team_layer_dir(team_name)?.join(filename);
    fs::create_dir_all(team_layer_file.parent().unwrap())?;
    fs::copy(&home_file, &team_layer_file)?;

    Ok(())
}

/// Map personal dotfile name to team repo name
/// e.g., ".zshrc" -> "acme.zshrc" (with team_slug "acme")
pub fn map_personal_to_team_name(personal_name: &str, team_slug: &str) -> String {
    if let Some(stripped) = personal_name.strip_prefix('.') {
        format!("{}.{}", team_slug, stripped)
    } else {
        format!("{}.{}", team_slug, personal_name)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_layer_paths() {
        let layers = layers_dir().unwrap();
        assert!(layers.ends_with("layers"));

        let personal = personal_layer_dir().unwrap();
        assert!(personal.ends_with("personal"));

        let team = team_layer_dir("acme").unwrap();
        assert!(team.ends_with("acme"));
    }

    #[test]
    fn test_map_personal_to_team_name() {
        assert_eq!(map_personal_to_team_name(".zshrc", "acme"), "acme.zshrc");
        assert_eq!(
            map_personal_to_team_name(".gitconfig", "acme"),
            "acme.gitconfig"
        );
        assert_eq!(
            map_personal_to_team_name("file.txt", "corp"),
            "corp.file.txt"
        );
    }

    #[test]
    fn test_map_team_to_personal_name() {
        // New format with org slug
        assert_eq!(map_team_to_personal_name("acme.zshrc", "acme"), ".zshrc");
        assert_eq!(
            map_team_to_personal_name("acme.gitconfig", "acme"),
            ".gitconfig"
        );
        // Backwards compat: team.* prefix still works
        assert_eq!(map_team_to_personal_name("team.zshrc", "acme"), ".zshrc");
        assert_eq!(
            map_team_to_personal_name("team.gitconfig", "acme"),
            ".gitconfig"
        );
        // Already has dot prefix
        assert_eq!(map_team_to_personal_name(".zshrc", "acme"), ".zshrc");
    }
}

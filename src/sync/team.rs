use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// Tracks team-synced symlinks and conflict resolutions
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct TeamManifest {
    /// Symlinks created by team sync: target_path -> source_path
    pub symlinks: HashMap<String, String>,
    /// Conflict resolutions: target_path -> resolution
    pub conflicts: HashMap<String, ConflictResolution>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ConflictResolution {
    /// Personal file kept, team config skipped
    PersonalWins,
    /// Personal file renamed to .personal suffix, team config symlinked
    PersonalRenamed,
    /// Team config symlinked with .team suffix, personal kept
    TeamRenamed,
}

impl TeamManifest {
    /// Load manifest from disk
    pub fn load() -> Result<Self> {
        let path = Self::manifest_path()?;
        if !path.exists() {
            return Ok(Self::default());
        }

        let content = std::fs::read_to_string(&path).context("Failed to read team manifest")?;
        let manifest: TeamManifest =
            serde_json::from_str(&content).context("Failed to parse team manifest")?;
        Ok(manifest)
    }

    /// Save manifest to disk
    pub fn save(&self) -> Result<()> {
        let path = Self::manifest_path()?;
        let content =
            serde_json::to_string_pretty(self).context("Failed to serialize team manifest")?;
        std::fs::write(&path, content).context("Failed to write team manifest")?;
        Ok(())
    }

    /// Get manifest file path
    fn manifest_path() -> Result<PathBuf> {
        let config_dir = crate::config::Config::config_dir()?;
        Ok(config_dir.join("team-manifest.json"))
    }

    /// Add a symlink to the manifest
    pub fn add_symlink(&mut self, target: PathBuf, source: PathBuf) {
        self.symlinks.insert(
            target.to_string_lossy().to_string(),
            source.to_string_lossy().to_string(),
        );
    }

    /// Record a conflict resolution
    pub fn add_conflict(&mut self, target: PathBuf, resolution: ConflictResolution) {
        self.conflicts
            .insert(target.to_string_lossy().to_string(), resolution);
    }

    /// Remove all symlinks and clean up manifest
    pub fn cleanup(&mut self) -> Result<()> {
        for target_str in self.symlinks.keys() {
            let target = PathBuf::from(target_str);
            if target.exists() && target.is_symlink() {
                std::fs::remove_file(&target)
                    .with_context(|| format!("Failed to remove symlink: {}", target_str))?;
            }
        }

        // Also clean up renamed personal files if they still have .personal extension
        for (target_str, resolution) in &self.conflicts {
            if let ConflictResolution::PersonalRenamed = resolution {
                let personal_path = PathBuf::from(format!("{}.personal", target_str));
                if personal_path.exists() {
                    // Don't auto-delete renamed personal files, just notify
                    eprintln!(
                        "Note: Renamed personal file still exists: {}",
                        personal_path.display()
                    );
                }
            }
        }

        self.symlinks.clear();
        self.conflicts.clear();
        self.save()?;
        Ok(())
    }
}

/// Discovers directories in team repo that should be symlinked
pub fn discover_symlinkable_dirs(team_sync_dir: &Path) -> Result<Vec<SymlinkableDir>> {
    let mut dirs = Vec::new();
    let home = home::home_dir().context("Could not find home directory")?;

    // Check for common config directories
    let candidates = vec![
        (".claude", ".claude"),
        (".config", ".config"),
        // Add more as needed
    ];

    for (team_subdir, home_target) in candidates {
        let team_path = team_sync_dir.join(team_subdir);
        if team_path.exists() && team_path.is_dir() {
            dirs.push(SymlinkableDir {
                team_path: team_path.clone(),
                target_base: home.join(home_target),
            });
        }
    }

    Ok(dirs)
}

#[derive(Debug)]
pub struct SymlinkableDir {
    /// Path in team repo (e.g., ~/.tether/team-sync/.claude)
    pub team_path: PathBuf,
    /// Target base directory (e.g., ~/.claude)
    pub target_base: PathBuf,
}

/// Result of attempting to create a symlink
#[derive(Debug)]
pub enum SymlinkResult {
    Created(PathBuf),
    Conflict(PathBuf),
    Skipped(PathBuf),
}

impl SymlinkableDir {
    /// Create symlinks for all items in this directory
    pub fn create_symlinks(
        &self,
        manifest: &mut TeamManifest,
        auto_resolve: bool,
    ) -> Result<Vec<SymlinkResult>> {
        let mut results = Vec::new();

        // Ensure target base exists
        if !self.target_base.exists() {
            std::fs::create_dir_all(&self.target_base)
                .context("Failed to create target directory")?;
        }

        // Iterate through items in team directory
        for entry in std::fs::read_dir(&self.team_path)? {
            let entry = entry?;
            let team_item = entry.path();
            let item_name = entry.file_name();
            let target_item = self.target_base.join(&item_name);

            // Check if target already exists
            if target_item.exists() && !target_item.is_symlink() {
                if auto_resolve {
                    // Skip conflicts in auto mode
                    manifest.add_conflict(target_item.clone(), ConflictResolution::PersonalWins);
                    results.push(SymlinkResult::Conflict(target_item));
                } else {
                    // In interactive mode, this will be handled by caller
                    results.push(SymlinkResult::Conflict(target_item));
                }
            } else {
                // Create symlink
                if target_item.exists() {
                    std::fs::remove_file(&target_item)?; // Remove old symlink if exists
                }

                #[cfg(unix)]
                std::os::unix::fs::symlink(&team_item, &target_item).with_context(|| {
                    format!(
                        "Failed to create symlink: {} -> {}",
                        target_item.display(),
                        team_item.display()
                    )
                })?;

                #[cfg(windows)]
                {
                    if team_item.is_dir() {
                        std::os::windows::fs::symlink_dir(&team_item, &target_item).with_context(
                            || {
                                format!(
                                    "Failed to create directory symlink: {} -> {}",
                                    target_item.display(),
                                    team_item.display()
                                )
                            },
                        )?;
                    } else {
                        std::os::windows::fs::symlink_file(&team_item, &target_item).with_context(
                            || {
                                format!(
                                    "Failed to create file symlink: {} -> {}",
                                    target_item.display(),
                                    team_item.display()
                                )
                            },
                        )?;
                    }
                }

                manifest.add_symlink(target_item.clone(), team_item);
                results.push(SymlinkResult::Created(target_item));
            }
        }

        Ok(results)
    }
}

/// Handle a conflict by prompting user
pub fn resolve_conflict(target: &Path, team_source: &Path) -> Result<ConflictResolution> {
    use crate::cli::{Output, Prompt};

    println!();
    Output::warning(&format!("Conflict: {}", target.display()));
    Output::info("A personal config already exists at this location");
    println!();
    println!("Options:");
    println!("  1. Keep personal (skip team sync for this file)");
    println!(
        "  2. Rename personal -> {}.personal, use team version",
        target.file_name().unwrap().to_string_lossy()
    );
    println!(
        "  3. Rename team -> {}.team, keep personal",
        target.file_name().unwrap().to_string_lossy()
    );
    println!();

    let choice = Prompt::select(
        "Choose an option:",
        vec!["Keep personal", "Rename personal", "Rename team"],
        0,
    )?;

    match choice {
        0 => Ok(ConflictResolution::PersonalWins),
        1 => {
            // Rename personal file
            let personal_backup = target.with_extension("personal");
            std::fs::rename(target, &personal_backup).context("Failed to rename personal file")?;

            // Create symlink to team config
            #[cfg(unix)]
            std::os::unix::fs::symlink(team_source, target)
                .context("Failed to create symlink after renaming personal")?;

            #[cfg(windows)]
            {
                if team_source.is_dir() {
                    std::os::windows::fs::symlink_dir(team_source, target)?;
                } else {
                    std::os::windows::fs::symlink_file(team_source, target)?;
                }
            }

            Output::success(&format!(
                "Personal file renamed to: {}",
                personal_backup.display()
            ));
            Ok(ConflictResolution::PersonalRenamed)
        }
        2 => {
            // Create team symlink with .team suffix
            let team_link = target.with_extension("team");

            #[cfg(unix)]
            std::os::unix::fs::symlink(team_source, &team_link)
                .context("Failed to create team symlink")?;

            #[cfg(windows)]
            {
                if team_source.is_dir() {
                    std::os::windows::fs::symlink_dir(team_source, &team_link)?;
                } else {
                    std::os::windows::fs::symlink_file(team_source, &team_link)?;
                }
            }

            Output::success(&format!("Team config linked as: {}", team_link.display()));
            Ok(ConflictResolution::TeamRenamed)
        }
        _ => unreachable!(),
    }
}

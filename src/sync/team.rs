use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

/// Tracks team-synced symlinks, conflict resolutions, and file preferences
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct TeamManifest {
    /// Symlinks created by team sync: team_name -> (target_path -> source_path)
    pub symlinks: HashMap<String, HashMap<String, String>>,
    /// Conflict resolutions: team_name -> (target_path -> resolution)
    pub conflicts: HashMap<String, HashMap<String, ConflictResolution>>,
    /// Patterns for files that are always local (never synced): team_name -> patterns
    /// e.g., ["*.local", "*.local.*", ".env.local"]
    #[serde(default)]
    pub local_patterns: HashMap<String, Vec<String>>,
    /// Files user has explicitly marked as personal (skip team sync): team_name -> file paths
    #[serde(default)]
    pub personal_files: HashMap<String, HashSet<String>>,
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
        crate::sync::atomic_write(&path, content.as_bytes())
            .context("Failed to write team manifest")
    }

    /// Get manifest file path
    fn manifest_path() -> Result<PathBuf> {
        let config_dir = crate::config::Config::config_dir()?;
        Ok(config_dir.join("team-manifest.json"))
    }

    /// Add a symlink to the manifest for a specific team
    pub fn add_symlink(&mut self, team_name: &str, target: PathBuf, source: PathBuf) {
        self.symlinks
            .entry(team_name.to_string())
            .or_default()
            .insert(
                target.to_string_lossy().to_string(),
                source.to_string_lossy().to_string(),
            );
    }

    /// Record a conflict resolution for a specific team
    pub fn add_conflict(
        &mut self,
        team_name: &str,
        target: PathBuf,
        resolution: ConflictResolution,
    ) {
        self.conflicts
            .entry(team_name.to_string())
            .or_default()
            .insert(target.to_string_lossy().to_string(), resolution);
    }

    /// Remove all symlinks and clean up manifest
    pub fn cleanup(&mut self) -> Result<()> {
        self.cleanup_team(None)
    }

    /// Remove symlinks for a specific team (or all if team_name is None)
    pub fn cleanup_team(&mut self, team_name: Option<&str>) -> Result<()> {
        let teams_to_clean: Vec<String> = match team_name {
            Some(name) => vec![name.to_string()],
            None => self.symlinks.keys().cloned().collect(),
        };

        for team in &teams_to_clean {
            if let Some(team_symlinks) = self.symlinks.get(team) {
                for target_str in team_symlinks.keys() {
                    let target = PathBuf::from(target_str);
                    if target.exists() && target.is_symlink() {
                        std::fs::remove_file(&target)
                            .with_context(|| format!("Failed to remove symlink: {}", target_str))?;
                    }
                }
            }

            // Clean up renamed personal files if they still have .personal extension
            if let Some(team_conflicts) = self.conflicts.get(team) {
                for (target_str, resolution) in team_conflicts {
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
            }

            // Remove team from manifest
            self.symlinks.remove(team);
            self.conflicts.remove(team);
        }

        self.save()?;
        Ok(())
    }

    /// Get local patterns for a team (with defaults)
    pub fn get_local_patterns(&self, team_name: &str) -> Vec<String> {
        self.local_patterns
            .get(team_name)
            .cloned()
            .unwrap_or_else(default_local_patterns)
    }

    /// Set local patterns for a team
    pub fn set_local_patterns(&mut self, team_name: &str, patterns: Vec<String>) {
        self.local_patterns.insert(team_name.to_string(), patterns);
    }

    /// Check if a file matches local patterns (should not be synced from team)
    pub fn is_local_file(&self, team_name: &str, filename: &str) -> bool {
        let patterns = self.get_local_patterns(team_name);
        is_local_file(filename, &patterns)
    }

    /// Check if user has marked a file as personal (skip team sync)
    pub fn is_personal_file(&self, team_name: &str, filepath: &str) -> bool {
        self.personal_files
            .get(team_name)
            .map(|files| files.contains(filepath))
            .unwrap_or(false)
    }

    /// Mark a file as personal (skip team sync)
    pub fn add_personal_file(&mut self, team_name: &str, filepath: &str) {
        self.personal_files
            .entry(team_name.to_string())
            .or_default()
            .insert(filepath.to_string());
    }

    /// Remove personal file marker (resume team sync)
    pub fn remove_personal_file(&mut self, team_name: &str, filepath: &str) {
        if let Some(files) = self.personal_files.get_mut(team_name) {
            files.remove(filepath);
        }
    }

    /// Get all personal files for a team
    pub fn get_personal_files(&self, team_name: &str) -> Vec<String> {
        self.personal_files
            .get(team_name)
            .map(|s| s.iter().cloned().collect())
            .unwrap_or_default()
    }
}

/// Default local patterns (files that are never synced from team)
pub fn default_local_patterns() -> Vec<String> {
    vec![
        "*.local".to_string(),
        "*.local.*".to_string(),
        ".env.local".to_string(),
        "appsettings.local.json".to_string(),
    ]
}

/// Check if a filename matches any local pattern
pub fn is_local_file(filename: &str, patterns: &[String]) -> bool {
    for pattern in patterns {
        if glob_match(pattern, filename) {
            return true;
        }
    }
    false
}

/// Simple glob matching for local file patterns
/// Supports: * (any chars), ? (single char)
/// Uses iterative approach with backtracking to avoid stack overflow
pub fn glob_match(pattern: &str, text: &str) -> bool {
    let p: Vec<char> = pattern.chars().collect();
    let t: Vec<char> = text.chars().collect();

    let mut pi = 0; // pattern index
    let mut ti = 0; // text index
    let mut star_pi = None; // position of last * in pattern
    let mut star_ti = 0; // text position when we hit last *

    while ti < t.len() {
        if pi < p.len() && (p[pi] == '?' || p[pi] == t[ti]) {
            // Match single char or ?
            pi += 1;
            ti += 1;
        } else if pi < p.len() && p[pi] == '*' {
            // Record star position for backtracking
            star_pi = Some(pi);
            star_ti = ti;
            pi += 1; // Try matching * with empty string first
        } else if let Some(sp) = star_pi {
            // Backtrack: * matches one more character
            pi = sp + 1;
            star_ti += 1;
            ti = star_ti;
        } else {
            return false;
        }
    }

    // Check remaining pattern is all *
    while pi < p.len() && p[pi] == '*' {
        pi += 1;
    }

    pi == p.len()
}

/// Discovers directories in team repo that should be symlinked
pub fn discover_symlinkable_dirs(team_sync_dir: &Path) -> Result<Vec<SymlinkableDir>> {
    let mut dirs = Vec::new();
    let home = crate::home_dir()?;

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
        team_name: &str,
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

            // Security: validate item name doesn't contain path traversal
            let item_name_str = item_name.to_string_lossy();
            if item_name_str.contains("..") || item_name_str.starts_with('/') {
                continue; // Skip unsafe paths
            }

            // Security: if team_item is a symlink, verify it points within the team repo
            if team_item.is_symlink() {
                if let Ok(link_target) = std::fs::read_link(&team_item) {
                    // Resolve the symlink target relative to team_path
                    let resolved = if link_target.is_absolute() {
                        link_target
                    } else {
                        self.team_path.join(&link_target)
                    };
                    // Canonicalize and check it's within team_path
                    if let Ok(canonical) = resolved.canonicalize() {
                        if let Ok(team_canonical) = self.team_path.canonicalize() {
                            if !canonical.starts_with(&team_canonical) {
                                // Symlink points outside team repo - skip it
                                continue;
                            }
                        }
                    } else {
                        // Can't resolve symlink - skip for safety
                        continue;
                    }
                }
            }

            let target_item = self.target_base.join(&item_name);

            // Check if target already exists
            if target_item.exists() && !target_item.is_symlink() {
                if auto_resolve {
                    // Skip conflicts in auto mode
                    manifest.add_conflict(
                        team_name,
                        target_item.clone(),
                        ConflictResolution::PersonalWins,
                    );
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

                super::create_symlink(&team_item, &target_item).with_context(|| {
                    format!(
                        "Failed to create symlink: {} -> {}",
                        target_item.display(),
                        team_item.display()
                    )
                })?;

                manifest.add_symlink(team_name, target_item.clone(), team_item);
                results.push(SymlinkResult::Created(target_item));
            }
        }

        Ok(results)
    }
}

/// Extract org name from Git URL
/// Examples:
/// - git@github.com:acme-corp/team-configs.git → "acme-corp"
/// - https://github.com/company/dotfiles.git → "company"
/// - git@gitlab.com:my-org/configs.git → "my-org"
pub fn extract_org_from_url(url: &str) -> Option<String> {
    // Try HTTPS format first: https://host/org/repo.git
    if url.starts_with("http://") || url.starts_with("https://") {
        let parts: Vec<&str> = url.split('/').collect();
        if parts.len() >= 5 {
            // https://github.com/company/repo.git -> parts = ["https:", "", "github.com", "company", "repo.git"]
            return Some(parts[3].to_string());
        }
        return None;
    }

    // Try SSH format: git@host:org/repo.git
    if let Some(after_colon) = url.split(':').nth(1) {
        if let Some(org) = after_colon.split('/').next() {
            return Some(org.to_string());
        }
    }

    None
}

/// Alias for backwards compatibility
pub fn extract_team_name_from_url(url: &str) -> Option<String> {
    extract_org_from_url(url)
}

/// Get the git remote origin URL for a project directory
pub fn get_project_remote_url(project_path: &Path) -> Option<String> {
    use std::process::Command;

    let output = Command::new("git")
        .args(["remote", "get-url", "origin"])
        .current_dir(project_path)
        .output()
        .ok()?;

    if output.status.success() {
        Some(String::from_utf8_lossy(&output.stdout).trim().to_string())
    } else {
        None
    }
}

/// Get the org from a project's git remote
pub fn get_project_org(project_path: &Path) -> Option<String> {
    get_project_remote_url(project_path).and_then(|url| extract_org_from_url(&url))
}

/// Check if a project belongs to any of the allowed orgs for a team
pub fn project_matches_team_orgs(project_path: &Path, allowed_orgs: &[String]) -> bool {
    if allowed_orgs.is_empty() {
        return false; // No orgs configured = no project sync
    }

    match get_project_org(project_path) {
        Some(org) => allowed_orgs
            .iter()
            .any(|allowed| allowed.eq_ignore_ascii_case(&org)),
        None => false, // Can't determine org = no match
    }
}

/// Find which team owns a project based on its normalized URL
/// Returns the team name if found, None otherwise
///
/// The normalized URL format is "host/org/repo" (e.g., "github.com/acme-corp/api")
/// Team orgs are stored as "host/org" (e.g., "github.com/acme-corp")
pub fn find_team_for_project(
    normalized_url: &str,
    teams: &std::collections::HashMap<String, crate::config::TeamConfig>,
) -> Option<String> {
    let project_org = crate::sync::extract_org_from_normalized_url(normalized_url)?;

    for (team_name, team_config) in teams {
        if !team_config.enabled {
            continue;
        }
        for org in &team_config.orgs {
            if org.eq_ignore_ascii_case(&project_org) {
                return Some(team_name.clone());
            }
        }
    }

    None
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
            super::create_symlink(team_source, target)
                .context("Failed to create symlink after renaming personal")?;

            Output::success(&format!(
                "Personal file renamed to: {}",
                personal_backup.display()
            ));
            Ok(ConflictResolution::PersonalRenamed)
        }
        2 => {
            // Create team symlink with .team suffix
            let team_link = target.with_extension("team");

            super::create_symlink(team_source, &team_link)
                .context("Failed to create team symlink")?;

            Output::success(&format!("Team config linked as: {}", team_link.display()));
            Ok(ConflictResolution::TeamRenamed)
        }
        _ => unreachable!(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_glob_match_star() {
        assert!(glob_match("*.local", ".env.local"));
        assert!(glob_match("*.local", "config.local"));
        assert!(!glob_match("*.local", ".env.local.bak"));
    }

    #[test]
    fn test_glob_match_star_middle() {
        assert!(glob_match("*.local.*", "config.local.json"));
        assert!(glob_match("*.local.*", "appsettings.local.json"));
        assert!(!glob_match("*.local.*", ".env.local"));
    }

    #[test]
    fn test_glob_match_exact() {
        assert!(glob_match(".env.local", ".env.local"));
        assert!(!glob_match(".env.local", ".env.local.bak"));
        assert!(!glob_match(".env.local", "env.local"));
    }

    #[test]
    fn test_glob_match_question() {
        assert!(glob_match("file?.txt", "file1.txt"));
        assert!(glob_match("file?.txt", "fileA.txt"));
        assert!(!glob_match("file?.txt", "file12.txt"));
    }

    #[test]
    fn test_glob_match_double_star() {
        // Double * pattern like *service-account*.json
        assert!(glob_match(
            "*service-account*.json",
            "my-service-account-prod.json"
        ));
        assert!(glob_match("*service-account*.json", "service-account.json"));
        assert!(glob_match(
            "*service-account*.json",
            "gcp-service-account-dev.json"
        ));
        assert!(!glob_match(
            "*service-account*.json",
            "service-account.yaml"
        ));
    }

    #[test]
    fn test_glob_match_env_patterns() {
        // .env* should match all env files
        assert!(glob_match(".env*", ".env"));
        assert!(glob_match(".env*", ".env.local"));
        assert!(glob_match(".env*", ".env.development"));
        assert!(glob_match(".env*", ".env.production.local"));
        assert!(!glob_match(".env*", "env"));
    }

    #[test]
    fn test_is_local_file_default_patterns() {
        let patterns = default_local_patterns();
        assert!(is_local_file(".env.local", &patterns));
        assert!(is_local_file("config.local", &patterns));
        assert!(is_local_file("appsettings.local.json", &patterns));
        assert!(!is_local_file(".env.development", &patterns));
        assert!(!is_local_file("appsettings.json", &patterns));
    }

    #[test]
    fn test_extract_org_ssh() {
        assert_eq!(
            extract_org_from_url("git@github.com:acme-corp/repo.git"),
            Some("acme-corp".to_string())
        );
        assert_eq!(
            extract_org_from_url("git@gitlab.com:my-org/project.git"),
            Some("my-org".to_string())
        );
    }

    #[test]
    fn test_extract_org_https() {
        assert_eq!(
            extract_org_from_url("https://github.com/company/repo.git"),
            Some("company".to_string())
        );
        assert_eq!(
            extract_org_from_url("https://gitlab.com/org-name/project.git"),
            Some("org-name".to_string())
        );
    }

    #[test]
    fn test_find_team_for_project() {
        use std::collections::HashMap;

        let mut teams = HashMap::new();
        teams.insert(
            "acme".to_string(),
            crate::config::TeamConfig {
                enabled: true,
                url: "git@github.com:acme-corp/configs.git".to_string(),
                auto_inject: false,
                read_only: true,
                orgs: vec![
                    "github.com/acme-corp".to_string(),
                    "github.com/acme-inc".to_string(),
                ],
            },
        );
        teams.insert(
            "personal".to_string(),
            crate::config::TeamConfig {
                enabled: true,
                url: "git@github.com:user/dotfiles.git".to_string(),
                auto_inject: false,
                read_only: false,
                orgs: vec!["github.com/user".to_string()],
            },
        );

        // Match acme-corp
        assert_eq!(
            find_team_for_project("github.com/acme-corp/api", &teams),
            Some("acme".to_string())
        );

        // Match acme-inc (alias)
        assert_eq!(
            find_team_for_project("github.com/acme-inc/dashboard", &teams),
            Some("acme".to_string())
        );

        // Match personal
        assert_eq!(
            find_team_for_project("github.com/user/myproject", &teams),
            Some("personal".to_string())
        );

        // No match
        assert_eq!(
            find_team_for_project("github.com/other-org/repo", &teams),
            None
        );

        // Case insensitive
        assert_eq!(
            find_team_for_project("github.com/ACME-CORP/api", &teams),
            Some("acme".to_string())
        );
    }
}

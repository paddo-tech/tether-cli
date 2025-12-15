use anyhow::{Context, Result};
use std::fs;
use std::path::Path;

/// File types and how to handle them
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum FileType {
    Toml,      // Deep merge (no include support)
    Json,      // Deep merge (no include support)
    Shell,     // Use source directive
    GitConfig, // Use [include] directive
    Unknown,   // Skip - warn user
}

/// Detect file type from path
pub fn detect_file_type(path: &Path) -> FileType {
    let extension = path.extension().and_then(|e| e.to_str());
    let filename = path.file_name().and_then(|n| n.to_str()).unwrap_or("");

    // Check extension first
    match extension {
        Some("toml") => return FileType::Toml,
        Some("json") => return FileType::Json,
        _ => {}
    }

    // GitConfig - uses [include] directive
    if filename == ".gitconfig"
        || filename.ends_with("gitconfig")
        || filename == "team.gitconfig"
    {
        return FileType::GitConfig;
    }

    // Shell files - use source directive
    if filename == ".zshrc"
        || filename == ".bashrc"
        || filename == ".bash_profile"
        || filename == ".profile"
        || filename == ".zprofile"
        || filename == ".zshenv"
        || filename.ends_with("rc")
        || filename.ends_with("profile")
        || filename.starts_with("team.") && (filename.ends_with("rc") || filename.ends_with("profile"))
    {
        return FileType::Shell;
    }

    FileType::Unknown
}

/// Merge two files: team (base) + personal (overlay)
/// Only for file types that don't support includes (TOML, JSON)
/// Personal wins on key conflicts
pub fn merge_files(team_path: &Path, personal_path: &Path) -> Result<String> {
    let file_type = detect_file_type(personal_path);

    let team_content = fs::read_to_string(team_path)
        .with_context(|| format!("Failed to read team file: {}", team_path.display()))?;
    let personal_content = fs::read_to_string(personal_path)
        .with_context(|| format!("Failed to read personal file: {}", personal_path.display()))?;

    match file_type {
        FileType::Toml => merge_toml(&team_content, &personal_content),
        FileType::Json => merge_json(&team_content, &personal_content),
        FileType::Shell | FileType::GitConfig | FileType::Unknown => {
            Err(anyhow::anyhow!(
                "File type {:?} should use source/include, not merge",
                file_type
            ))
        }
    }
}

/// Deep merge TOML: personal keys override team keys
fn merge_toml(team: &str, personal: &str) -> Result<String> {
    let team_val: toml::Value = toml::from_str(team).context("Invalid team TOML")?;
    let personal_val: toml::Value = toml::from_str(personal).context("Invalid personal TOML")?;

    let merged = deep_merge_toml(team_val, personal_val);
    toml::to_string_pretty(&merged).context("Failed to serialize merged TOML")
}

fn deep_merge_toml(base: toml::Value, overlay: toml::Value) -> toml::Value {
    match (base, overlay) {
        (toml::Value::Table(mut base_map), toml::Value::Table(overlay_map)) => {
            for (key, overlay_val) in overlay_map {
                match base_map.remove(&key) {
                    Some(base_val) => {
                        base_map.insert(key, deep_merge_toml(base_val, overlay_val));
                    }
                    None => {
                        base_map.insert(key, overlay_val);
                    }
                }
            }
            toml::Value::Table(base_map)
        }
        // For non-tables: overlay (personal) always wins
        (_, overlay) => overlay,
    }
}

/// Deep merge JSON: personal keys override team keys
fn merge_json(team: &str, personal: &str) -> Result<String> {
    let team_val: serde_json::Value = serde_json::from_str(team).context("Invalid team JSON")?;
    let personal_val: serde_json::Value =
        serde_json::from_str(personal).context("Invalid personal JSON")?;

    let merged = deep_merge_json(team_val, personal_val);
    serde_json::to_string_pretty(&merged).context("Failed to serialize merged JSON")
}

fn deep_merge_json(base: serde_json::Value, overlay: serde_json::Value) -> serde_json::Value {
    match (base, overlay) {
        (serde_json::Value::Object(mut base_map), serde_json::Value::Object(overlay_map)) => {
            for (key, overlay_val) in overlay_map {
                match base_map.remove(&key) {
                    Some(base_val) => {
                        base_map.insert(key, deep_merge_json(base_val, overlay_val));
                    }
                    None => {
                        base_map.insert(key, overlay_val);
                    }
                }
            }
            serde_json::Value::Object(base_map)
        }
        // For non-objects: overlay (personal) always wins
        (_, overlay) => overlay,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_detect_file_type() {
        assert_eq!(
            detect_file_type(Path::new("config.toml")),
            FileType::Toml
        );
        assert_eq!(
            detect_file_type(Path::new("settings.json")),
            FileType::Json
        );
        assert_eq!(
            detect_file_type(Path::new(".gitconfig")),
            FileType::GitConfig
        );
        assert_eq!(
            detect_file_type(Path::new(".zshrc")),
            FileType::Shell
        );
        assert_eq!(
            detect_file_type(Path::new(".bashrc")),
            FileType::Shell
        );
        assert_eq!(
            detect_file_type(Path::new("random.txt")),
            FileType::Unknown
        );
    }

    #[test]
    fn test_merge_toml_deep() {
        let team = r#"
[build]
jobs = 4

[alias]
t = "test"
b = "build"
"#;
        let personal = r#"
[build]
jobs = 8
target = "release"

[alias]
t = "test --release"
"#;
        let merged = merge_toml(team, personal).unwrap();

        // personal values should win
        assert!(merged.contains("jobs = 8"));
        // personal additions
        assert!(merged.contains("target = \"release\""));
        // personal override
        assert!(merged.contains("t = \"test --release\""));
        // team value preserved if not in personal
        assert!(merged.contains("b = \"build\""));
    }

    #[test]
    fn test_merge_json_deep() {
        let team = r#"{"a": 1, "b": {"x": 10, "y": 20}}"#;
        let personal = r#"{"a": 2, "b": {"x": 15}, "c": 3}"#;
        let merged = merge_json(team, personal).unwrap();

        let val: serde_json::Value = serde_json::from_str(&merged).unwrap();
        assert_eq!(val["a"], 2); // personal wins
        assert_eq!(val["b"]["x"], 15); // personal wins
        assert_eq!(val["b"]["y"], 20); // team preserved
        assert_eq!(val["c"], 3); // personal addition
    }

}

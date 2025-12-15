use anyhow::{Context, Result};
use std::fs;
use std::path::Path;

/// File types we can merge intelligently
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum FileType {
    Toml,
    Json,
    Ini,      // gitconfig, .ini, .cfg
    Plain,    // Shell scripts, unknown files - concatenate
}

/// Detect file type from path
pub fn detect_file_type(path: &Path) -> FileType {
    let extension = path.extension().and_then(|e| e.to_str());
    let filename = path.file_name().and_then(|n| n.to_str()).unwrap_or("");

    // Check extension first
    match extension {
        Some("toml") => return FileType::Toml,
        Some("json") => return FileType::Json,
        Some("ini") | Some("cfg") => return FileType::Ini,
        _ => {}
    }

    // Check filename patterns
    if filename == ".gitconfig"
        || filename.ends_with("gitconfig")
        || filename == ".gitignore_global"
    {
        return FileType::Ini;
    }

    FileType::Plain
}

/// Merge two files: team (base) + personal (overlay)
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
        FileType::Ini => merge_ini(&team_content, &personal_content),
        FileType::Plain => Ok(merge_plain(&team_content, &personal_content)),
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

/// Merge INI/gitconfig: section-level merge, personal sections win
/// Format: [section] or [section "subsection"] followed by key = value
fn merge_ini(team: &str, personal: &str) -> Result<String> {
    let team_sections = parse_ini(team);
    let personal_sections = parse_ini(personal);

    let mut merged = team_sections;

    // Personal sections override team sections (at section level, not key level for simplicity)
    // For gitconfig, users typically want their entire section to override
    for (section, entries) in personal_sections {
        merged.insert(section, entries);
    }

    Ok(serialize_ini(&merged))
}

/// Parse INI-style content into sections
fn parse_ini(content: &str) -> std::collections::HashMap<String, Vec<String>> {
    let mut sections = std::collections::HashMap::new();
    let mut current_section = String::new();
    let mut current_entries = Vec::new();

    for line in content.lines() {
        let trimmed = line.trim();

        if trimmed.starts_with('[') && trimmed.ends_with(']') {
            // Save previous section
            if !current_section.is_empty() || !current_entries.is_empty() {
                sections.insert(current_section.clone(), current_entries);
            }
            current_section = trimmed.to_string();
            current_entries = Vec::new();
        } else {
            current_entries.push(line.to_string());
        }
    }

    // Save last section
    if !current_section.is_empty() || !current_entries.is_empty() {
        sections.insert(current_section, current_entries);
    }

    sections
}

/// Serialize INI sections back to string
fn serialize_ini(sections: &std::collections::HashMap<String, Vec<String>>) -> String {
    let mut result = Vec::new();

    // Handle entries before any section header (e.g., comments at top)
    if let Some(entries) = sections.get("") {
        for entry in entries {
            result.push(entry.clone());
        }
    }

    // Sort sections for consistent output
    let mut section_names: Vec<_> = sections.keys().filter(|k| !k.is_empty()).collect();
    section_names.sort();

    for section in section_names {
        if !result.is_empty() {
            result.push(String::new()); // Blank line between sections
        }
        result.push(section.clone());
        if let Some(entries) = sections.get(section) {
            for entry in entries {
                result.push(entry.clone());
            }
        }
    }

    result.join("\n")
}

/// Merge plain text files by concatenation
/// Team content first, then personal content (personal overrides if both define same things)
fn merge_plain(team: &str, personal: &str) -> String {
    let separator = "\n# --- Personal config below (overrides team defaults) ---\n";
    format!("{}{}{}", team.trim_end(), separator, personal)
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
            FileType::Ini
        );
        assert_eq!(
            detect_file_type(Path::new(".zshrc")),
            FileType::Plain
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

    #[test]
    fn test_merge_ini() {
        let team = r#"
[user]
    name = Team Name
    email = team@example.com

[alias]
    st = status
"#;
        let personal = r#"
[user]
    name = Personal Name

[core]
    editor = vim
"#;
        let merged = merge_ini(team, personal).unwrap();

        // personal section replaces team section entirely
        assert!(merged.contains("name = Personal Name"));
        assert!(!merged.contains("email = team@example.com")); // Gone because [user] replaced
        // team sections preserved if not in personal
        assert!(merged.contains("st = status"));
        // personal additions
        assert!(merged.contains("editor = vim"));
    }

    #[test]
    fn test_merge_plain() {
        let team = "export TEAM_VAR=1";
        let personal = "export PERSONAL_VAR=2";
        let merged = merge_plain(team, personal);

        assert!(merged.contains("TEAM_VAR=1"));
        assert!(merged.contains("PERSONAL_VAR=2"));
        assert!(merged.contains("# --- Personal config below"));
    }
}

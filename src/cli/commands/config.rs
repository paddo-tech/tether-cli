use crate::cli::{Output, Prompt};
use crate::config::{Config, DotfileEntry, FeaturesConfig};
use anyhow::Result;
use comfy_table::{presets::UTF8_FULL, Attribute, Cell, Color};
use inquire::Select as InquireSelect;

pub async fn get(key: &str) -> Result<()> {
    let config = Config::load()?;
    let config_toml = toml::to_string_pretty(&config)?;
    let value = toml::from_str::<toml::Value>(&config_toml)?;

    // Parse nested key (e.g., "project_configs.enabled")
    let keys: Vec<&str> = key.split('.').collect();
    let mut current = &value;

    for k in &keys {
        match current.get(k) {
            Some(v) => current = v,
            None => anyhow::bail!("Key '{}' not found in config", key),
        }
    }

    // Pretty print the value
    match current {
        toml::Value::String(s) => println!("{}", s),
        toml::Value::Integer(i) => println!("{}", i),
        toml::Value::Float(f) => println!("{}", f),
        toml::Value::Boolean(b) => println!("{}", b),
        toml::Value::Array(arr) => {
            println!("[");
            for item in arr {
                println!("  {},", toml::to_string(item)?.trim());
            }
            println!("]");
        }
        toml::Value::Table(_) => {
            println!("{}", toml::to_string_pretty(current)?);
        }
        toml::Value::Datetime(dt) => println!("{}", dt),
    }

    Ok(())
}

pub async fn set(key: &str, value: &str) -> Result<()> {
    let config = Config::load()?;
    set_key(&config, key, parse_value(value)?)?.save()?;
    Output::success(&format!("Set {} = {}", key, value));
    Ok(())
}

/// Read `value` as a TOML value, such as `true`, `7` or `["a/b"]`. A bare word that is not
/// TOML, such as `mocha`, is a string. A value that starts like an array, table or quoted
/// string must parse.
fn parse_value(value: &str) -> Result<toml::Value> {
    match toml::from_str::<toml::Table>(&format!("v = {}", value)) {
        Ok(mut table) => Ok(table.remove("v").expect("the table has v")),
        Err(e) if value.trim_start().starts_with(['[', '{', '"', '\'']) => {
            anyhow::bail!("Cannot read {} as a TOML value: {}", value, e)
        }
        Err(_) => Ok(toml::Value::String(value.to_string())),
    }
}

/// Tables at their defaults are not serialized, so missing tables on the path are created.
/// `Config` ignores keys it does not know, so a key that does not survive the round trip
/// through `Config` is unknown, unless it is a known key at a value serialization skips. A
/// known key rejects a datetime, and an unknown key accepts anything.
fn set_key(config: &Config, key: &str, new_value: toml::Value) -> Result<Config> {
    let keys: Vec<&str> = key.split('.').collect();
    let updated: Config = with_key(config, key, new_value.clone())?.try_into()?;
    let check = toml::Value::try_from(&updated)?;
    if keys.iter().try_fold(&check, |v, k| v.get(k)) == Some(&new_value) {
        return Ok(updated);
    }
    let probe = toml::Value::Datetime("1979-05-27T07:32:00Z".parse()?);
    if with_key(config, key, probe)?.try_into::<Config>().is_ok() {
        anyhow::bail!("Unknown config key '{}'", key);
    }
    Ok(updated)
}

/// `config` as TOML with `key` set to `value`.
fn with_key(config: &Config, key: &str, value: toml::Value) -> Result<toml::Value> {
    let mut toml_value = toml::Value::try_from(config)?;
    let keys: Vec<&str> = key.split('.').collect();
    let mut current = &mut toml_value;
    for k in &keys[..keys.len() - 1] {
        let Some(table) = current.as_table_mut() else {
            anyhow::bail!("Cannot set value at '{}'", key);
        };
        current = table
            .entry(k.to_string())
            .or_insert_with(|| toml::Value::Table(toml::map::Map::new()));
    }
    let Some(table) = current.as_table_mut() else {
        anyhow::bail!("Cannot set value at '{}'", key);
    };
    table.insert(keys[keys.len() - 1].to_string(), value);
    Ok(toml_value)
}

pub async fn edit() -> Result<()> {
    if !Prompt::is_interactive() {
        anyhow::bail!(
            "tether config edit needs a terminal. Use 'tether config set <key> <value>' instead"
        );
    }
    let config_path = Config::config_path()?;

    // Get editor from environment or use default
    let editor = std::env::var("EDITOR").unwrap_or_else(|_| {
        if cfg!(target_os = "macos") {
            "nano".to_string()
        } else {
            "vi".to_string()
        }
    });

    Output::info(&format!("Opening config in {}...", editor));
    Output::info(&format!("File: {}", config_path.display()));

    // Open editor
    let status = std::process::Command::new(&editor)
        .arg(&config_path)
        .status()?;

    if status.success() {
        // Validate the config by trying to load it
        match Config::load() {
            Ok(_) => {
                Output::success("Config updated successfully");
            }
            Err(e) => anyhow::bail!(
                "Config validation failed: {}. Your changes were saved but contain errors",
                e
            ),
        }
    } else {
        anyhow::bail!("Editor exited with error");
    }

    Ok(())
}

pub async fn dotfiles() -> Result<()> {
    let mut config = Config::load()?;
    let mut cursor = 0usize;

    loop {
        Output::header("Sync Configuration");

        // Section 1: Home directory dotfiles
        println!();
        Output::subheader("Home Directory (~/)");
        render_dotfile_table("Files", &config.dotfiles.files);
        render_entry_table("Folders", &config.dotfiles.dirs);

        // Section 2: Project configs
        println!();
        let status = if config.project_configs.enabled {
            "enabled"
        } else {
            "disabled"
        };
        Output::subheader(&format!("Project Configs ({})", status));
        render_entry_table("Search Paths", &config.project_configs.search_paths);
        render_entry_table("File Patterns", &config.project_configs.patterns);

        let options = vec![
            "Dotfiles",
            "Dotfile Folders",
            "Project Search Paths",
            "Project File Patterns",
            "Toggle Project Scanning",
            "Done",
        ];
        let choice = Prompt::select(
            "Select section",
            options.clone(),
            cursor.min(options.len() - 1),
        )?;
        cursor = choice;

        let changed = match choice {
            0 => Some(manage_dotfile_list(
                "Dotfiles",
                "file path (e.g., .zshrc)",
                &mut config.dotfiles.files,
            )?),
            1 => Some(manage_entry_list(
                "Dotfile Folders",
                "folder path (e.g., .config/nvim)",
                &mut config.dotfiles.dirs,
            )?),
            2 => Some(manage_entry_list(
                "Project Search Paths",
                "path (e.g., ~/Projects)",
                &mut config.project_configs.search_paths,
            )?),
            3 => Some(manage_entry_list(
                "Project File Patterns",
                "pattern (e.g., .env.local)",
                &mut config.project_configs.patterns,
            )?),
            4 => {
                config.project_configs.enabled = !config.project_configs.enabled;
                let state = if config.project_configs.enabled {
                    "enabled"
                } else {
                    "disabled"
                };
                Output::success(&format!("Project config scanning {}", state));
                Some(true)
            }
            _ => None,
        };

        if let Some(should_save) = changed {
            if should_save {
                config.save()?;
                Output::success("Configuration updated");
            }
        } else {
            break;
        }
    }

    Ok(())
}

fn manage_entry_list(title: &str, prompt_label: &str, entries: &mut Vec<String>) -> Result<bool> {
    let mut changed = false;
    loop {
        println!();
        render_entry_table(title, entries);
        let actions = vec!["Add", "Remove", "Back"];
        let choice = Prompt::select(&format!("{} - select an action", title), actions.clone(), 0)?;

        match choice {
            0 => {
                let input = Prompt::input(&format!("Enter {}", prompt_label), None)?;
                let value = input.trim();
                if value.is_empty() {
                    Output::warning("Value cannot be empty");
                    continue;
                }
                if entries.iter().any(|item| item == value) {
                    Output::warning("Already tracked");
                    continue;
                }
                entries.push(value.to_string());
                normalize_entries(entries);
                changed = true;
                Output::success(&format!("Added {}", value));
            }
            1 => {
                if entries.is_empty() {
                    Output::info("Nothing to remove");
                    continue;
                }

                let selection = InquireSelect::new(
                    &format!("Select {} to remove", title.to_lowercase()),
                    entries.clone(),
                )
                .prompt()?;

                entries.retain(|item| item != &selection);
                changed = true;
                Output::success(&format!("Removed {}", selection));
            }
            _ => break,
        }
    }

    Ok(changed)
}

fn render_entry_table(title: &str, entries: &[String]) {
    use crate::cli::output::Colorize;

    if entries.is_empty() {
        println!("{}", format!("{}: (none)", title).bright_black());
        return;
    }

    let mut table = Output::table();
    table.load_preset(UTF8_FULL).set_header(vec![
        Cell::new(format!("{} ({})", title, entries.len()))
            .add_attribute(Attribute::Bold)
            .fg(Color::Cyan),
        Cell::new("Entry")
            .add_attribute(Attribute::Bold)
            .fg(Color::Cyan),
    ]);

    for (idx, entry) in entries.iter().enumerate() {
        table.add_row(vec![
            Cell::new(format!("#{}", idx + 1)).fg(Color::Green),
            Cell::new(entry),
        ]);
    }

    println!("{table}");
}

fn render_dotfile_table(title: &str, entries: &[DotfileEntry]) {
    use crate::cli::output::Colorize;

    if entries.is_empty() {
        println!("{}", format!("{}: (none)", title).bright_black());
        return;
    }

    let mut table = Output::table();
    table.load_preset(UTF8_FULL).set_header(vec![
        Cell::new(format!("{} ({})", title, entries.len()))
            .add_attribute(Attribute::Bold)
            .fg(Color::Cyan),
        Cell::new("Path")
            .add_attribute(Attribute::Bold)
            .fg(Color::Cyan),
        Cell::new("Create if Missing")
            .add_attribute(Attribute::Bold)
            .fg(Color::Cyan),
    ]);

    for (idx, entry) in entries.iter().enumerate() {
        let create_flag = if entry.create_if_missing() {
            "yes"
        } else {
            "no"
        };
        table.add_row(vec![
            Cell::new(format!("#{}", idx + 1)).fg(Color::Green),
            Cell::new(entry.path()),
            Cell::new(create_flag).fg(if entry.create_if_missing() {
                Color::Green
            } else {
                Color::Yellow
            }),
        ]);
    }

    println!("{table}");
}

fn normalize_entries(entries: &mut Vec<String>) {
    entries.iter_mut().for_each(|entry| {
        *entry = entry.trim().to_string();
    });
    entries.retain(|entry| !entry.is_empty());
    entries.sort();
    entries.dedup();
}

fn manage_dotfile_list(
    title: &str,
    prompt_label: &str,
    entries: &mut Vec<DotfileEntry>,
) -> Result<bool> {
    let mut changed = false;
    loop {
        println!();
        render_dotfile_table(title, entries);
        let actions = vec!["Add", "Remove", "Toggle create_if_missing", "Back"];
        let choice = Prompt::select(&format!("{} - select an action", title), actions.clone(), 0)?;

        match choice {
            0 => {
                let input = Prompt::input(&format!("Enter {}", prompt_label), None)?;
                let value = input.trim();
                if value.is_empty() {
                    Output::warning("Value cannot be empty");
                    continue;
                }
                if entries.iter().any(|e| e.path() == value) {
                    Output::warning("Already tracked");
                    continue;
                }
                let create = Prompt::confirm("Create if missing on other machines?", true)?;
                if create {
                    entries.push(DotfileEntry::Simple(value.to_string()));
                } else {
                    entries.push(DotfileEntry::WithOptions {
                        path: value.to_string(),
                        create_if_missing: false,
                        on_conflict: Default::default(),
                    });
                }
                entries.sort_by(|a, b| a.path().cmp(b.path()));
                changed = true;
                Output::success(&format!("Added {}", value));
            }
            1 => {
                if entries.is_empty() {
                    Output::info("Nothing to remove");
                    continue;
                }

                let paths: Vec<String> = entries.iter().map(|e| e.path().to_string()).collect();
                let selection = InquireSelect::new(
                    &format!("Select {} to remove", title.to_lowercase()),
                    paths,
                )
                .prompt()?;

                entries.retain(|e| e.path() != selection);
                changed = true;
                Output::success(&format!("Removed {}", selection));
            }
            2 => {
                if entries.is_empty() {
                    Output::info("Nothing to toggle");
                    continue;
                }

                let paths: Vec<String> = entries.iter().map(|e| e.path().to_string()).collect();
                let selection =
                    InquireSelect::new("Select file to toggle create_if_missing", paths)
                        .prompt()?;

                if let Some(entry) = entries.iter_mut().find(|e| e.path() == selection) {
                    let new_value = !entry.create_if_missing();
                    *entry = DotfileEntry::WithOptions {
                        path: selection.clone(),
                        create_if_missing: new_value,
                        on_conflict: entry.on_conflict(),
                    };
                    changed = true;
                    Output::success(&format!("{}: create_if_missing = {}", selection, new_value));
                }
            }
            _ => break,
        }
    }

    Ok(changed)
}

/// List all features and their status
pub async fn features_list() -> Result<()> {
    use crate::cli::output::Colorize;

    let config = Config::load()?;

    Output::header("Feature Toggles");
    println!();

    let features = [
        (
            "personal_dotfiles",
            config.features.personal_dotfiles,
            "Sync shell configs (.zshrc, .gitconfig)",
        ),
        (
            "personal_packages",
            config.features.personal_packages,
            "Sync packages (brew, npm, etc.)",
        ),
        (
            "team_dotfiles",
            config.features.team_dotfiles,
            "Sync org-based team dotfiles",
        ),
        (
            "collab_secrets",
            config.features.collab_secrets,
            "Share project secrets with collaborators",
        ),
        (
            "team_layering",
            config.features.team_layering,
            "Merge team + personal dotfiles (experimental)",
        ),
    ];

    for (name, enabled, desc) in features {
        if enabled {
            Output::key_value_colored(name, "enabled", |v| v.green().to_string());
        } else {
            Output::key_value_colored(name, "disabled", |v| v.dimmed().to_string());
        }
        println!("    {}", desc.dimmed());
    }

    println!();
    Output::dim("Enable/disable: tether config features <enable|disable> <feature>");

    Ok(())
}

/// Enable a feature
pub async fn features_enable(feature: &str) -> Result<()> {
    let mut config = Config::load()?;

    set_feature(&mut config.features, feature, true)?;
    config.save()?;
    Output::success(&format!("Enabled {}", feature));
    show_feature_guidance(feature, true);
    Ok(())
}

/// Disable a feature
pub async fn features_disable(feature: &str) -> Result<()> {
    let mut config = Config::load()?;

    set_feature(&mut config.features, feature, false)?;
    config.save()?;
    Output::success(&format!("Disabled {}", feature));
    Ok(())
}

fn set_feature(features: &mut FeaturesConfig, name: &str, enabled: bool) -> Result<()> {
    match name {
        "personal_dotfiles" => features.personal_dotfiles = enabled,
        "personal_packages" => features.personal_packages = enabled,
        "team_dotfiles" => features.team_dotfiles = enabled,
        "collab_secrets" => features.collab_secrets = enabled,
        "team_layering" => {
            if enabled {
                Output::warning("team_layering is experimental and may have issues");
            }
            features.team_layering = enabled;
        }
        _ => return Err(anyhow::anyhow!("Unknown feature: {}. Valid: personal_dotfiles, personal_packages, team_dotfiles, collab_secrets, team_layering", name)),
    }
    Ok(())
}

fn show_feature_guidance(feature: &str, enabled: bool) {
    if !enabled {
        return;
    }

    let config = Config::load().ok();

    match feature {
        "team_dotfiles" => {
            // Check if team is configured
            let has_team = config
                .as_ref()
                .and_then(|c| c.teams.as_ref())
                .map(|t| !t.active.is_empty())
                .unwrap_or(false);

            if !has_team {
                println!();
                Output::warning("No team configured yet");
                Output::info("Set up your team:");
                println!("  tether team setup");
            }
        }
        "collab_secrets" => {
            println!();
            Output::info("In a project directory:");
            println!("  tether collab init");
        }
        "team_layering" => {
            // Check dependencies
            let team_enabled = config
                .as_ref()
                .map(|c| c.features.team_dotfiles)
                .unwrap_or(false);

            if !team_enabled {
                println!();
                Output::warning("team_layering requires team_dotfiles to be enabled");
            }
        }
        "personal_dotfiles" | "personal_packages"
            if config
                .as_ref()
                .map(|c| c.backend.url.is_empty())
                .unwrap_or(true) =>
        {
            println!();
            Output::info("Set up personal sync repo:");
            println!("  tether init");
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_set_key_creates_default_dashboard_table() {
        let config = set_key(
            &Config::default(),
            "dashboard.theme",
            toml::Value::String("mocha".into()),
        )
        .unwrap();
        assert_eq!(config.dashboard.theme.as_deref(), Some("mocha"));
    }

    #[test]
    fn test_set_key_rejects_unknown_key() {
        let value = toml::Value::Boolean(true);
        assert!(set_key(&Config::default(), "nope.enabled", value.clone()).is_err());
        assert!(set_key(&Config::default(), "dashboard.nope", value).is_err());
        // An existing table ignores an unknown key as well
        let five = toml::Value::Integer(5);
        assert!(set_key(&Config::default(), "sync.nope", five).is_err());
        let interval = toml::Value::String("10m".into());
        assert!(set_key(&Config::default(), "sync.interval", interval).is_ok());
    }

    #[test]
    fn values_parse_as_toml() {
        use toml::Value;
        assert_eq!(parse_value("true").unwrap(), Value::Boolean(true));
        assert_eq!(parse_value("7").unwrap(), Value::Integer(7));
        assert_eq!(parse_value("mocha").unwrap(), Value::String("mocha".into()));
        assert_eq!(parse_value("\"7\"").unwrap(), Value::String("7".into()));
        assert_eq!(parse_value("1.2.3").unwrap(), Value::String("1.2.3".into()));
        let taps = parse_value(r#"["azure/kubelogin"]"#).unwrap();
        assert_eq!(
            taps,
            Value::Array(vec![Value::String("azure/kubelogin".into())])
        );
        let config = set_key(&Config::default(), "packages.brew.trusted_taps", taps).unwrap();
        assert_eq!(config.packages.brew.trusted_taps, ["azure/kubelogin"]);
        assert!(parse_value("[\"unclosed\"").is_err());
        // A string where the config wants an array is an error, not a silent no-op
        let wrong = parse_value("azure/kubelogin").unwrap();
        assert!(set_key(&Config::default(), "packages.brew.trusted_taps", wrong).is_err());
    }

    #[test]
    fn test_set_key_accepts_existing_key_at_skipped_default() {
        let empty = toml::Value::Array(Vec::new());
        let config = set_key(&Config::default(), "packages.allow_scripts", empty).unwrap();
        assert!(config.packages.allow_scripts.is_empty());
    }
}

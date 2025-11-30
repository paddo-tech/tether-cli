use crate::cli::{Output, Prompt};
use crate::config::Config;
use anyhow::Result;
use comfy_table::{presets::UTF8_FULL, Attribute, Cell, Color, Table};
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
            None => {
                Output::error(&format!("Key '{}' not found in config", key));
                return Ok(());
            }
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
        _ => println!("{:?}", current),
    }

    Ok(())
}

pub async fn set(key: &str, value: &str) -> Result<()> {
    let mut config = Config::load()?;
    let config_toml = toml::to_string_pretty(&config)?;
    let mut toml_value = toml::from_str::<toml::Value>(&config_toml)?;

    // Parse nested key (e.g., "project_configs.enabled")
    let keys: Vec<&str> = key.split('.').collect();

    // Navigate to the parent of the target key
    let mut current = &mut toml_value;
    for k in &keys[..keys.len() - 1] {
        match current.get_mut(k) {
            Some(v) => current = v,
            None => {
                Output::error(&format!("Key path '{}' not found in config", key));
                return Ok(());
            }
        }
    }

    // Set the value
    let last_key = keys[keys.len() - 1];
    let table = match current.as_table_mut() {
        Some(t) => t,
        None => {
            Output::error(&format!("Cannot set value at '{}'", key));
            return Ok(());
        }
    };

    // Parse the value string into appropriate TOML type
    let new_value: toml::Value = if value == "true" {
        toml::Value::Boolean(true)
    } else if value == "false" {
        toml::Value::Boolean(false)
    } else if let Ok(i) = value.parse::<i64>() {
        toml::Value::Integer(i)
    } else if let Ok(f) = value.parse::<f64>() {
        toml::Value::Float(f)
    } else if value.starts_with('[') && value.ends_with(']') {
        // Array value - parse as TOML
        match toml::from_str(value) {
            Ok(v) => v,
            Err(e) => {
                Output::error(&format!("Failed to parse array: {}", e));
                return Ok(());
            }
        }
    } else {
        toml::Value::String(value.to_string())
    };

    table.insert(last_key.to_string(), new_value);

    // Convert back to config and save
    let config_toml = toml::to_string_pretty(&toml_value)?;
    config = toml::from_str(&config_toml)?;
    config.save()?;

    Output::success(&format!("Set {} = {}", key, value));
    Ok(())
}

pub async fn edit() -> Result<()> {
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
            Err(e) => {
                Output::error(&format!("Config validation failed: {}", e));
                Output::warning("Your changes were saved but contain errors");
            }
        }
    } else {
        Output::warning("Editor exited with error");
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
        render_entry_table("Files", &config.dotfiles.files);
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
            0 => Some(manage_entry_list(
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
    use owo_colors::OwoColorize;

    if entries.is_empty() {
        println!("{}", format!("{}: (none)", title).bright_black());
        return;
    }

    let mut table = Table::new();
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

fn normalize_entries(entries: &mut Vec<String>) {
    entries.iter_mut().for_each(|entry| {
        *entry = entry.trim().to_string();
    });
    entries.retain(|entry| !entry.is_empty());
    entries.sort();
    entries.dedup();
}

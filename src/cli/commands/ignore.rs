use crate::cli::Output;
use crate::config::Config;
use crate::sync::{MachineState, SyncEngine, SyncState};
use anyhow::Result;
use std::path::PathBuf;

fn ignore_file_path() -> Result<PathBuf> {
    Ok(Config::config_dir()?.join("ignore"))
}

pub async fn add(pattern: &str) -> Result<()> {
    let path = ignore_file_path()?;

    // Read existing patterns
    let mut patterns = if path.exists() {
        std::fs::read_to_string(&path)?
            .lines()
            .map(|s| s.to_string())
            .collect::<Vec<_>>()
    } else {
        Vec::new()
    };

    // Check if pattern already exists
    if patterns.iter().any(|p| p == pattern) {
        Output::warning(&format!("Pattern '{}' already exists", pattern));
        return Ok(());
    }

    // Add pattern
    patterns.push(pattern.to_string());

    // Write back
    std::fs::write(&path, patterns.join("\n") + "\n")?;

    Output::success(&format!("Added ignore pattern: {}", pattern));
    Ok(())
}

pub async fn list() -> Result<()> {
    let path = ignore_file_path()?;

    if !path.exists() {
        Output::info("No ignore patterns configured");
        Output::info("Add patterns with: tether ignore add <pattern>");
        return Ok(());
    }

    let content = std::fs::read_to_string(&path)?;
    let patterns: Vec<_> = content.lines().filter(|l| !l.is_empty()).collect();

    if patterns.is_empty() {
        Output::info("No ignore patterns configured");
        return Ok(());
    }

    println!();
    println!("Ignore patterns:");
    for pattern in patterns {
        println!("  • {}", pattern);
    }
    println!();

    Output::info(&format!("File: {}", path.display()));
    Ok(())
}

pub async fn remove(pattern: &str) -> Result<()> {
    let path = ignore_file_path()?;

    if !path.exists() {
        Output::error("No ignore patterns configured");
        return Ok(());
    }

    let content = std::fs::read_to_string(&path)?;
    let patterns: Vec<_> = content
        .lines()
        .filter(|l| !l.is_empty() && *l != pattern)
        .collect();

    if patterns.len() == content.lines().filter(|l| !l.is_empty()).count() {
        Output::error(&format!("Pattern '{}' not found", pattern));
        return Ok(());
    }

    std::fs::write(&path, patterns.join("\n") + "\n")?;

    Output::success(&format!("Removed ignore pattern: {}", pattern));
    Ok(())
}

/// Ignore a dotfile on this machine (won't be overwritten during sync)
pub async fn ignore_dotfile(file: &str) -> Result<()> {
    let state = SyncState::load()?;
    let sync_path = SyncEngine::sync_path()?;

    let mut machine_state = MachineState::load_from_repo(&sync_path, &state.machine_id)?
        .unwrap_or_else(|| MachineState::new(&state.machine_id));

    // Normalize dotfile name (ensure it starts with .)
    let file = if !file.starts_with('.') {
        format!(".{}", file)
    } else {
        file.to_string()
    };

    if machine_state.ignored_dotfiles.contains(&file) {
        Output::warning(&format!("'{}' is already ignored on this machine", file));
        return Ok(());
    }

    machine_state.ignored_dotfiles.push(file.clone());
    machine_state.ignored_dotfiles.sort();
    machine_state.save_to_repo(&sync_path)?;

    Output::success(&format!(
        "Ignoring '{}' on this machine (won't be overwritten during sync)",
        file
    ));
    Ok(())
}

/// Ignore a project config on this machine
pub async fn ignore_project(project: &str, path: &str) -> Result<()> {
    let state = SyncState::load()?;
    let sync_path = SyncEngine::sync_path()?;

    let mut machine_state = MachineState::load_from_repo(&sync_path, &state.machine_id)?
        .unwrap_or_else(|| MachineState::new(&state.machine_id));

    let ignored = machine_state
        .ignored_project_configs
        .entry(project.to_string())
        .or_default();

    if ignored.contains(&path.to_string()) {
        Output::warning(&format!(
            "'{}:{}' is already ignored on this machine",
            project, path
        ));
        return Ok(());
    }

    ignored.push(path.to_string());
    ignored.sort();
    machine_state.save_to_repo(&sync_path)?;

    Output::success(&format!(
        "Ignoring '{}:{}' on this machine (won't be overwritten during sync)",
        project, path
    ));
    Ok(())
}

/// List files ignored on this machine
pub async fn sync_list() -> Result<()> {
    let state = SyncState::load()?;
    let sync_path = SyncEngine::sync_path()?;

    let machine_state = match MachineState::load_from_repo(&sync_path, &state.machine_id)? {
        Some(ms) => ms,
        None => {
            Output::info("No machine state found");
            return Ok(());
        }
    };

    let has_ignored = !machine_state.ignored_dotfiles.is_empty()
        || !machine_state.ignored_project_configs.is_empty();

    if !has_ignored {
        Output::info("No files are ignored on this machine");
        Output::info("Use 'tether ignore dotfile <file>' or 'tether ignore project <project> <path>' to ignore files");
        return Ok(());
    }

    println!();
    if !machine_state.ignored_dotfiles.is_empty() {
        println!("Ignored dotfiles:");
        for file in &machine_state.ignored_dotfiles {
            println!("  • {}", file);
        }
    }

    if !machine_state.ignored_project_configs.is_empty() {
        println!("Ignored project configs:");
        for (project, paths) in &machine_state.ignored_project_configs {
            for path in paths {
                println!("  • {}:{}", project, path);
            }
        }
    }
    println!();

    Ok(())
}

/// Unignore a file on this machine
pub async fn sync_remove(file: &str) -> Result<()> {
    let state = SyncState::load()?;
    let sync_path = SyncEngine::sync_path()?;

    let mut machine_state = match MachineState::load_from_repo(&sync_path, &state.machine_id)? {
        Some(ms) => ms,
        None => {
            Output::error("No machine state found");
            return Ok(());
        }
    };

    // Check if it's a project config (format: "project:path")
    if let Some((project, path)) = file.split_once(':') {
        if let Some(ignored) = machine_state.ignored_project_configs.get_mut(project) {
            let len_before = ignored.len();
            ignored.retain(|p| p != path);

            if ignored.len() < len_before {
                if ignored.is_empty() {
                    machine_state.ignored_project_configs.remove(project);
                }
                machine_state.save_to_repo(&sync_path)?;
                Output::success(&format!("Unignored '{}:{}'", project, path));
                return Ok(());
            }
        }
        Output::error(&format!("'{}:{}' is not in the ignore list", project, path));
    } else {
        // It's a dotfile
        let len_before = machine_state.ignored_dotfiles.len();
        machine_state.ignored_dotfiles.retain(|f| f != file);

        if machine_state.ignored_dotfiles.len() < len_before {
            machine_state.save_to_repo(&sync_path)?;
            Output::success(&format!("Unignored '{}'", file));
            return Ok(());
        }
        Output::error(&format!("'{}' is not in the ignore list", file));
    }

    Ok(())
}

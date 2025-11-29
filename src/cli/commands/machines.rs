use crate::cli::{Output, Prompt};
use crate::sync::{GitBackend, SyncEngine, SyncState};
use anyhow::Result;
use comfy_table::{presets::UTF8_FULL, Attribute, Cell, Color, ContentArrangement, Table};
use owo_colors::OwoColorize;
use serde::{Deserialize, Serialize};

#[derive(Debug, Serialize, Deserialize)]
struct MachineInfo {
    machine_id: String,
    hostname: String,
    last_sync: String,
    #[serde(default)]
    os_version: String,
}

pub async fn list() -> Result<()> {
    let sync_path = SyncEngine::sync_path()?;
    let machines_dir = sync_path.join("machines");

    if !machines_dir.exists() {
        Output::info("No machines synced yet");
        return Ok(());
    }

    let state = SyncState::load()?;
    let current_machine = &state.machine_id;

    let mut machines: Vec<(String, MachineInfo)> = Vec::new();

    for entry in std::fs::read_dir(&machines_dir)? {
        let entry = entry?;
        let path = entry.path();

        if path.extension().map(|e| e == "json").unwrap_or(false) {
            if let Ok(content) = std::fs::read_to_string(&path) {
                if let Ok(info) = serde_json::from_str::<MachineInfo>(&content) {
                    let name = path
                        .file_stem()
                        .and_then(|s| s.to_str())
                        .unwrap_or("unknown")
                        .to_string();
                    machines.push((name, info));
                }
            }
        }
    }

    if machines.is_empty() {
        Output::info("No machines synced yet");
        return Ok(());
    }

    println!();
    println!("{}", "🖥️  Synced Machines".bright_cyan().bold());
    println!();

    let mut table = Table::new();
    table
        .load_preset(UTF8_FULL)
        .set_content_arrangement(ContentArrangement::Dynamic)
        .set_header(vec![
            Cell::new("Machine")
                .add_attribute(Attribute::Bold)
                .fg(Color::Cyan),
            Cell::new("Hostname")
                .add_attribute(Attribute::Bold)
                .fg(Color::Cyan),
            Cell::new("Last Sync")
                .add_attribute(Attribute::Bold)
                .fg(Color::Cyan),
            Cell::new("").add_attribute(Attribute::Bold).fg(Color::Cyan),
        ]);

    for (name, info) in &machines {
        let is_current = &info.machine_id == current_machine;
        let marker = if is_current { "(this machine)" } else { "" };

        table.add_row(vec![
            if is_current {
                Cell::new(name.clone()).fg(Color::Green)
            } else {
                Cell::new(name.clone())
            },
            Cell::new(&info.hostname),
            Cell::new(&info.last_sync),
            Cell::new(marker).fg(Color::Green),
        ]);
    }

    println!("{table}");
    println!();

    Ok(())
}

pub async fn rename(old: &str, new: &str) -> Result<()> {
    let sync_path = SyncEngine::sync_path()?;
    let machines_dir = sync_path.join("machines");

    let old_file = machines_dir.join(format!("{}.json", old));
    let new_file = machines_dir.join(format!("{}.json", new));

    if !old_file.exists() {
        Output::error(&format!("Machine '{}' not found", old));
        return Ok(());
    }

    if new_file.exists() {
        Output::error(&format!("Machine '{}' already exists", new));
        return Ok(());
    }

    // Read and update the machine info
    let content = std::fs::read_to_string(&old_file)?;
    let mut info: MachineInfo = serde_json::from_str(&content)?;
    info.machine_id = new.to_string();

    // Write to new file
    std::fs::write(&new_file, serde_json::to_string_pretty(&info)?)?;

    // Remove old file
    std::fs::remove_file(&old_file)?;

    // Update local state if this is the current machine
    let mut state = SyncState::load()?;
    if state.machine_id == old {
        state.machine_id = new.to_string();
        state.save()?;
    }

    // Commit and push
    let git = GitBackend::open(&sync_path)?;
    git.commit(&format!("Rename machine {} to {}", old, new), new)?;
    git.push()?;

    Output::success(&format!("Renamed machine '{}' to '{}'", old, new));
    Ok(())
}

pub async fn remove(name: &str) -> Result<()> {
    let state = SyncState::load()?;

    if state.machine_id == name {
        Output::error("Cannot remove the current machine");
        Output::info("Use this command from a different machine to remove this one");
        return Ok(());
    }

    let sync_path = SyncEngine::sync_path()?;
    let machines_dir = sync_path.join("machines");
    let machine_file = machines_dir.join(format!("{}.json", name));

    if !machine_file.exists() {
        Output::error(&format!("Machine '{}' not found", name));
        return Ok(());
    }

    if !Prompt::confirm(&format!("Remove machine '{}'?", name), false)? {
        return Ok(());
    }

    std::fs::remove_file(&machine_file)?;

    // Commit and push
    let git = GitBackend::open(&sync_path)?;
    git.commit(&format!("Remove machine {}", name), &state.machine_id)?;
    git.push()?;

    Output::success(&format!("Removed machine '{}'", name));
    Ok(())
}

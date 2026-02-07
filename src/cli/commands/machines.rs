use crate::cli::{Output, Prompt};
use crate::config::Config;
use crate::sync::{GitBackend, MachineState, SyncEngine, SyncState};
use anyhow::Result;
use chrono::Local;
use comfy_table::{Attribute, Cell, Color};
use owo_colors::OwoColorize;

pub async fn list() -> Result<()> {
    let config = Config::load()?;
    if !config.has_personal_features() {
        Output::warning("Machine management not available in team-only mode");
        return Ok(());
    }

    let sync_path = SyncEngine::sync_path()?;
    let machines = MachineState::list_all(&sync_path)?;

    if machines.is_empty() {
        Output::info("No machines synced yet");
        return Ok(());
    }

    let state = SyncState::load()?;
    let current_machine = &state.machine_id;

    println!();
    println!("{}", "Synced Machines".bright_cyan().bold());
    println!();

    let mut table = Output::table_full();
    table.set_header(vec![
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

    for machine in &machines {
        let is_current = &machine.machine_id == current_machine;
        let marker = if is_current { "(this machine)" } else { "" };
        let local_time = machine.last_sync.with_timezone(&Local);

        table.add_row(vec![
            if is_current {
                Cell::new(&machine.machine_id).fg(Color::Green)
            } else {
                Cell::new(&machine.machine_id)
            },
            Cell::new(&machine.hostname),
            Cell::new(local_time.format("%Y-%m-%d %H:%M:%S").to_string()),
            Cell::new(marker).fg(Color::Green),
        ]);
    }

    println!("{table}");
    println!();

    Ok(())
}

pub async fn rename(old: &str, new: &str) -> Result<()> {
    let config = Config::load()?;
    if !config.has_personal_features() {
        Output::warning("Machine management not available in team-only mode");
        return Ok(());
    }

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
    let mut machine = MachineState::load_from_repo(&sync_path, old)?
        .ok_or_else(|| anyhow::anyhow!("Machine not found"))?;
    machine.machine_id = new.to_string();

    // Write to new file
    let content = serde_json::to_string_pretty(&machine)?;
    std::fs::write(&new_file, content)?;

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
    let config = Config::load()?;
    if !config.has_personal_features() {
        Output::warning("Machine management not available in team-only mode");
        return Ok(());
    }

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

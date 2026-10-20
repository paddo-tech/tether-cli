use crate::cli::output::Colorize;
use crate::cli::Output;
use crate::config::Config;
use crate::sync::{conflict, ConflictResolution, ConflictState, Keep, SyncEngine};
use anyhow::Result;

/// With `keep`, each conflict keeps that side without a diff or a prompt.
pub async fn run(file: Option<&str>, keep: Option<Keep>) -> Result<()> {
    let config = Config::load()?;

    if !config.has_personal_features() {
        anyhow::bail!("Resolve is not available without personal features");
    }

    // Hold the lock while prompting, so a daemon sync cannot overwrite the
    // state.json baselines that the resolutions below record.
    let _sync_lock = crate::sync::acquire_sync_lock(true)?;

    let mut conflict_state = ConflictState::load()?;

    if conflict_state.conflicts.is_empty() {
        if let Some(f) = file {
            anyhow::bail!("No conflict found for '{}'", f);
        }
        Output::success("No conflicts to resolve");
        return Ok(());
    }

    let home = crate::home_dir()?;
    let sync_path = SyncEngine::sync_path()?;
    let machine_id = crate::sync::SyncState::load()?.machine_id;

    // Filter to specific file if provided
    let conflicts_to_resolve: Vec<_> = if let Some(file_filter) = file {
        conflict_state
            .conflicts
            .iter()
            .filter(|c| c.file_path == file_filter)
            .cloned()
            .collect()
    } else {
        conflict_state.conflicts.clone()
    };

    if conflicts_to_resolve.is_empty() {
        if let Some(f) = file {
            anyhow::bail!("No conflict found for '{}'", f);
        }
        Output::success("No conflicts to resolve");
        return Ok(());
    }

    println!();
    println!(
        "{} {} conflict(s) to resolve",
        "🔧".yellow(),
        conflicts_to_resolve.len()
    );
    println!();

    let mut resolved = Vec::new();
    let mut backup_dir = None;
    for pending in &conflicts_to_resolve {
        let (conflict, remote_file) =
            conflict::load_conflict(&config, &sync_path, &machine_id, &pending.file_path)?;

        let resolution = match keep {
            Some(Keep::Local) => ConflictResolution::KeepLocal,
            Some(Keep::Remote) => ConflictResolution::UseRemote,
            None => {
                conflict.show_diff()?;
                conflict.prompt_resolution()?
            }
        };

        match resolution {
            ConflictResolution::KeepLocal => {
                Output::info(&format!("  {} (kept local)", pending.file_path));
                conflict_state.remove_conflict(&pending.file_path);
            }
            ConflictResolution::UseRemote => {
                conflict::apply_side(&mut backup_dir, &conflict, &remote_file, Keep::Remote)?;
                Output::success(&format!("  {} (applied remote)", pending.file_path));
                conflict_state.remove_conflict(&pending.file_path);
            }
            ConflictResolution::Merged => {
                conflict.launch_merge_tool(&config.merge_tool()?, &home)?;
                conflict_state.remove_conflict(&pending.file_path);
            }
            ConflictResolution::Skip => {
                Output::info(&format!("  {} (skipped)", pending.file_path));
            }
        }
        if resolution != ConflictResolution::Skip {
            // Baseline on the remote so the next sync pushes the kept or merged
            // local file instead of detecting the same conflict again.
            resolved.push((pending.file_path.clone(), conflict.remote_hash.clone()));
        }

        println!();
    }

    conflict_state.save()?;
    // Reload after the prompts: package updates save state.json without the sync lock.
    let mut state = crate::sync::SyncState::load()?;
    for (file, remote_hash) in resolved {
        state.record_remote_file(&file, remote_hash);
    }
    state.save()?;

    let remaining = conflict_state.conflicts.len();
    if remaining > 0 {
        Output::warning(&format!("{} conflict(s) still pending", remaining));
    } else {
        Output::success("All conflicts resolved!");
    }

    Ok(())
}

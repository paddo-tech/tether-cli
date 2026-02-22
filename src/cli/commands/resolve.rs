use crate::cli::Output;
use crate::config::Config;
use crate::sync::{ConflictResolution, ConflictState, FileConflict, SyncEngine};
use anyhow::Result;
use owo_colors::OwoColorize;
use sha2::{Digest, Sha256};

pub async fn run(file: Option<&str>) -> Result<()> {
    let config = Config::load()?;

    if !config.has_personal_features() {
        Output::warning("Resolve not available without personal features");
        return Ok(());
    }

    let mut conflict_state = ConflictState::load()?;

    if conflict_state.conflicts.is_empty() {
        Output::success("No conflicts to resolve");
        return Ok(());
    }

    let home = crate::home_dir()?;
    let sync_path = SyncEngine::sync_path()?;
    let state = crate::sync::SyncState::load()?;
    let machine_id = &state.machine_id;
    let profile = config.profile_name(machine_id);

    // Get encryption key if needed
    let key = if config.security.encrypt_dotfiles {
        Some(crate::security::get_encryption_key()?)
    } else {
        None
    };

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
            Output::error(&format!("No conflict found for '{}'", f));
        } else {
            Output::success("No conflicts to resolve");
        }
        return Ok(());
    }

    println!();
    println!(
        "{} {} conflict(s) to resolve",
        "🔧".yellow(),
        conflicts_to_resolve.len()
    );
    println!();

    for pending in &conflicts_to_resolve {
        // Load local and remote content
        let local_path = home.join(&pending.file_path);
        let local_content = if local_path.exists() {
            std::fs::read(&local_path)?
        } else {
            Vec::new()
        };

        // Get remote content
        let shared = config.is_dotfile_shared(machine_id, &pending.file_path);
        let repo_rel = crate::sync::resolve_dotfile_repo_path(
            &sync_path,
            &pending.file_path,
            config.security.encrypt_dotfiles,
            profile,
            shared,
        );
        let remote_file = sync_path.join(&repo_rel);
        let remote_content = if remote_file.exists() {
            let raw = std::fs::read(&remote_file)?;
            if config.security.encrypt_dotfiles {
                crate::security::decrypt(&raw, key.as_ref().unwrap())?
            } else {
                raw
            }
        } else {
            Vec::new()
        };

        let conflict = FileConflict {
            file_path: pending.file_path.clone(),
            local_hash: format!("{:x}", Sha256::digest(&local_content)),
            last_synced_hash: None,
            remote_hash: format!("{:x}", Sha256::digest(&remote_content)),
            local_content,
            remote_content,
        };

        // Show diff and prompt for resolution
        conflict.show_diff()?;
        let resolution = conflict.prompt_resolution()?;

        match resolution {
            ConflictResolution::KeepLocal => {
                Output::info(&format!("  {} (kept local)", pending.file_path));
                conflict_state.remove_conflict(&pending.file_path);
            }
            ConflictResolution::UseRemote => {
                std::fs::write(&local_path, &conflict.remote_content)?;
                Output::success(&format!("  {} (applied remote)", pending.file_path));
                conflict_state.remove_conflict(&pending.file_path);
            }
            ConflictResolution::Merged => {
                conflict.launch_merge_tool(&config.merge, &home)?;
                conflict_state.remove_conflict(&pending.file_path);
            }
            ConflictResolution::Skip => {
                Output::info(&format!("  {} (skipped)", pending.file_path));
            }
        }

        println!();
    }

    conflict_state.save()?;

    let remaining = conflict_state.conflicts.len();
    if remaining > 0 {
        Output::warning(&format!("{} conflict(s) still pending", remaining));
    } else {
        Output::success("All conflicts resolved!");
    }

    Ok(())
}

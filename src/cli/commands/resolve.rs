use crate::cli::Output;
use crate::config::Config;
use crate::sync::{ConflictResolution, ConflictState, FileConflict, SyncEngine};
use anyhow::Result;
use owo_colors::OwoColorize;
use sha2::{Digest, Sha256};

pub async fn run(file: Option<&str>) -> Result<()> {
    let config = Config::load()?;
    let mut conflict_state = ConflictState::load()?;

    if conflict_state.conflicts.is_empty() {
        Output::success("No conflicts to resolve");
        return Ok(());
    }

    let home = home::home_dir().ok_or_else(|| anyhow::anyhow!("Could not find home directory"))?;
    let sync_path = SyncEngine::sync_path()?;
    let dotfiles_dir = sync_path.join("dotfiles");

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
        let filename = pending.file_path.trim_start_matches('.');
        let remote_content = if config.security.encrypt_dotfiles {
            let enc_file = dotfiles_dir.join(format!("{}.enc", filename));
            if enc_file.exists() {
                let encrypted = std::fs::read(&enc_file)?;
                crate::security::decrypt_file(&encrypted, key.as_ref().unwrap())?
            } else {
                Vec::new()
            }
        } else {
            let plain_file = dotfiles_dir.join(filename);
            if plain_file.exists() {
                std::fs::read(&plain_file)?
            } else {
                Vec::new()
            }
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

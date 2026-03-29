use crate::cli::output::relative_time;
use crate::cli::{Output, Prompt};
use crate::config::Config;
use crate::sync::{
    list_backup_files, list_backups, restore_file, GitBackend, SyncEngine, SyncState,
};
use anyhow::Result;

pub async fn run(timestamp: Option<&str>, file: Option<&str>) -> Result<()> {
    let backups = list_backups()?;

    if backups.is_empty() {
        Output::info("No backups available");
        return Ok(());
    }

    // Select backup timestamp
    let selected_timestamp = match timestamp {
        Some(t) => t.to_string(),
        None => {
            // Show list and let user pick
            let options: Vec<&str> = backups.iter().map(|s| s.as_str()).collect();
            let idx = Prompt::select("Select backup to restore from", options.clone(), 0)?;
            options[idx].to_string()
        }
    };

    // Get files in this backup
    let files = list_backup_files(&selected_timestamp)?;
    if files.is_empty() {
        Output::info("No files in this backup");
        return Ok(());
    }

    // Select file to restore
    let (category, rel_path) = match file {
        Some(f) => {
            // Find matching file
            files
                .iter()
                .find(|(cat, path)| path == f || format!("{}/{}", cat, path) == f)
                .cloned()
                .ok_or_else(|| anyhow::anyhow!("File '{}' not found in backup", f))?
        }
        None => {
            // Show list and let user pick
            let display: Vec<String> = files
                .iter()
                .map(|(cat, path)| format!("{}/{}", cat, path))
                .collect();
            let options: Vec<&str> = display.iter().map(|s| s.as_str()).collect();
            let idx = Prompt::select("Select file to restore", options, 0)?;
            files[idx].clone()
        }
    };

    // Confirm restore
    println!();
    Output::warning(&format!(
        "This will overwrite: {}",
        if category == "dotfiles" {
            format!("~/{}", rel_path)
        } else {
            rel_path.clone()
        }
    ));

    if !Prompt::confirm("Continue?", false)? {
        Output::info("Restore cancelled");
        return Ok(());
    }

    // Do the restore
    match restore_file(&selected_timestamp, &category, &rel_path) {
        Ok(dest) => {
            Output::success(&format!("Restored to {}", dest.display()));
        }
        Err(e) => {
            Output::error(&format!("Failed to restore: {}", e));
        }
    }

    Ok(())
}

pub async fn git_restore(file: &str, commit: Option<&str>) -> Result<()> {
    if !crate::config::is_safe_dotfile_path(file) {
        anyhow::bail!("Unsafe file path: {}", file);
    }

    let config = Config::load()?;
    let sync_path = SyncEngine::sync_path()?;
    let git = GitBackend::open(&sync_path)?;
    let home = crate::home_dir()?;
    let state = SyncState::load()?;

    let encrypted = config.security.encrypt_dotfiles;
    let profile = config.profile_name(&state.machine_id);
    let shared = config.is_dotfile_shared(&state.machine_id, file);
    let repo_path =
        crate::sync::resolve_dotfile_repo_path(&sync_path, file, encrypted, profile, shared);

    // Get or pick commit
    let selected_commit = match commit {
        Some(c) => c.to_string(),
        None => {
            let entries = git.file_log(&repo_path, 20)?;
            if entries.is_empty() {
                Output::info(&format!("No history found for {}", file));
                return Ok(());
            }

            let options: Vec<String> = entries
                .iter()
                .map(|e| {
                    format!(
                        "{}  {}  {}  {}",
                        e.short_hash,
                        relative_time(e.date),
                        e.machine_id,
                        e.message
                    )
                })
                .collect();
            let opts: Vec<&str> = options.iter().map(|s| s.as_str()).collect();
            let idx = Prompt::select("Select commit to restore from", opts, 0)?;
            entries[idx].commit_hash.clone()
        }
    };

    // Get file at commit
    let content = git.show_at_commit(&selected_commit, &repo_path)?;

    // Decrypt if needed
    let plaintext = if config.security.encrypt_dotfiles {
        let key = crate::security::get_encryption_key()?;
        crate::security::decrypt(&content, &key)?
    } else {
        content
    };

    // Backup current file
    let dest = home.join(file);
    if dest.exists() {
        let backup_dir = crate::sync::create_backup_dir()?;
        crate::sync::backup_file(&backup_dir, "dotfiles", file, &dest)?;
    }

    // Write restored content
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&dest, &plaintext)?;

    // Don't update state hash — leaving it unchanged makes the next sync see
    // "local changed, remote unchanged" and push restored content to repo.

    Output::success(&format!(
        "Restored {} from commit {}",
        file,
        &selected_commit[..7.min(selected_commit.len())]
    ));
    Ok(())
}

pub async fn list_cmd() -> Result<()> {
    let backups = list_backups()?;

    if backups.is_empty() {
        Output::info("No backups available");
        return Ok(());
    }

    Output::section("Backups");
    println!();

    for timestamp in &backups {
        let files = list_backup_files(timestamp).unwrap_or_default();
        println!(
            "  {} ({} file{})",
            timestamp,
            files.len(),
            if files.len() == 1 { "" } else { "s" }
        );

        for (category, path) in files.iter().take(5) {
            Output::dim(&format!("    {}/{}", category, path));
        }
        if files.len() > 5 {
            Output::dim(&format!("    ... and {} more", files.len() - 5));
        }
        println!();
    }

    Ok(())
}

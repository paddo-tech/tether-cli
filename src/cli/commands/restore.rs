use crate::cli::{Output, Prompt};
use crate::sync::{list_backup_files, list_backups, restore_file};
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

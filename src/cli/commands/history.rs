use crate::cli::output::relative_time;
use crate::cli::Output;
use crate::config::Config;
use crate::sync::{GitBackend, SyncEngine, SyncState};
use anyhow::Result;

pub async fn run(file: &str, limit: usize) -> Result<()> {
    if !crate::config::is_safe_dotfile_path(file) {
        anyhow::bail!("Unsafe file path: {}", file);
    }

    let config = Config::load()?;
    let sync_path = SyncEngine::sync_path()?;
    let git = GitBackend::open(&sync_path)?;
    let state = SyncState::load()?;

    let encrypted = config.security.encrypt_dotfiles;
    let profile = config.profile_name(&state.machine_id);
    let shared = config.is_dotfile_shared(&state.machine_id, file);
    let repo_path =
        crate::sync::resolve_dotfile_repo_path(&sync_path, file, encrypted, profile, shared);
    let entries = git.file_log(&repo_path, limit)?;

    if entries.is_empty() {
        Output::info(&format!("No history found for {}", file));
        return Ok(());
    }

    println!();
    Output::section(&format!("History for {} ({} entries)", file, entries.len()));
    println!();

    for entry in &entries {
        let time = relative_time(entry.date);
        println!(
            "  {}  {:>12}   {:15}  {}",
            entry.short_hash, time, entry.machine_id, entry.message
        );
    }

    println!();
    Ok(())
}

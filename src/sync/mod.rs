pub mod conflict;
pub mod discovery;
pub mod engine;
pub mod git;
pub mod packages;
pub mod state;
pub mod team;

pub use conflict::{
    detect_conflict, notify_conflict, notify_conflicts, ConflictResolution, ConflictState,
    FileConflict, PendingConflict,
};
pub use discovery::discover_sourced_dirs;
pub use engine::SyncEngine;
pub use git::GitBackend;
pub use packages::{import_packages, sync_packages};
pub use state::{MachineState, SyncState};
pub use team::{
    discover_symlinkable_dirs, extract_team_name_from_url, resolve_conflict, TeamManifest,
};

use anyhow::Result;
use std::path::Path;

/// Atomically write content to a file by writing to a temp file and renaming.
/// This prevents file corruption from interrupted writes.
pub fn atomic_write(path: &Path, content: &[u8]) -> Result<()> {
    use std::io::Write;

    let parent = path
        .parent()
        .ok_or_else(|| anyhow::anyhow!("Invalid path: no parent directory"))?;
    std::fs::create_dir_all(parent)?;

    // Create temp file in same directory (required for atomic rename)
    let mut temp = tempfile::NamedTempFile::new_in(parent)?;
    temp.write_all(content)?;
    temp.flush()?;

    // Persist atomically renames the temp file to the target
    temp.persist(path)?;
    Ok(())
}

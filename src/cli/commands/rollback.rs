use crate::cli::Output;
use crate::config::Config;
use crate::sync::{GitBackend, SyncEngine, SyncState};
use anyhow::Result;
use std::collections::HashSet;

/// Reverse-delta against the union manifest at `commit`; the follow-up sync records removals.
pub async fn packages(manager: &str, commit: &str) -> Result<()> {
    let pkg_manager = crate::packages::manager_for_key(manager)
        .ok_or_else(|| anyhow::anyhow!("Rollback is not supported for {}", manager))?;

    let config = Config::load()?;
    let state = SyncState::load()?;
    if !config.is_manager_enabled(&state.machine_id, manager) {
        anyhow::bail!("{} is disabled for this machine", manager);
    }
    if !pkg_manager.is_available().await {
        anyhow::bail!("{} is not available on this machine", manager);
    }

    let manifest = crate::sync::packages::manifest_filename(manager)
        .ok_or_else(|| anyhow::anyhow!("No manifest for {}", manager))?;
    let repo_path = format!("manifests/{}", manifest);

    // Removal tombstones are diffed against the saved package list, so it must
    // match what is installed before anything is removed.
    super::sync::run(false, false, false).await?;

    let sync_path = SyncEngine::sync_path()?;
    let git = GitBackend::open(&sync_path)?;
    let snapshot = git.show_at_commit(commit, &repo_path)?;
    let snapshot = String::from_utf8_lossy(&snapshot);
    let target: HashSet<String> = snapshot
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(str::to_string)
        .collect();

    let installed: HashSet<String> = pkg_manager
        .list_installed()
        .await?
        .into_iter()
        .map(|p| p.name)
        .collect();

    let mut to_install: Vec<&String> = target.difference(&installed).collect();
    let mut to_uninstall: Vec<&String> = installed.difference(&target).collect();
    to_install.sort();
    to_uninstall.sort();

    if to_install.is_empty() && to_uninstall.is_empty() {
        Output::info(&format!("{} already matches that snapshot", manager));
        return Ok(());
    }

    Output::header(&format!("Rolling back {}", manager));
    for pkg in &to_uninstall {
        Output::list_item(&format!("uninstall {}", pkg));
    }
    for pkg in &to_install {
        Output::list_item(&format!("install {}", pkg));
    }

    let mut failed: Vec<&str> = Vec::new();
    for pkg in &to_uninstall {
        if let Err(e) = pkg_manager.uninstall(pkg).await {
            Output::warning(&format!("Failed to uninstall {}: {}", pkg, e));
            failed.push(pkg);
        }
    }
    if !to_install.is_empty() {
        let manifest_text = to_install
            .iter()
            .map(|s| s.as_str())
            .collect::<Vec<_>>()
            .join("\n")
            + "\n";
        if let Err(e) = pkg_manager.import_manifest(&manifest_text).await {
            Output::warning(&format!("Some packages failed to install: {}", e));
        }
        // import_manifest swallows per-package errors, so check the result.
        let now_installed: HashSet<String> = pkg_manager
            .list_installed()
            .await?
            .into_iter()
            .map(|p| p.name)
            .collect();
        for pkg in &to_install {
            if !now_installed.contains(*pkg) {
                Output::warning(&format!("Failed to install {}", pkg));
                failed.push(pkg);
            }
        }
    }

    Output::success("Rollback applied; syncing...");
    super::sync::run(false, false, false).await?;

    if !failed.is_empty() {
        anyhow::bail!("{} package(s) failed: {}", failed.len(), failed.join(", "));
    }
    Ok(())
}

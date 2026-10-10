use crate::cli::prompts::Prompt;
use crate::cli::Output;
use crate::config::Config;
use crate::packages::pin::parse_pin;
use crate::packages::Ecosystem;
use crate::sync::packages::{RollbackGate, RollbackLine};
use crate::sync::{GitBackend, MachineState, SyncEngine, SyncState};
use anyhow::Result;
use std::collections::{HashMap, HashSet};

/// Reverse-delta against the union manifest at `commit`; the follow-up sync records removals.
/// Nothing changes before the user confirms the plan. The sync lock covers the whole rollback, so the daemon cannot install or record packages
/// between its steps. Each package installs at the newest version a trusted machine record
/// lists, as in a sync, unless the user confirms the snapshot's version in a terminal. The
/// snapshot's version is the one this machine's record lists at `commit`. Every
/// install passes the trust and OSV checks of a sync, and a confirmed version counts as
/// approved. Without a terminal, `yes` must confirm the rollback, and no older version
/// installs.
pub async fn packages(manager: &str, commit: &str, yes: bool) -> Result<()> {
    let _sync_lock = crate::sync::acquire_sync_lock(true)?;
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

    let sync_path = SyncEngine::sync_path()?;
    let git = GitBackend::open(&sync_path)?;
    let snapshot = git.show_at_commit(commit, &repo_path)?;
    let snapshot = String::from_utf8_lossy(&snapshot);
    // Manifests name packages only, so the versions come from this machine's record at the
    // same commit. They are only suggestions: an older version still needs confirmation.
    let record = git
        .show_at_commit(commit, &format!("machines/{}.json", state.machine_id))
        .ok();
    let pins = snapshot_versions(
        pkg_manager.ecosystem(),
        manager,
        &snapshot,
        record.as_deref(),
    );
    let target: HashSet<String> = pins.keys().cloned().collect();

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

    let gate = RollbackGate::load(&config, &sync_path, &state.machine_id, manager)?;
    let plan = gate.plan(
        to_install
            .iter()
            .map(|name| ((*name).clone(), pins[*name].clone()))
            .collect(),
    );

    Output::header(&format!("Rolling back {} to {}", manager, commit));
    for pkg in &to_uninstall {
        Output::list_item(&format!("uninstall {}", pkg));
    }
    for line in &plan {
        Output::list_item(&describe(line));
    }
    let interactive = Prompt::is_interactive();
    if !yes {
        if !interactive {
            anyhow::bail!("Run 'tether rollback' in a terminal to confirm, or pass -y");
        }
        if !Prompt::confirm("Apply this rollback?", false)? {
            return Ok(());
        }
    }
    // --yes answers only the rollback question, never the choice of an older version
    let mut confirmed = HashSet::new();
    if interactive && !yes {
        for line in plan.iter().filter(|l| l.needs_confirmation()) {
            let question = format!(
                "Install {} {}? {}",
                line.name,
                line.version.as_deref().unwrap_or_default(),
                not_trusted(line)
            );
            if Prompt::question(&question, false)? {
                confirmed.insert(line.name.clone());
            }
        }
    }

    // A snapshot can hold any machine's packages, so they pass the same checks as a sync
    let install_lines = gate.gate(plan, &confirmed).await?;
    let to_install: Vec<String> = install_lines
        .iter()
        .map(|line| parse_pin(pkg_manager.ecosystem(), line).0)
        .collect();
    for line in &install_lines {
        Output::list_item(&format!("install {}", line));
    }

    // A sync tombstones only packages its saved list knew, so the rollback records its own
    // removals; else the next sync would install them again.
    let mut record = crate::sync::signing::own_record(&sync_path, &state.machine_id)?
        .unwrap_or_else(|| crate::sync::MachineState::new(&state.machine_id));
    if record.add_removals(manager, &to_uninstall) {
        crate::sync::signing::save_record(&sync_path, &record)?;
    }

    let mut failed: Vec<&str> = Vec::new();
    for pkg in &to_uninstall {
        if let Err(e) = pkg_manager.uninstall(pkg).await {
            Output::warning(&format!("Failed to uninstall {}: {}", pkg, e));
            failed.push(pkg);
        }
    }
    if !install_lines.is_empty() {
        let manifest_text = install_lines.join("\n") + "\n";
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
            if !now_installed.contains(pkg) {
                Output::warning(&format!("Failed to install {}", pkg));
                failed.push(pkg);
            }
        }
    }

    Output::success("Rollback applied; syncing...");
    super::sync::run_locked(false, false, false).await?;

    if !failed.is_empty() {
        anyhow::bail!("{} package(s) failed: {}", failed.len(), failed.join(", "));
    }
    Ok(())
}

/// Each package a manifest snapshot names, with the version this machine's record at that
/// commit lists. A pinned line from a pre-release 2.0 build keeps its own version.
fn snapshot_versions(
    ecosystem: Ecosystem,
    manager: &str,
    snapshot: &str,
    record: Option<&[u8]>,
) -> HashMap<String, Option<String>> {
    let recorded = record
        .and_then(|bytes| serde_json::from_slice::<MachineState>(bytes).ok())
        .and_then(|mut record| {
            record.validate().ok()?;
            record.package_versions.remove(manager)
        })
        .unwrap_or_default();
    snapshot
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(|l| {
            let (name, version) = parse_pin(ecosystem, l);
            let version = version.or_else(|| recorded.get(&name).cloned());
            (name, version)
        })
        .collect()
}

fn describe(line: &RollbackLine) -> String {
    if line.needs_confirmation() {
        let otherwise = match &line.trusted {
            Some(newest) => format!("installs {}", newest),
            None => "waits for approval".to_string(),
        };
        return format!(
            "install {} {} if you confirm it ({}), else it {}",
            line.name,
            line.version.as_deref().unwrap_or_default(),
            not_trusted(line),
            otherwise
        );
    }
    match line.trusted.as_deref().or(line.version.as_deref()) {
        Some(version) => format!("install {} {}", line.name, version),
        None => format!("install {}", line.name),
    }
}

fn not_trusted(line: &RollbackLine) -> String {
    match &line.trusted {
        Some(newest) => format!("the newest version a trusted machine lists is {}", newest),
        None => "no trusted machine lists it".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snapshot_versions_come_from_this_machines_record_at_the_commit() {
        let mut record = MachineState::new("this");
        record.package_versions.insert(
            "npm".to_string(),
            HashMap::from([
                ("a".to_string(), "1.0.0".to_string()),
                ("b".to_string(), "2.0.0".to_string()),
            ]),
        );
        let json = serde_json::to_vec(&record).unwrap();
        let versions = snapshot_versions(Ecosystem::Npm, "npm", "a\nb@3.0.0\nc\n", Some(&json));
        assert_eq!(versions["a"].as_deref(), Some("1.0.0"));
        assert_eq!(versions["b"].as_deref(), Some("3.0.0"));
        assert_eq!(versions["c"], None);
        assert_eq!(
            snapshot_versions(Ecosystem::Npm, "npm", "a\n", None)["a"],
            None
        );
    }

    #[test]
    fn add_removals_records_each_removal_once() {
        let mut record = crate::sync::MachineState::new("this");
        record
            .removed_packages
            .insert("npm".to_string(), vec!["old".to_string()]);
        let (old, new) = ("old".to_string(), "new".to_string());
        assert!(record.add_removals("npm", &[&old, &new]));
        assert_eq!(record.removed_packages["npm"], vec!["old", "new"]);
        assert!(!record.add_removals("npm", &[&new]));
    }
}

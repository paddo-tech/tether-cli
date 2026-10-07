use crate::cli::output::Output;
use crate::cli::prompts::Prompt;
use crate::packages::{
    brew::BrewManager, bun::BunManager, gem::GemManager, manager::PackageManager, npm::NpmManager,
    pnpm::PnpmManager, uv::UvManager, Cooldown, PackageInfo,
};
use crate::sync::SyncState;
use anyhow::Result;
use chrono::Utc;

/// Holds the sync lock, because upgrades queue malicious targets in the inbox that a sync
/// reads and changes. Without a terminal it upgrades only with `-y`.
pub async fn run(dry_run: bool) -> Result<()> {
    if !dry_run && !Prompt::assume_yes() && !Prompt::is_interactive() {
        anyhow::bail!(
            "tether upgrade needs a terminal to confirm. Pass -y to upgrade without asking, or --dry-run to list the upgrades"
        );
    }
    let _sync_lock = crate::sync::acquire_sync_lock(true)?;
    Output::header(if dry_run {
        "Upgrades (dry run)"
    } else {
        "Upgrading packages"
    });

    let managers: Vec<Box<dyn PackageManager>> = vec![
        Box::new(BrewManager::new()),
        Box::new(NpmManager::new()),
        Box::new(PnpmManager::new()),
        Box::new(BunManager::new()),
        Box::new(GemManager::new()),
        Box::new(UvManager::new()),
    ];

    // Determine which managers are available and have packages
    let mut available: Vec<(usize, usize)> = Vec::new();
    for (i, manager) in managers.iter().enumerate() {
        if !manager.is_available().await {
            continue;
        }
        let packages = manager.list_installed().await?;
        if packages.is_empty() {
            continue;
        }
        if manager.cooldown().await == Cooldown::Unsupported
            && !confirm_without_cooldown(manager.name(), dry_run)?
        {
            Output::warning(&format!("Skipped {}", manager.name()));
            continue;
        }
        available.push((i, packages.len()));
    }

    let mut any_change = false;
    for (i, _) in &available {
        any_change |= show_plan(managers[*i].as_ref()).await;
    }
    if dry_run {
        Output::info("Dry run: nothing changed");
        return Ok(());
    }
    if !any_change {
        Output::success("Packages are up to date");
        return Ok(());
    }
    if !Prompt::confirm("Upgrade these packages?", true)? {
        Output::info("Upgrade cancelled");
        return Ok(());
    }

    let total = available.len();
    let mut any_actual_updates = false;

    for (step_num, (i, pkg_count)) in available.iter().enumerate() {
        let manager = &managers[*i];
        let before = manager.list_installed().await?;

        Output::step(
            step_num + 1,
            total,
            &format!("{} ({} packages)", manager.name(), pkg_count),
        );
        manager.update_all().await?;

        any_actual_updates |= report_changes(&before, &manager.list_installed().await?);
    }

    // Update state
    let mut state = SyncState::load()?;
    let now = Utc::now();
    state.last_upgrade = Some(now);
    if any_actual_updates {
        state.last_upgrade_with_updates = Some(now);
    }
    state.save()?;

    if any_actual_updates {
        Output::success("Packages upgraded");
    } else {
        Output::info("No package version changed");
    }
    Ok(())
}

/// Print each package whose installed version changed, read again after the upgrade, so
/// the report never claims an upgrade the manager did not make. True when one changed.
fn report_changes(before: &[PackageInfo], after: &[PackageInfo]) -> bool {
    let changes = changed_versions(before, after);
    for (name, old, new) in &changes {
        Output::list_item(&format!("{} {} {} {}", name, old, Output::ARROW, new));
    }
    !changes.is_empty()
}

/// Packages installed before and after with a different version, as (name, before, after).
fn changed_versions(
    before: &[PackageInfo],
    after: &[PackageInfo],
) -> Vec<(String, String, String)> {
    let version = |p: &PackageInfo| p.version.clone().unwrap_or_else(|| "?".to_string());
    after
        .iter()
        .filter_map(|new| {
            let old = before.iter().find(|p| p.name == new.name)?;
            (old.version != new.version).then(|| (new.name.clone(), version(old), version(new)))
        })
        .collect()
}

/// Print what an upgrade of `manager` would change. True when a package would move, or when
/// the manager could not list its upgrades. A target older than the installed version
/// stays, because an upgrade never downgrades.
async fn show_plan(manager: &dyn PackageManager) -> bool {
    let candidates = match manager.upgrade_candidates().await {
        Ok(candidates) => candidates,
        Err(e) => {
            Output::warning(&format!(
                "{}: could not list upgrades: {}",
                manager.name(),
                e
            ));
            return true;
        }
    };
    let forward: Vec<_> = candidates
        .iter()
        .filter(|u| u.moves_forward(manager.ecosystem()))
        .collect();
    let kept: Vec<_> = candidates
        .iter()
        .filter(|u| u.is_downgrade(manager.ecosystem()))
        .collect();
    let held: Vec<_> = candidates.iter().filter_map(|u| u.hold_note()).collect();
    if forward.is_empty() && kept.is_empty() && held.is_empty() {
        return false;
    }
    Output::subheader(manager.name());
    for u in &forward {
        Output::list_item(&format!(
            "{} {} {} {}",
            u.name,
            u.current.as_deref().unwrap_or("?"),
            Output::ARROW,
            u.target
        ));
    }
    for u in &kept {
        Output::list_item(&format!(
            "{} stays at {}: the release-age limit allows only {}",
            u.name,
            u.current.as_deref().unwrap_or("?"),
            u.target
        ));
    }
    for note in &held {
        Output::list_item(note);
    }
    !forward.is_empty()
}

/// A manager that cannot enforce the release-age limit upgrades only when the user says so
/// in a terminal. `-y` and a run without a terminal skip it.
fn confirm_without_cooldown(manager: &str, dry_run: bool) -> Result<bool> {
    if dry_run || !Prompt::is_interactive() {
        return Ok(false);
    }
    Prompt::question(
        &format!(
            "This {} version cannot enforce packages.min_release_age_days. Upgrade {} packages anyway?",
            manager, manager
        ),
        false,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_installed_versions_that_moved_count_as_changes() {
        let p = |name: &str, version: &str| PackageInfo {
            name: name.to_string(),
            version: Some(version.to_string()),
        };
        let before = [p("pinned", "1.0"), p("moved", "1.0"), p("gone", "1.0")];
        let after = [p("pinned", "1.0"), p("moved", "2.0"), p("new", "1.0")];
        assert_eq!(
            changed_versions(&before, &after),
            [("moved".to_string(), "1.0".to_string(), "2.0".to_string())]
        );
    }
}

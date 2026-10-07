use crate::cli::output::Output;
use crate::cli::prompts::Prompt;
use crate::packages::{
    brew::BrewManager, bun::BunManager, gem::GemManager, manager::PackageManager, npm::NpmManager,
    pnpm::PnpmManager, uv::UvManager, Cooldown, PackageInfo,
};
use crate::packages::{install_upgrades, Upgrade};
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

    // One manager that fails to list or plan is skipped, so the others still upgrade
    let mut failed: Vec<&str> = Vec::new();
    let mut plans: Vec<(&dyn PackageManager, Vec<Upgrade>)> = Vec::new();
    for manager in &managers {
        let manager = manager.as_ref();
        if !manager.is_available().await {
            continue;
        }
        let packages = match manager.list_installed().await {
            Ok(packages) => packages,
            Err(e) => {
                Output::warning(&format!(
                    "{}: could not list packages: {}",
                    manager.name(),
                    e
                ));
                failed.push(manager.name());
                continue;
            }
        };
        if packages.is_empty() {
            continue;
        }
        if manager.cooldown().await == Cooldown::Unsupported
            && !confirm_without_cooldown(manager.name(), dry_run)?
        {
            Output::warning(&format!("Skipped {}", manager.name()));
            continue;
        }
        if let Err(e) = manager.refresh().await {
            Output::warning(&format!("{}: could not refresh: {}", manager.name(), e));
        }
        match show_plan(manager).await {
            Ok(planned) if planned.is_empty() => {}
            Ok(planned) => plans.push((manager, planned)),
            Err(e) => {
                Output::warning(&format!(
                    "{}: could not list upgrades: {}",
                    manager.name(),
                    e
                ));
                failed.push(manager.name());
            }
        }
    }

    if dry_run {
        Output::info("Dry run: nothing changed");
        return Ok(());
    }
    if plans.is_empty() {
        if !failed.is_empty() {
            anyhow::bail!("Could not check upgrades for: {}", failed.join(", "));
        }
        Output::success("Packages are up to date");
        return Ok(());
    }
    if !Prompt::confirm("Upgrade these packages?", true)? {
        Output::info("Upgrade cancelled");
        return Ok(());
    }

    let total = plans.len();
    let mut any_actual_updates = false;
    for (step_num, (manager, planned)) in plans.into_iter().enumerate() {
        let before = manager.installed_versions().await.unwrap_or_default();
        Output::step(
            step_num + 1,
            total,
            &format!("{} ({} upgrades)", manager.name(), planned.len()),
        );
        // Exactly the planned upgrades: a release that passes the age limit meanwhile waits
        if let Err(e) = install_upgrades(manager, planned).await {
            Output::warning(&format!("{}: {}", manager.name(), e));
            failed.push(manager.name());
        }
        match manager.installed_versions().await {
            Ok(after) => any_actual_updates |= report_changes(&before, &after),
            Err(e) => Output::warning(&format!(
                "{}: could not list versions after the upgrade: {}",
                manager.name(),
                e
            )),
        }
    }

    // State records the managers that ran, also when another one failed
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
    if !failed.is_empty() {
        anyhow::bail!("Upgrades failed for: {}", failed.join(", "));
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

/// Print what an upgrade of `manager` would change, and return the upgrades to install. A
/// target older than the installed version stays, because an upgrade never downgrades.
async fn show_plan(manager: &dyn PackageManager) -> Result<Vec<Upgrade>> {
    let candidates = manager.upgrade_candidates().await?;
    let ecosystem = manager.ecosystem();
    let (forward, rest): (Vec<Upgrade>, Vec<Upgrade>) = candidates
        .into_iter()
        .partition(|u| u.moves_forward(ecosystem));
    let kept: Vec<_> = rest.iter().filter(|u| u.is_downgrade(ecosystem)).collect();
    let held: Vec<_> = rest.iter().filter_map(|u| u.hold_note()).collect();
    if forward.is_empty() && kept.is_empty() && held.is_empty() {
        return Ok(forward);
    }
    Output::subheader(manager.name());
    for u in &forward {
        Output::list_item(&plan_line(u));
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
    Ok(forward)
}

/// One planned upgrade. A cask whose version is `latest` keeps that version, so its line
/// says that brew installs a newer build.
fn plan_line(u: &Upgrade) -> String {
    let current = u.current.as_deref().unwrap_or("?");
    if current == u.target {
        return format!("{} {} (newer build)", u.name, current);
    }
    format!("{} {} {} {}", u.name, current, Output::ARROW, u.target)
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
    fn a_latest_cask_shows_in_the_plan() {
        let latest = Upgrade::new("homebrew/cask/foo", Some("latest"), "latest");
        assert!(latest.moves_forward(crate::packages::Ecosystem::Brew));
        assert_eq!(plan_line(&latest), "homebrew/cask/foo latest (newer build)");
    }

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

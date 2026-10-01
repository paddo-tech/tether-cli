use crate::cli::output::Output;
use crate::cli::prompts::Prompt;
use crate::packages::{
    brew::BrewManager, bun::BunManager, gem::GemManager, manager::PackageManager, npm::NpmManager,
    pnpm::PnpmManager, uv::UvManager, Cooldown,
};
use crate::sync::SyncState;
use anyhow::Result;
use chrono::Utc;
use std::io::IsTerminal;

pub async fn run() -> Result<()> {
    Output::header("Upgrading packages");

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
            && !confirm_without_cooldown(manager.name())?
        {
            Output::warning(&format!("Skipped {}", manager.name()));
            continue;
        }
        available.push((i, packages.len()));
    }

    let total = available.len();
    let mut any_upgraded = false;
    let mut any_actual_updates = false;

    for (step_num, (i, pkg_count)) in available.iter().enumerate() {
        let manager = &managers[*i];
        let hash_before = manager.compute_manifest_hash().await.ok();

        Output::step(
            step_num + 1,
            total,
            &format!("{} ({} packages)", manager.name(), pkg_count),
        );
        manager.update_all().await?;
        any_upgraded = true;

        let hash_after = manager.compute_manifest_hash().await.ok();
        if hash_before != hash_after {
            any_actual_updates = true;
        }
    }

    // Update state
    let mut state = SyncState::load()?;
    let now = Utc::now();
    state.last_upgrade = Some(now);
    if any_actual_updates {
        state.last_upgrade_with_updates = Some(now);
    }
    state.save()?;

    if any_upgraded {
        Output::success("Packages upgraded");
    } else {
        Output::warning("No packages to upgrade");
    }

    Ok(())
}

/// A manager that cannot enforce the release-age limit upgrades only with the user's consent,
/// so a run without a terminal skips it.
fn confirm_without_cooldown(manager: &str) -> Result<bool> {
    if !std::io::stdin().is_terminal() {
        return Ok(false);
    }
    Prompt::confirm(
        &format!(
            "This {} version cannot enforce packages.min_release_age_days. Upgrade {} packages anyway?",
            manager, manager
        ),
        false,
    )
}

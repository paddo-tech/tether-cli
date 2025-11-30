use crate::cli::output::Output;
use crate::packages::{
    brew::BrewManager, bun::BunManager, gem::GemManager, manager::PackageManager, npm::NpmManager,
    pnpm::PnpmManager,
};
use anyhow::Result;

pub async fn run() -> Result<()> {
    Output::header("Upgrading packages");

    let managers: Vec<Box<dyn PackageManager>> = vec![
        Box::new(BrewManager::new()),
        Box::new(NpmManager::new()),
        Box::new(PnpmManager::new()),
        Box::new(BunManager::new()),
        Box::new(GemManager::new()),
    ];

    let mut any_upgraded = false;

    for manager in managers {
        if !manager.is_available().await {
            continue;
        }

        let packages = manager.list_installed().await?;
        if packages.is_empty() {
            continue;
        }

        println!("  {} ({} packages)...", manager.name(), packages.len());
        manager.update_all().await?;
        any_upgraded = true;
    }

    if any_upgraded {
        Output::success("Packages upgraded");
    } else {
        Output::warning("No packages to upgrade");
    }

    Ok(())
}

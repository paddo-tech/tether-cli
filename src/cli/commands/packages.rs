use anyhow::Result;

use crate::cli::output::Output;
use crate::cli::prompts::Prompt;
use crate::packages::inbox::{self, InboxItem, Kind, Reason};
use crate::packages::{
    BrewManager, BunManager, GemManager, NpmManager, PackageInfo, PackageManager, PnpmManager,
    UvManager,
};

struct PackageEntry {
    manager: String,
    name: String,
    version: Option<String>,
}

struct ManagerInfo {
    name: String,
    display: String,
    packages: Vec<PackageInfo>,
}

pub async fn run(list_only: bool, yes: bool) -> Result<()> {
    let managers: Vec<Box<dyn PackageManager>> = vec![
        Box::new(BrewManager::new()),
        Box::new(NpmManager::new()),
        Box::new(PnpmManager::new()),
        Box::new(BunManager::new()),
        Box::new(GemManager::new()),
        Box::new(UvManager::new()),
    ];

    // Collect packages grouped by manager
    let mut manager_infos: Vec<ManagerInfo> = Vec::new();

    for manager in &managers {
        if !manager.is_available().await {
            continue;
        }

        match manager.list_installed().await {
            Ok(packages) => {
                if !packages.is_empty() {
                    manager_infos.push(ManagerInfo {
                        name: manager.name().to_string(),
                        display: format!("{} ({} packages)", manager.name(), packages.len()),
                        packages,
                    });
                }
            }
            Err(e) => {
                Output::warning(&format!(
                    "Failed to list {} packages: {}",
                    manager.name(),
                    e
                ));
            }
        }
    }

    if manager_infos.is_empty() {
        Output::info("No packages found");
        return Ok(());
    }

    manager_infos.sort_by(|a, b| a.name.cmp(&b.name));

    if list_only {
        print_package_list(&manager_infos);
        return Ok(());
    }

    // Interactive mode: first select managers to expand
    let option_refs: Vec<&str> = manager_infos.iter().map(|m| m.display.as_str()).collect();

    let selected_indices =
        match Prompt::multi_select("Select package managers to expand:", option_refs, &[]) {
            Ok(indices) => indices,
            Err(_) => return Ok(()),
        };

    if selected_indices.is_empty() {
        Output::info("No managers selected");
        return Ok(());
    }

    // Build package list from selected managers only
    let mut all_packages: Vec<PackageEntry> = Vec::new();
    for &idx in &selected_indices {
        let info = &manager_infos[idx];
        for pkg in &info.packages {
            all_packages.push(PackageEntry {
                manager: info.name.clone(),
                name: pkg.name.clone(),
                version: pkg.version.clone(),
            });
        }
    }

    all_packages.sort_by(|a, b| (&a.manager, &a.name).cmp(&(&b.manager, &b.name)));

    // Now select packages to uninstall
    let options: Vec<String> = all_packages
        .iter()
        .map(|p| {
            let version = p.version.as_deref().unwrap_or("");
            if version.is_empty() {
                format!("[{}] {}", p.manager, p.name)
            } else {
                format!("[{}] {} ({})", p.manager, p.name, version)
            }
        })
        .collect();

    let option_refs: Vec<&str> = options.iter().map(|s| s.as_str()).collect();

    let selected = match Prompt::multi_select("Select packages to uninstall:", option_refs, &[]) {
        Ok(indices) => indices,
        Err(_) => return Ok(()),
    };

    if selected.is_empty() {
        Output::info("No packages selected");
        return Ok(());
    }

    // Process each selected package
    for idx in selected {
        let pkg = &all_packages[idx];
        uninstall_package(&managers, pkg, yes).await?;
    }

    Output::success("Uninstall complete");
    Ok(())
}

fn print_package_list(manager_infos: &[ManagerInfo]) {
    for info in manager_infos {
        Output::section(&info.name);
        for pkg in &info.packages {
            let display = match &pkg.version {
                Some(v) => format!("{} ({})", pkg.name, v),
                None => pkg.name.clone(),
            };
            Output::list_item(&display);
        }
    }
    println!();
}

async fn uninstall_package(
    managers: &[Box<dyn PackageManager>],
    pkg: &PackageEntry,
    yes: bool,
) -> Result<()> {
    let manager = managers
        .iter()
        .find(|m| m.name() == pkg.manager)
        .ok_or_else(|| anyhow::anyhow!("Manager {} not found", pkg.manager))?;

    // Check for dependents
    let dependents = manager.get_dependents(&pkg.name).await.unwrap_or_default();

    if !dependents.is_empty() {
        Output::warning(&format!(
            "{} is required by: {}",
            pkg.name,
            dependents.join(", ")
        ));

        if !yes && !Prompt::confirm(&format!("Uninstall {} anyway?", pkg.name), false)? {
            Output::dim(&format!("Skipped {}", pkg.name));
            return Ok(());
        }
    }

    // Uninstall
    match manager.uninstall(&pkg.name).await {
        Ok(()) => {
            Output::success(&format!("Uninstalled {} ({})", pkg.name, pkg.manager));
        }
        Err(e) => {
            Output::error(&format!("Failed to uninstall {}: {}", pkg.name, e));
        }
    }

    Ok(())
}

fn describe(item: &InboxItem) -> String {
    let mut text = item.id();
    if let Kind::TrustMachine { fingerprint, .. } = &item.kind {
        text.push_str(&format!(" key {}", fingerprint));
    }
    if let Some(version) = &item.version {
        text.push_str(&format!(" {}", version));
    }
    if let Some(machine) = &item.source_machine {
        text.push_str(&format!(" from {}", machine));
    }
    let reasons: Vec<&str> = item.reasons.iter().map(|r| r.label()).collect();
    text.push_str(&format!(" ({})", reasons.join(", ")));
    if !item.advisories.is_empty() {
        text.push_str(&format!(" [OSV: {}]", item.advisories.join(", ")));
    }
    text
}

/// List packages waiting for approval.
pub async fn inbox_list() -> Result<()> {
    let items = inbox::list()?;
    if items.is_empty() {
        Output::info("No packages wait for approval");
        return Ok(());
    }
    Output::section("Packages waiting for approval");
    for item in &items {
        Output::list_item(&describe(item));
    }
    Output::dim("Run 'tether packages approve <id>' or 'tether packages reject <id>'");
    Ok(())
}

/// Approve a held package and install it now, or trust a held machine key. The sync lock
/// keeps the daemon from installing the same package while this install runs.
pub async fn approve(id: &str) -> Result<()> {
    let _sync_lock = crate::sync::acquire_sync_lock(true)?;
    approve_locked(id).await
}

/// The caller holds the sync lock.
async fn approve_locked(id: &str) -> Result<()> {
    let item = inbox::approve(id)?;
    if let Kind::TrustMachine { fingerprint, .. } = &item.kind {
        Output::success(&format!(
            "Trusted machine {} with key {}",
            item.name, fingerprint
        ));
        return Ok(());
    }
    Output::info(&format!("Approved {}. Installing...", item.id()));
    inbox::install(&item, true).await?;
    Output::success(&format!("Installed {}", item.name));
    Ok(())
}

/// Reject a held package so later syncs do not offer it again.
pub async fn reject(id: &str) -> Result<()> {
    let item = inbox::reject(id)?;
    Output::success(&format!("Rejected {}", item.id()));
    Ok(())
}

/// Ask about each held package. Only a terminal user can answer, so callers check for one.
/// The caller holds the sync lock.
pub async fn review_inbox() -> Result<()> {
    let items = inbox::list()?;
    if items.is_empty() {
        return Ok(());
    }
    Output::section("Packages waiting for approval");
    for item in &items {
        Output::list_item(&describe(item));
    }
    // A new machine can inherit hundreds of packages, so one answer can cover them all
    if items.len() > 1 {
        let options = vec!["Review each", "Install all", "Decide later"];
        match Prompt::select("Install these packages?", options, 0)? {
            0 => {}
            1 => {
                // A machine key needs its own answer, so "Install all" leaves it pending
                for item in items {
                    if item.reasons.contains(&Reason::Malicious) || item.kind != Kind::Package {
                        continue;
                    }
                    if let Err(e) = approve_locked(&item.id()).await {
                        Output::warning(&format!("{}: {}", item.name, e));
                    }
                }
                return Ok(());
            }
            _ => return Ok(()),
        }
    }
    for item in items {
        let options = if item.reasons.contains(&Reason::Malicious) {
            vec!["Reject", "Decide later"]
        } else if item.kind != Kind::Package {
            vec!["Trust", "Reject", "Decide later"]
        } else {
            vec!["Install", "Reject", "Decide later"]
        };
        let choice = options[Prompt::select(&describe(&item), options.clone(), options.len() - 1)?];
        let result = match choice {
            "Install" | "Trust" => approve_locked(&item.id()).await,
            "Reject" => reject(&item.id()).await,
            _ => Ok(()),
        };
        if let Err(e) = result {
            Output::warning(&format!("{}: {}", item.name, e));
        }
    }
    Ok(())
}

use anyhow::Result;
use std::collections::HashMap;

use crate::cli::output::Output;
use crate::cli::prompts::Prompt;
use crate::packages::{
    BrewManager, BunManager, GemManager, NpmManager, PackageInfo, PackageManager, PnpmManager,
    UvManager,
};

struct PackageEntry {
    manager: String,
    name: String,
    version: Option<String>,
}

pub async fn run(list_only: bool) -> Result<()> {
    let managers: Vec<Box<dyn PackageManager>> = vec![
        Box::new(BrewManager::new()),
        Box::new(NpmManager::new()),
        Box::new(PnpmManager::new()),
        Box::new(BunManager::new()),
        Box::new(GemManager::new()),
        Box::new(UvManager::new()),
    ];

    // Collect packages grouped by manager
    let mut packages_by_manager: HashMap<String, Vec<PackageInfo>> = HashMap::new();

    for manager in &managers {
        if !manager.is_available().await {
            continue;
        }

        match manager.list_installed().await {
            Ok(packages) => {
                if !packages.is_empty() {
                    packages_by_manager.insert(manager.name().to_string(), packages);
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

    if packages_by_manager.is_empty() {
        Output::info("No packages found");
        return Ok(());
    }

    if list_only {
        print_package_list(&packages_by_manager);
        return Ok(());
    }

    // Interactive mode: first select managers to expand
    let mut manager_options: Vec<String> = packages_by_manager
        .iter()
        .map(|(name, pkgs)| format!("{} ({} packages)", name, pkgs.len()))
        .collect();
    manager_options.sort();

    let option_refs: Vec<&str> = manager_options.iter().map(|s| s.as_str()).collect();

    let selected_managers =
        match Prompt::multi_select("Select package managers to expand:", option_refs, &[]) {
            Ok(indices) => indices,
            Err(_) => return Ok(()),
        };

    if selected_managers.is_empty() {
        Output::info("No managers selected");
        return Ok(());
    }

    // Get selected manager names
    let selected_names: Vec<String> = selected_managers
        .iter()
        .map(|&i| {
            manager_options[i]
                .split(' ')
                .next()
                .unwrap_or("")
                .to_string()
        })
        .collect();

    // Build package list from selected managers only
    let mut all_packages: Vec<PackageEntry> = Vec::new();
    for name in &selected_names {
        if let Some(pkgs) = packages_by_manager.get(name) {
            for pkg in pkgs {
                all_packages.push(PackageEntry {
                    manager: name.clone(),
                    name: pkg.name.clone(),
                    version: pkg.version.clone(),
                });
            }
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
        uninstall_package(&managers, pkg).await?;
    }

    Output::success("Uninstall complete");
    Ok(())
}

fn print_package_list(packages_by_manager: &HashMap<String, Vec<PackageInfo>>) {
    let mut managers: Vec<_> = packages_by_manager.keys().collect();
    managers.sort();

    for manager in managers {
        if let Some(packages) = packages_by_manager.get(manager) {
            Output::section(manager);
            for pkg in packages {
                let display = match &pkg.version {
                    Some(v) => format!("{} ({})", pkg.name, v),
                    None => pkg.name.clone(),
                };
                Output::list_item(&display);
            }
        }
    }
    println!();
}

async fn uninstall_package(managers: &[Box<dyn PackageManager>], pkg: &PackageEntry) -> Result<()> {
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

        if !Prompt::confirm(&format!("Uninstall {} anyway?", pkg.name), false)? {
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

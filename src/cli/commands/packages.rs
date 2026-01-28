use anyhow::Result;

use crate::cli::output::Output;
use crate::cli::prompts::Prompt;
use crate::packages::{
    BrewManager, BunManager, GemManager, NpmManager, PackageManager, PnpmManager, UvManager,
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

    // Collect all packages
    let mut all_packages: Vec<PackageEntry> = Vec::new();

    for manager in &managers {
        if !manager.is_available().await {
            continue;
        }

        match manager.list_installed().await {
            Ok(packages) => {
                for pkg in packages {
                    all_packages.push(PackageEntry {
                        manager: manager.name().to_string(),
                        name: pkg.name,
                        version: pkg.version,
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

    if all_packages.is_empty() {
        Output::info("No packages found");
        return Ok(());
    }

    // Sort by manager, then name
    all_packages.sort_by(|a, b| (&a.manager, &a.name).cmp(&(&b.manager, &b.name)));

    if list_only {
        print_package_list(&all_packages);
        return Ok(());
    }

    // Interactive mode
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
        Err(_) => {
            // User cancelled
            return Ok(());
        }
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

fn print_package_list(packages: &[PackageEntry]) {
    let mut current_manager = "";

    for pkg in packages {
        if pkg.manager != current_manager {
            Output::section(&pkg.manager);
            current_manager = &pkg.manager;
        }

        let display = match &pkg.version {
            Some(v) => format!("{} ({})", pkg.name, v),
            None => pkg.name.clone(),
        };
        Output::list_item(&display);
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

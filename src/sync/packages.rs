use crate::cli::Output;
use crate::config::Config;
use crate::packages::{
    normalize_formula_name, BrewManager, BrewfilePackages, BunManager, GemManager, NpmManager,
    PackageManager, PnpmManager,
};
use crate::sync::state::PackageState;
use crate::sync::{MachineState, SyncState};
use anyhow::Result;
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};
use std::path::Path;

/// Definition of a package manager for sync purposes
struct PackageManagerDef {
    /// Key used in machine state (e.g., "npm", "brew_formulae")
    state_key: &'static str,
    /// Display name for user messages
    display_name: &'static str,
    /// Manifest filename
    manifest_file: &'static str,
}

const SIMPLE_MANAGERS: &[PackageManagerDef] = &[
    PackageManagerDef {
        state_key: "npm",
        display_name: "npm",
        manifest_file: "npm.txt",
    },
    PackageManagerDef {
        state_key: "pnpm",
        display_name: "pnpm",
        manifest_file: "pnpm.txt",
    },
    PackageManagerDef {
        state_key: "bun",
        display_name: "bun",
        manifest_file: "bun.txt",
    },
    PackageManagerDef {
        state_key: "gem",
        display_name: "gem",
        manifest_file: "gems.txt",
    },
];

/// Import packages from manifests, installing only missing packages
pub async fn import_packages(
    config: &Config,
    sync_path: &Path,
    machine_state: &MachineState,
) -> Result<()> {
    let manifests_dir = sync_path.join("manifests");
    if !manifests_dir.exists() {
        return Ok(());
    }

    // Homebrew - special handling for formulae/casks/taps
    if config.packages.brew.enabled {
        import_brew(&manifests_dir, machine_state).await;
    }

    // Simple package managers (npm, pnpm, bun, gem)
    for def in SIMPLE_MANAGERS {
        let enabled = match def.state_key {
            "npm" => config.packages.npm.enabled,
            "pnpm" => config.packages.pnpm.enabled,
            "bun" => config.packages.bun.enabled,
            "gem" => config.packages.gem.enabled,
            _ => false,
        };

        if enabled {
            import_simple_manager(def, &manifests_dir, machine_state).await;
        }
    }

    Ok(())
}

/// Import brew packages (formulae, casks, taps)
async fn import_brew(manifests_dir: &Path, machine_state: &MachineState) {
    let brewfile = manifests_dir.join("Brewfile");
    if !brewfile.exists() {
        return;
    }

    let brew = BrewManager::new();
    if !brew.is_available().await {
        return;
    }

    let manifest = match std::fs::read_to_string(&brewfile) {
        Ok(m) => m,
        Err(_) => return,
    };

    // Parse the Brewfile
    let mut brew_packages = BrewfilePackages::parse(&manifest);

    // Filter out removed packages
    let removed_formulae: HashSet<_> = machine_state
        .removed_packages
        .get("brew_formulae")
        .map(|v| v.iter().collect())
        .unwrap_or_default();
    let removed_casks: HashSet<_> = machine_state
        .removed_packages
        .get("brew_casks")
        .map(|v| v.iter().collect())
        .unwrap_or_default();
    let removed_taps: HashSet<_> = machine_state
        .removed_packages
        .get("brew_taps")
        .map(|v| v.iter().collect())
        .unwrap_or_default();

    brew_packages
        .formulae
        .retain(|p| !removed_formulae.contains(p));
    brew_packages.casks.retain(|p| !removed_casks.contains(p));
    brew_packages.taps.retain(|p| !removed_taps.contains(p));

    // Calculate missing packages (normalize formula names for comparison)
    let local_formulae: HashSet<_> = machine_state
        .packages
        .get("brew_formulae")
        .map(|v| v.iter().map(|s| s.as_str()).collect())
        .unwrap_or_default();
    let local_casks: HashSet<_> = machine_state
        .packages
        .get("brew_casks")
        .map(|v| v.iter().map(|s| s.as_str()).collect())
        .unwrap_or_default();

    // Compare using normalized names (strip tap prefix like "oven-sh/bun/bun" -> "bun")
    let missing_formulae: Vec<_> = brew_packages
        .formulae
        .iter()
        .filter(|p| !local_formulae.contains(normalize_formula_name(p)))
        .collect();
    let missing_casks: Vec<_> = brew_packages
        .casks
        .iter()
        .filter(|p| !local_casks.contains(p.as_str()))
        .collect();

    let total_missing = missing_formulae.len() + missing_casks.len();
    if total_missing == 0 {
        return;
    }

    let missing_names: Vec<_> = missing_formulae
        .iter()
        .chain(missing_casks.iter())
        .map(|s| s.as_str())
        .collect();
    Output::info(&format!("Installing {} brew package{}: {}",
        total_missing,
        if total_missing == 1 { "" } else { "s" },
        missing_names.join(", ")
    ));
    if !missing_casks.is_empty() {
        Output::info("Casks may prompt for your password");
    }

    // Generate filtered manifest and import
    let filtered_manifest = brew_packages.generate();
    if let Err(e) = brew.import_manifest(&filtered_manifest).await {
        Output::warning(&format!("Failed to import Brewfile: {}", e));
    }
}

/// Import a simple package manager (one package per line manifest)
async fn import_simple_manager(
    def: &PackageManagerDef,
    manifests_dir: &Path,
    machine_state: &MachineState,
) {
    let manifest_path = manifests_dir.join(def.manifest_file);
    if !manifest_path.exists() {
        return;
    }

    // Get the appropriate manager
    let manager: Box<dyn PackageManager> = match def.state_key {
        "npm" => Box::new(NpmManager::new()),
        "pnpm" => Box::new(PnpmManager::new()),
        "bun" => Box::new(BunManager::new()),
        "gem" => Box::new(GemManager::new()),
        _ => return,
    };

    if !manager.is_available().await {
        return;
    }

    let manifest = match std::fs::read_to_string(&manifest_path) {
        Ok(m) => m,
        Err(_) => return,
    };

    let local_packages: HashSet<_> = machine_state
        .packages
        .get(def.state_key)
        .map(|v| v.iter().cloned().collect())
        .unwrap_or_default();

    let removed_packages: HashSet<_> = machine_state
        .removed_packages
        .get(def.state_key)
        .map(|v| v.iter().cloned().collect())
        .unwrap_or_default();

    // Filter to only missing packages
    let missing: Vec<_> = manifest
        .lines()
        .filter(|line| {
            let pkg = line.trim();
            !pkg.is_empty() && !removed_packages.contains(pkg) && !local_packages.contains(pkg)
        })
        .map(|s| s.to_string())
        .collect();

    if missing.is_empty() {
        return;
    }

    Output::info(&format!(
        "Installing {} {} package{}...",
        missing.len(),
        def.display_name,
        if missing.len() == 1 { "" } else { "s" }
    ));

    let filtered_manifest = missing.join("\n") + "\n";

    if let Err(e) = manager.import_manifest(&filtered_manifest).await {
        Output::warning(&format!(
            "Failed to import {}: {}",
            manifest_path.display(),
            e
        ));
    }
}

/// Export package manifests using union of all machine states
pub async fn sync_packages(
    config: &Config,
    state: &mut SyncState,
    sync_path: &Path,
    machine_state: &MachineState,
    dry_run: bool,
) -> Result<()> {
    let manifests_dir = sync_path.join("manifests");
    std::fs::create_dir_all(&manifests_dir)?;

    // Load all machine states and compute union of packages
    let mut machines = MachineState::list_all(sync_path)?;

    // Update/add current machine's state in the list for union computation
    if let Some(pos) = machines
        .iter()
        .position(|m| m.machine_id == machine_state.machine_id)
    {
        machines[pos] = machine_state.clone();
    } else {
        machines.push(machine_state.clone());
    }

    let union_packages = MachineState::compute_union_packages(&machines);

    // Homebrew - generate manifest from union
    if config.packages.brew.enabled {
        sync_brew(&union_packages, state, &manifests_dir, dry_run)?;
    }

    // Simple package managers
    for def in SIMPLE_MANAGERS {
        let enabled = match def.state_key {
            "npm" => config.packages.npm.enabled,
            "pnpm" => config.packages.pnpm.enabled,
            "bun" => config.packages.bun.enabled,
            "gem" => config.packages.gem.enabled,
            _ => false,
        };

        if enabled {
            sync_simple_manager(def, &union_packages, state, &manifests_dir, dry_run)?;
        }
    }

    Ok(())
}

/// Sync brew manifest from union
fn sync_brew(
    union_packages: &HashMap<String, Vec<String>>,
    state: &mut SyncState,
    manifests_dir: &Path,
    dry_run: bool,
) -> Result<()> {
    let brew_packages = BrewfilePackages {
        taps: union_packages.get("brew_taps").cloned().unwrap_or_default(),
        formulae: union_packages
            .get("brew_formulae")
            .cloned()
            .unwrap_or_default(),
        casks: union_packages
            .get("brew_casks")
            .cloned()
            .unwrap_or_default(),
    };

    let manifest = brew_packages.generate();
    let hash = format!("{:x}", Sha256::digest(manifest.as_bytes()));
    let manifest_path = manifests_dir.join("Brewfile");

    let file_hash = std::fs::read(&manifest_path)
        .ok()
        .map(|c| format!("{:x}", Sha256::digest(&c)));
    let changed = file_hash.as_ref() != Some(&hash);

    if changed && !dry_run {
        std::fs::write(&manifest_path, &manifest)?;
        state.packages.insert(
            "brew".to_string(),
            PackageState {
                last_sync: chrono::Utc::now(),
                hash,
            },
        );
    }

    Ok(())
}

/// Sync a simple package manager manifest from union
fn sync_simple_manager(
    def: &PackageManagerDef,
    union_packages: &HashMap<String, Vec<String>>,
    state: &mut SyncState,
    manifests_dir: &Path,
    dry_run: bool,
) -> Result<()> {
    let packages = union_packages
        .get(def.state_key)
        .cloned()
        .unwrap_or_default();
    let manifest = if packages.is_empty() {
        String::new()
    } else {
        packages.join("\n") + "\n"
    };
    let hash = format!("{:x}", Sha256::digest(manifest.as_bytes()));
    let manifest_path = manifests_dir.join(def.manifest_file);

    let file_hash = std::fs::read(&manifest_path)
        .ok()
        .map(|c| format!("{:x}", Sha256::digest(&c)));
    let changed = file_hash.as_ref() != Some(&hash);

    if changed && !dry_run {
        std::fs::write(&manifest_path, &manifest)?;
        state.packages.insert(
            def.state_key.to_string(),
            PackageState {
                last_sync: chrono::Utc::now(),
                hash,
            },
        );
    }

    Ok(())
}

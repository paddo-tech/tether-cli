use crate::cli::Output;
use crate::config::Config;
use crate::packages::{
    normalize_formula_name, BrewManager, BrewfilePackages, BunManager, GemManager, NpmManager,
    PackageManager, PnpmManager, UvManager,
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
    PackageManagerDef {
        state_key: "uv",
        display_name: "uv",
        manifest_file: "uv.txt",
    },
];

/// Import packages from manifests, installing only missing packages.
/// In daemon mode, casks are deferred (require password).
/// Returns list of deferred casks (empty if not in daemon mode).
pub async fn import_packages(
    config: &Config,
    sync_path: &Path,
    state: &mut SyncState,
    machine_state: &MachineState,
    daemon_mode: bool,
    previously_deferred: &[String],
) -> Result<Vec<String>> {
    let manifests_dir = sync_path.join("manifests");
    if !manifests_dir.exists() {
        return Ok(Vec::new());
    }

    let mut deferred_casks = Vec::new();

    // Homebrew - special handling for formulae/casks/taps
    if config.packages.brew.enabled {
        let (casks, installed) = import_brew(
            &manifests_dir,
            machine_state,
            daemon_mode,
            previously_deferred,
        )
        .await;
        deferred_casks = casks;

        if installed {
            update_last_upgrade(state, "brew");
        }
    }

    // Simple package managers (npm, pnpm, bun, gem)
    for def in SIMPLE_MANAGERS {
        let enabled = match def.state_key {
            "npm" => config.packages.npm.enabled,
            "pnpm" => config.packages.pnpm.enabled,
            "bun" => config.packages.bun.enabled,
            "gem" => config.packages.gem.enabled,
            "uv" => config.packages.uv.enabled,
            _ => false,
        };

        if enabled {
            let installed = import_simple_manager(def, &manifests_dir, machine_state).await;
            if installed {
                update_last_upgrade(state, def.state_key);
            }
        }
    }

    Ok(deferred_casks)
}

/// Update last_upgrade timestamp for a package manager
fn update_last_upgrade(state: &mut SyncState, manager: &str) {
    let now = chrono::Utc::now();
    state
        .packages
        .entry(manager.to_string())
        .and_modify(|e| e.last_upgrade = Some(now))
        .or_insert_with(|| crate::sync::state::PackageState {
            last_sync: now,
            last_modified: None,
            last_upgrade: Some(now),
            hash: String::new(),
        });
}

/// Import brew packages (formulae, casks, taps).
/// Casks are installed individually to detect which need password.
/// Returns (deferred_casks, installed_any) - list of casks needing password and whether any packages were installed.
async fn import_brew(
    manifests_dir: &Path,
    machine_state: &MachineState,
    daemon_mode: bool,
    previously_deferred: &[String],
) -> (Vec<String>, bool) {
    let brewfile = manifests_dir.join("Brewfile");
    if !brewfile.exists() {
        return (Vec::new(), false);
    }

    let brew = BrewManager::new();
    if !brew.is_available().await {
        return (Vec::new(), false);
    }

    let manifest = match std::fs::read_to_string(&brewfile) {
        Ok(m) => m,
        Err(_) => return (Vec::new(), false),
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
        .cloned()
        .collect();

    // Collect casks to install: missing + previously deferred that still need install
    let mut casks_to_try: Vec<_> = brew_packages
        .casks
        .iter()
        .filter(|p| !local_casks.contains(p.as_str()))
        .cloned()
        .collect();

    for deferred in previously_deferred {
        if !local_casks.contains(deferred.as_str())
            && !casks_to_try.contains(deferred)
            && !removed_casks.contains(deferred)
        {
            casks_to_try.push(deferred.clone());
        }
    }

    let mut installed_any = false;

    // Install formulae via bundle (no password needed)
    if !missing_formulae.is_empty() {
        Output::info(&format!(
            "Installing {} brew formula{}: {}",
            missing_formulae.len(),
            if missing_formulae.len() == 1 { "" } else { "e" },
            missing_formulae.join(", ")
        ));

        // Explicitly tap any missing taps before bundle install
        // (brew bundle sometimes fails to tap before installing)
        if let Ok(local_taps) = brew.list_taps().await {
            let local_taps_set: HashSet<_> = local_taps.iter().map(|s| s.as_str()).collect();
            for tap in &brew_packages.taps {
                if !local_taps_set.contains(tap.as_str()) {
                    if let Err(e) = brew.tap(tap).await {
                        Output::warning(&format!("Failed to tap {}: {}", tap, e));
                    }
                }
            }
        }

        let formulae_manifest = BrewfilePackages {
            taps: brew_packages.taps,
            formulae: missing_formulae,
            casks: Vec::new(),
        };
        if brew.import_manifest(&formulae_manifest.generate()).await.is_ok() {
            installed_any = true;
        }
    }

    // Install casks one-by-one to detect which need password
    let mut flagged_casks = Vec::new();

    if !casks_to_try.is_empty() {
        Output::info(&format!(
            "Installing {} cask{}: {}",
            casks_to_try.len(),
            if casks_to_try.len() == 1 { "" } else { "s" },
            casks_to_try.join(", ")
        ));
        if !daemon_mode {
            Output::info("Casks may prompt for your password");
        }

        for cask in &casks_to_try {
            match brew.install_cask(cask, !daemon_mode).await {
                Ok(true) => {
                    installed_any = true;
                }
                Ok(false) => {
                    if daemon_mode {
                        // Daemon: needs password - flag for manual sync
                        Output::info(&format!(
                            "Cask {} requires password, flagged for manual sync",
                            cask
                        ));
                        flagged_casks.push(cask.clone());
                    } else {
                        // Interactive: user had their chance, just log failure
                        Output::warning(&format!("Failed to install cask {}", cask));
                    }
                }
                Err(e) => {
                    Output::warning(&format!("Failed to install cask {}: {}", cask, e));
                }
            }
        }
    }

    (flagged_casks, installed_any)
}

/// Import a simple package manager (one package per line manifest)
/// Returns true if any packages were installed.
async fn import_simple_manager(
    def: &PackageManagerDef,
    manifests_dir: &Path,
    machine_state: &MachineState,
) -> bool {
    let manifest_path = manifests_dir.join(def.manifest_file);
    if !manifest_path.exists() {
        return false;
    }

    // Get the appropriate manager
    let manager: Box<dyn PackageManager> = match def.state_key {
        "npm" => Box::new(NpmManager::new()),
        "pnpm" => Box::new(PnpmManager::new()),
        "bun" => Box::new(BunManager::new()),
        "gem" => Box::new(GemManager::new()),
        "uv" => Box::new(UvManager::new()),
        _ => return false,
    };

    if !manager.is_available().await {
        return false;
    }

    let manifest = match std::fs::read_to_string(&manifest_path) {
        Ok(m) => m,
        Err(_) => return false,
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
        return false;
    }

    Output::info(&format!(
        "Installing {} {} package{}...",
        missing.len(),
        def.display_name,
        if missing.len() == 1 { "" } else { "s" }
    ));

    let filtered_manifest = missing.join("\n") + "\n";

    match manager.import_manifest(&filtered_manifest).await {
        Ok(_) => true,
        Err(e) => {
            Output::warning(&format!(
                "Failed to import {}: {}",
                manifest_path.display(),
                e
            ));
            false
        }
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
            "uv" => config.packages.uv.enabled,
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

    if !dry_run {
        let now = chrono::Utc::now();
        let existing = state.packages.get("brew");

        if changed {
            std::fs::write(&manifest_path, &manifest)?;
        }

        state.packages.insert(
            "brew".to_string(),
            PackageState {
                last_sync: now,
                last_modified: if changed {
                    Some(now)
                } else {
                    existing.and_then(|e| e.last_modified)
                },
                last_upgrade: existing.and_then(|e| e.last_upgrade),
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

    if !dry_run {
        let now = chrono::Utc::now();
        let existing = state.packages.get(def.state_key);

        if changed {
            std::fs::write(&manifest_path, &manifest)?;
        }

        state.packages.insert(
            def.state_key.to_string(),
            PackageState {
                last_sync: now,
                last_modified: if changed {
                    Some(now)
                } else {
                    existing.and_then(|e| e.last_modified)
                },
                last_upgrade: existing.and_then(|e| e.last_upgrade),
                hash,
            },
        );
    }

    Ok(())
}

use crate::cli::Output;
use crate::config::Config;
use crate::packages::inbox::{self, Checks, Inbox, InboxItem, Reason};
use crate::packages::osv;
use crate::packages::pin::{format_pin, parse_pin};
use crate::packages::{
    normalize_formula_name, BrewManager, BrewfilePackages, Cooldown, Ecosystem, PackageManager,
    PackagePolicy,
};
use crate::sync::state::PackageState;
use crate::sync::{GitBackend, MachineState, SyncState};
use anyhow::Result;
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
    /// Naming and version-pin syntax of the manifest lines
    ecosystem: Ecosystem,
}

/// Map a machine-state package key to its manifest filename in `manifests/`.
/// All three brew keys (`brew_formulae`, `brew_casks`, `brew_taps`) share the Brewfile.
pub fn manifest_filename(state_key: &str) -> Option<&'static str> {
    if state_key.starts_with("brew_") {
        return Some("Brewfile");
    }
    SIMPLE_MANAGERS
        .iter()
        .find(|d| d.state_key == state_key)
        .map(|d| d.manifest_file)
}

const SIMPLE_MANAGERS: &[PackageManagerDef] = &[
    PackageManagerDef {
        state_key: "npm",
        display_name: "npm",
        manifest_file: "npm.txt",
        ecosystem: Ecosystem::Npm,
    },
    PackageManagerDef {
        state_key: "pnpm",
        display_name: "pnpm",
        manifest_file: "pnpm.txt",
        ecosystem: Ecosystem::Npm,
    },
    PackageManagerDef {
        state_key: "bun",
        display_name: "bun",
        manifest_file: "bun.txt",
        ecosystem: Ecosystem::Npm,
    },
    PackageManagerDef {
        state_key: "gem",
        display_name: "gem",
        manifest_file: "gems.txt",
        ecosystem: Ecosystem::Gem,
    },
    PackageManagerDef {
        state_key: "uv",
        display_name: "uv",
        manifest_file: "uv.txt",
        ecosystem: Ecosystem::Python,
    },
];

/// What an import did beyond installing.
#[derive(Debug, Default)]
pub struct ImportOutcome {
    /// Casks that need a password; only the daemon defers them
    pub deferred_casks: Vec<String>,
    /// Packages newly held in the approval inbox
    pub queued: Vec<InboxItem>,
}

/// Import packages from manifests, installing only missing packages that pass the
/// trust checks. Others go to the approval inbox. In daemon mode, casks are deferred
/// (require password).
pub async fn import_packages(
    config: &Config,
    sync_path: &Path,
    state: &mut SyncState,
    machine_state: &MachineState,
    daemon_mode: bool,
    previously_deferred: &[String],
) -> Result<ImportOutcome> {
    let mut outcome = ImportOutcome::default();
    let manifests_dir = sync_path.join("manifests");
    if !manifests_dir.exists() {
        return Ok(outcome);
    }

    prune_inbox(machine_state)?;
    let trust = Trust {
        inbox: Inbox::load()?,
        provenance: Provenance::load(sync_path, &machine_state.machine_id),
        auto_install_from_trusted: config.packages.auto_install_from_trusted,
    };

    let mid = &machine_state.machine_id;

    // Homebrew - special handling for formulae/casks/taps
    if config.is_manager_enabled(mid, "brew") {
        let (casks, installed) = import_brew(
            &manifests_dir,
            machine_state,
            daemon_mode,
            previously_deferred,
            &trust,
            &mut outcome.queued,
        )
        .await;
        outcome.deferred_casks = casks;

        if installed {
            update_last_upgrade(state, "brew");
        }
    }

    // Simple package managers (npm, pnpm, bun, gem)
    for def in SIMPLE_MANAGERS {
        if config.is_manager_enabled(mid, def.state_key) {
            let installed = import_simple_manager(
                def,
                &manifests_dir,
                machine_state,
                &trust,
                &mut outcome.queued,
            )
            .await;
            if installed {
                update_last_upgrade(state, def.state_key);
            }
        }
    }

    outcome.queued = inbox::add(outcome.queued)?;
    report_held(&outcome.queued);

    Ok(outcome)
}

fn report_held(items: &[InboxItem]) {
    for item in items {
        Output::warning(&format!(
            "Holding {} ({}) for approval: {}. Run 'tether packages inbox'",
            item.name,
            item.manager,
            item.reasons
                .iter()
                .map(|r| r.label())
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
}

/// Pending items for packages the user has since installed are settled.
fn prune_inbox(machine_state: &MachineState) -> Result<()> {
    let installed = |manager: &str, name: &str| {
        machine_state.packages.get(manager).is_some_and(|names| {
            names.iter().any(|n| {
                n == name || (manager.starts_with("brew_") && n == normalize_formula_name(name))
            })
        })
    };
    let inbox = Inbox::load()?;
    if inbox.items.iter().any(|i| installed(&i.manager, &i.name)) {
        Inbox::update(|inbox| {
            for manager in machine_state.packages.keys() {
                inbox.prune_installed(manager, |name| installed(manager, name));
            }
            Ok(())
        })?;
    }
    Ok(())
}

/// Trust inputs shared by every manager in one import.
struct Trust {
    inbox: Inbox,
    provenance: Provenance,
    auto_install_from_trusted: bool,
}

impl Trust {
    /// True when the package was already decided on: pending, or rejected.
    /// Items held as malicious are checked again, because approval cannot clear them and
    /// an unpinned query can match a report that covers only some releases.
    fn settled(&self, manager: &str, name: &str) -> bool {
        self.inbox.items.iter().any(|i| {
            i.manager == manager && i.name == name && !i.reasons.contains(&Reason::Malicious)
        }) || self.inbox.is_rejected(manager, name)
    }

    fn checks(&self, manager: &str, name: &str) -> Checks {
        Checks {
            from_this_machine: self.provenance.listed_here(manager, name),
            approved: self.inbox.is_approved(manager, name),
            auto_install_from_trusted: self.auto_install_from_trusted,
            ..Checks::default()
        }
    }

    fn item(
        &self,
        manager: &str,
        name: &str,
        version: Option<String>,
        tap: Option<String>,
        reasons: Vec<Reason>,
    ) -> InboxItem {
        let (source_machine, commit) = self.provenance.source(manager, name);
        InboxItem {
            manager: manager.to_string(),
            name: name.to_string(),
            version,
            tap,
            source_machine,
            commit,
            reasons,
            advisories: Vec::new(),
            first_seen: chrono::Utc::now(),
        }
    }
}

/// Which machines list a package, and the commit that last changed the source machine's
/// state. Commit signatures will be checked against that commit.
struct Provenance {
    machines: Vec<MachineState>,
    this_machine: String,
    sync_path: std::path::PathBuf,
}

impl Provenance {
    fn load(sync_path: &Path, this_machine: &str) -> Self {
        Self {
            machines: MachineState::list_all(sync_path).unwrap_or_default(),
            this_machine: this_machine.to_string(),
            sync_path: sync_path.to_path_buf(),
        }
    }

    fn lists(machine: &MachineState, manager: &str, name: &str) -> bool {
        machine
            .packages
            .get(manager)
            .is_some_and(|names| names.iter().any(|n| n == name))
    }

    fn listed_here(&self, manager: &str, name: &str) -> bool {
        self.machines
            .iter()
            .any(|m| m.machine_id == self.this_machine && Self::lists(m, manager, name))
    }

    fn source(&self, manager: &str, name: &str) -> (Option<String>, Option<String>) {
        let Some(machine) = self
            .machines
            .iter()
            .filter(|m| m.machine_id != self.this_machine && Self::lists(m, manager, name))
            .max_by_key(|m| m.last_sync)
        else {
            return (None, None);
        };
        let commit = GitBackend::new(self.sync_path.clone())
            .file_log(&format!("machines/{}.json", machine.machine_id), 1)
            .ok()
            .and_then(|log| log.into_iter().next())
            .map(|entry| entry.commit_hash);
        (Some(machine.machine_id.clone()), commit)
    }
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
    trust: &Trust,
    queued: &mut Vec<InboxItem>,
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
    brew_packages.retain_valid();

    // Untrusted taps that are not tapped yet wait for approval
    let policy = PackagePolicy::load();
    let local_taps: HashSet<String> = brew
        .list_taps()
        .await
        .map(|t| t.into_iter().collect())
        .unwrap_or_default();
    let (trusted_taps, untrusted_taps): (Vec<String>, Vec<String>) = brew_packages
        .taps
        .drain(..)
        .partition(|t| policy.tap_trusted(t));
    for tap in untrusted_taps {
        if !local_taps.contains(&tap) && !trust.settled("brew_taps", &tap) {
            queued.push(trust.item("brew_taps", &tap, None, None, vec![Reason::UntrustedTap]));
        }
    }
    brew_packages.taps = trusted_taps;

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

    let missing_formulae = gate_brew(
        &brew,
        &policy,
        trust,
        "brew_formulae",
        missing_formulae,
        queued,
    )
    .await;
    let casks_to_try = gate_brew(&brew, &policy, trust, "brew_casks", casks_to_try, queued).await;

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
        for tap in &brew_packages.taps {
            if !local_taps.contains(tap) {
                if let Err(e) = brew.tap(tap).await {
                    Output::warning(&format!("Failed to tap {}: {}", tap, e));
                }
            }
        }

        let formulae_manifest = BrewfilePackages {
            taps: brew_packages.taps,
            formulae: missing_formulae,
            casks: Vec::new(),
        };
        if brew
            .import_manifest(&formulae_manifest.generate())
            .await
            .is_ok()
        {
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

/// Keep the formulae or casks that may install and queue the rest. A short name is
/// looked up, because brew resolves it to whichever tapped repository has it.
async fn gate_brew(
    brew: &BrewManager,
    policy: &PackagePolicy,
    trust: &Trust,
    manager: &str,
    names: Vec<String>,
    queued: &mut Vec<InboxItem>,
) -> Vec<String> {
    let mut allowed = Vec::new();
    for name in names {
        if trust.settled(manager, &name) {
            continue;
        }
        let mut checks = trust.checks(manager, &name);
        let mut tap = None;
        if !checks.approved {
            tap = brew.tap_for(&name, manager == "brew_casks").await;
            checks.untrusted_tap = !tap.as_deref().is_some_and(|t| policy.tap_trusted(t));
        }
        let reasons = inbox::reasons(checks);
        if reasons.is_empty() {
            allowed.push(name);
        } else {
            let tap = tap.filter(|_| checks.untrusted_tap);
            queued.push(trust.item(manager, &name, None, tap, reasons));
        }
    }
    allowed
}

/// Import a simple package manager (one package per line manifest)
/// Returns true if any packages were installed.
async fn import_simple_manager(
    def: &PackageManagerDef,
    manifests_dir: &Path,
    machine_state: &MachineState,
    trust: &Trust,
    queued: &mut Vec<InboxItem>,
) -> bool {
    let manifest_path = manifests_dir.join(def.manifest_file);
    if !manifest_path.exists() {
        return false;
    }

    let Some(manager) = crate::packages::manager_for_key(def.state_key) else {
        return false;
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

    // Filter to only missing packages that nobody has decided on yet
    let missing: Vec<(String, Option<String>, String)> = manifest
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(|line| {
            let (name, version) = parse_pin(def.ecosystem, line);
            (name, version, line.to_string())
        })
        .filter(|(name, _, _)| {
            !removed_packages.contains(name)
                && !local_packages.contains(name)
                && !trust.settled(def.state_key, name)
        })
        .collect();

    if missing.is_empty() {
        return false;
    }

    let allowed = gate_simple(def, manager.as_ref(), trust, missing, queued).await;

    if allowed.is_empty() {
        return false;
    }

    Output::info(&format!(
        "Installing {} {} package{}...",
        allowed.len(),
        def.display_name,
        if allowed.len() == 1 { "" } else { "s" }
    ));

    let filtered_manifest = allowed.join("\n") + "\n";

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

/// Gate the manifest lines a rollback would install exactly like a synced import.
/// Returns the lines that may install; the rest go to the approval inbox.
pub async fn gate_rollback(
    config: &Config,
    sync_path: &Path,
    machine_id: &str,
    state_key: &str,
    lines: Vec<String>,
) -> Result<Vec<String>> {
    let (Some(def), Some(manager)) = (
        SIMPLE_MANAGERS.iter().find(|d| d.state_key == state_key),
        crate::packages::manager_for_key(state_key),
    ) else {
        anyhow::bail!("Rollback is not supported for {}", state_key);
    };
    let trust = Trust {
        inbox: Inbox::load()?,
        provenance: Provenance::load(sync_path, machine_id),
        auto_install_from_trusted: config.packages.auto_install_from_trusted,
    };
    let candidates = lines
        .into_iter()
        .map(|line| {
            let (name, version) = parse_pin(def.ecosystem, &line);
            (name, version, line)
        })
        .filter(|(name, _, _)| !trust.settled(state_key, name))
        .collect();
    let mut queued = Vec::new();
    let allowed = gate_simple(def, manager.as_ref(), &trust, candidates, &mut queued).await;
    report_held(&inbox::add(queued)?);
    Ok(allowed)
}

/// Keep the lines that may install and queue the rest with their OSV advisories.
async fn gate_simple(
    def: &PackageManagerDef,
    manager: &dyn PackageManager,
    trust: &Trust,
    missing: Vec<(String, Option<String>, String)>,
    queued: &mut Vec<InboxItem>,
) -> Vec<String> {
    let cooldown_unsupported = manager.cooldown().await == Cooldown::Unsupported;
    let pins: Vec<(String, Option<String>)> = missing
        .iter()
        .map(|(name, version, _)| (name.clone(), version.clone()))
        .collect();
    let advisories = osv::advisories(def.ecosystem, &pins).await;
    let mut allowed = Vec::new();
    for ((name, version, line), advisories) in missing.into_iter().zip(advisories) {
        let checks = Checks {
            cooldown_unsupported,
            malicious: advisories.iter().any(|id| osv::is_malicious(id)),
            ..trust.checks(def.state_key, &name)
        };
        let reasons = inbox::reasons(checks);
        if reasons.is_empty() {
            if !advisories.is_empty() {
                Output::warning(&format!(
                    "{} has known vulnerabilities: {}",
                    line,
                    advisories.join(", ")
                ));
            }
            allowed.push(line);
        } else {
            let mut item = trust.item(def.state_key, &name, version, None, reasons);
            item.advisories = advisories;
            queued.push(item);
        }
    }
    allowed
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
    let union_versions = MachineState::compute_union_versions(&machines);

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
            sync_simple_manager(
                def,
                &union_packages,
                &union_versions,
                state,
                &manifests_dir,
                dry_run,
            )?;
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
    let hash = crate::sha256_hex(manifest.as_bytes());
    let manifest_path = manifests_dir.join("Brewfile");

    let file_hash = std::fs::read(&manifest_path)
        .ok()
        .map(|c| crate::sha256_hex(&c));
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
    union_versions: &HashMap<String, HashMap<String, String>>,
    state: &mut SyncState,
    manifests_dir: &Path,
    dry_run: bool,
) -> Result<()> {
    let manifest = manifest_lines(
        def.ecosystem,
        union_packages.get(def.state_key),
        union_versions.get(def.state_key),
    );
    let hash = crate::sha256_hex(manifest.as_bytes());
    let manifest_path = manifests_dir.join(def.manifest_file);

    let file_hash = std::fs::read(&manifest_path)
        .ok()
        .map(|c| crate::sha256_hex(&c));
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

/// One pinned line per package; packages no machine reports a version for stay unpinned.
fn manifest_lines(
    ecosystem: Ecosystem,
    packages: Option<&Vec<String>>,
    versions: Option<&HashMap<String, String>>,
) -> String {
    let lines: Vec<String> = packages
        .into_iter()
        .flatten()
        .map(|name| {
            let version = versions.and_then(|v| v.get(name)).map(String::as_str);
            format_pin(ecosystem, name, version)
        })
        .collect();
    if lines.is_empty() {
        String::new()
    } else {
        lines.join("\n") + "\n"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_manifest_lines_pin_known_versions() {
        let packages = vec!["@types/node".to_string(), "left-pad".to_string()];
        let versions = HashMap::from([("@types/node".to_string(), "24.1.0".to_string())]);
        assert_eq!(
            manifest_lines(Ecosystem::Npm, Some(&packages), Some(&versions)),
            "@types/node@24.1.0\nleft-pad\n"
        );
        assert_eq!(manifest_lines(Ecosystem::Npm, None, None), "");
    }

    #[test]
    fn test_update_last_upgrade_creates_entry() {
        let mut state = SyncState {
            machine_id: "test".to_string(),
            last_sync: chrono::Utc::now(),
            files: HashMap::new(),
            packages: HashMap::new(),
            last_upgrade: None,
            last_upgrade_with_updates: None,
            deferred_casks: Vec::new(),
            deferred_casks_hash: None,
            dismissed_imports: std::collections::HashSet::new(),
        };

        assert!(!state.packages.contains_key("brew"));

        update_last_upgrade(&mut state, "brew");

        assert!(state.packages.contains_key("brew"));
        let pkg_state = state.packages.get("brew").unwrap();
        assert!(pkg_state.last_upgrade.is_some());
    }

    #[test]
    fn test_update_last_upgrade_updates_existing() {
        let original_time = chrono::Utc::now() - chrono::Duration::hours(1);
        let original_modified = Some(original_time - chrono::Duration::hours(2));

        let mut state = SyncState {
            machine_id: "test".to_string(),
            last_sync: chrono::Utc::now(),
            files: HashMap::new(),
            packages: HashMap::new(),
            last_upgrade: None,
            last_upgrade_with_updates: None,
            deferred_casks: Vec::new(),
            deferred_casks_hash: None,
            dismissed_imports: std::collections::HashSet::new(),
        };

        state.packages.insert(
            "brew".to_string(),
            PackageState {
                last_sync: original_time,
                last_modified: original_modified,
                last_upgrade: Some(original_time),
                hash: "existing_hash".to_string(),
            },
        );

        update_last_upgrade(&mut state, "brew");

        let pkg_state = state.packages.get("brew").unwrap();
        // last_upgrade should be updated to now (newer than original)
        assert!(pkg_state.last_upgrade.unwrap() >= original_time);
        // Other fields preserved via and_modify
        assert_eq!(pkg_state.last_modified, original_modified);
        assert_eq!(pkg_state.last_sync, original_time);
        assert_eq!(pkg_state.hash, "existing_hash");
    }
}

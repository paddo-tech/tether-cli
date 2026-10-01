use crate::cli::Output;
use crate::config::Config;
use crate::packages::inbox::{self, Checks, Inbox, InboxItem, Kind, Reason, Signer};
use crate::packages::osv;
use crate::packages::pin::{format_pin, parse_pin};
use crate::packages::{
    normalize_formula_name, BrewManager, BrewfilePackages, Cooldown, Ecosystem, PackageManager,
    PackagePolicy,
};
use crate::sync::signing::{self, Generations, SignedRecord, TrustStore};
use crate::sync::state::PackageState;
use crate::sync::{GitBackend, MachineState, SyncState};
use anyhow::Result;
use ssh_key::PublicKey;
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

    let mid = &machine_state.machine_id;
    prune_inbox(machine_state)?;
    let held_machines = inbox::queue_machine_keys(sync_path, mid)?;
    let trust = Trust::load(config, sync_path, mid)?;

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

    for item in &held_machines {
        if let Kind::TrustMachine { fingerprint, .. } = &item.kind {
            Output::warning(&format!(
                "Machine {} signs with key {}, which this machine does not trust. Check it on that \
                 machine, then run 'tether machines trust {}'",
                item.name, fingerprint, item.name
            ));
        }
    }
    outcome.queued = inbox::add(outcome.queued)?;
    report_held(&outcome.queued);
    outcome.queued.splice(0..0, held_machines);

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
    /// The caller holds the sync lock, which also guards the record generations.
    fn load(config: &Config, sync_path: &Path, machine_id: &str) -> Result<Self> {
        let own_key = signing::load_or_create(machine_id)?;
        let mut generations = Generations::load()?;
        let provenance = Provenance::load(
            sync_path,
            machine_id,
            own_key.public_key(),
            &TrustStore::load()?,
            &mut generations,
        );
        generations.save()?;
        Ok(Self {
            inbox: Inbox::load()?,
            provenance,
            auto_install_from_trusted: config.packages.auto_install_from_trusted,
        })
    }

    /// True when the package was already decided on: pending, or rejected.
    /// Items held as malicious are checked again, because approval cannot clear them and
    /// an unpinned query can match a report that covers only some releases. Items held only
    /// for trust are checked again too, because a machine record can become trusted later.
    fn settled(&self, manager: &str, name: &str) -> bool {
        self.inbox.items.iter().any(|i| {
            i.manager == manager
                && i.name == name
                && !i.reasons.contains(&Reason::Malicious)
                && !i
                    .reasons
                    .iter()
                    .all(|r| matches!(r, Reason::Unsigned | Reason::UntrustedSigner))
        }) || self.inbox.is_rejected(manager, name)
    }

    /// `tap` is the tap a Homebrew formula or cask resolves to, because an approval covers
    /// only that tap.
    fn checks(
        &self,
        manager: &str,
        name: &str,
        version: Option<&str>,
        tap: Option<&str>,
    ) -> Checks {
        let entry = (
            manager.to_string(),
            name.to_string(),
            version.map(str::to_string),
        );
        Checks {
            from_this_machine: self.provenance.own.contains(&entry),
            approved: self.inbox.is_approved(manager, name, version, tap),
            auto_install_from_trusted: self.auto_install_from_trusted,
            signer: self.provenance.signer(&entry),
            ..Checks::default()
        }
    }

    /// The manifest only names a package: anyone who can push could pin it to an older
    /// version that some trusted record once listed. So the newest version that a trusted
    /// record lists now replaces the manifest's pin. Without one, the manifest's pin stays,
    /// and the trust checks hold it.
    fn trusted_pin(
        &self,
        def: &PackageManagerDef,
        (name, version, line): (String, Option<String>, String),
    ) -> (String, Option<String>, String) {
        match self.provenance.newest_trusted_version(def.state_key, &name) {
            Some(newest) => {
                let line = format_pin(def.ecosystem, &name, Some(&newest));
                (name, Some(newest), line)
            }
            None => (name, version, line),
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
        let (source_machine, commit, signer) = self.provenance.source(manager, name);
        InboxItem {
            kind: Kind::Package,
            manager: manager.to_string(),
            name: name.to_string(),
            version,
            tap,
            source_machine,
            commit,
            signer,
            reasons,
            advisories: Vec::new(),
            first_seen: chrono::Utc::now(),
        }
    }
}

/// Manager key, name and reported version of one package.
type Entry = (String, String, Option<String>);

/// Every package a machine record lists, at the version the record reports.
fn record_entries(record: &MachineState) -> impl Iterator<Item = Entry> + '_ {
    record.packages.iter().flat_map(move |(manager, names)| {
        names.iter().map(move |name| {
            let version = record
                .package_versions
                .get(manager)
                .and_then(|v| v.get(name))
                .cloned();
            (manager.clone(), name.clone(), version)
        })
    })
}

/// The packages machine records vouch for. A record counts only when its signature verifies
/// against the key trusted for that record's machine id. Manifests are the union of every
/// record, trusted or not, and commits can come from anyone who can push, so neither counts.
/// Each sync works this out again from the repo, so nothing carries over between syncs.
struct Provenance {
    records: Vec<SignedRecord>,
    /// Listed by this machine's record, signed with this machine's key
    own: HashSet<Entry>,
    /// Listed by another machine's record, signed with the key trusted for that machine
    trusted: HashSet<Entry>,
    /// Listed by a validly signed record whose key is not trusted for its machine
    untrusted: HashSet<Entry>,
    this_machine: String,
    sync_path: std::path::PathBuf,
}

impl Provenance {
    fn load(
        sync_path: &Path,
        this_machine: &str,
        own_key: &PublicKey,
        store: &TrustStore,
        generations: &mut Generations,
    ) -> Self {
        let records = signing::records(sync_path);
        let (mut own, mut trusted, mut untrusted) =
            (HashSet::new(), HashSet::new(), HashSet::new());
        for r in &records {
            let Some(signer) = &r.signer else {
                continue;
            };
            let id = &r.record.machine_id;
            let own_record = id == this_machine && signer.key_data() == own_key.key_data();
            let trusted_record = id != this_machine && store.trusts(id, signer);
            // A record that grants trust must be the newest its key has signed
            if (own_record || trusted_record) && !generations.accept(signer, r.record.generation) {
                Output::warning(&format!(
                    "Ignoring machines/{}.json: its key signed a newer record before \
                     (generation {}). Someone may be replaying an old record",
                    id, r.record.generation
                ));
                continue;
            }
            let set = if own_record {
                &mut own
            } else if trusted_record {
                &mut trusted
            } else {
                &mut untrusted
            };
            set.extend(record_entries(&r.record));
        }
        Self {
            records,
            own,
            trusted,
            untrusted,
            this_machine: this_machine.to_string(),
            sync_path: sync_path.to_path_buf(),
        }
    }

    /// The newest version of a package that this machine's or a trusted machine's record
    /// lists.
    fn newest_trusted_version(&self, manager: &str, name: &str) -> Option<String> {
        self.own
            .iter()
            .chain(&self.trusted)
            .filter(|(m, n, _)| m == manager && n == name)
            .filter_map(|(_, _, version)| version.clone())
            .max_by(|a, b| crate::packages::pin::compare_versions(a, b))
    }

    fn signer(&self, entry: &Entry) -> Signer {
        if self.trusted.contains(entry) {
            Signer::Trusted
        } else if self.untrusted.contains(entry) {
            Signer::Untrusted
        } else {
            Signer::None
        }
    }

    /// The other machine that last synced a record listing the package, the commit that
    /// last changed that record, and the fingerprint of the key that signed it.
    fn source(
        &self,
        manager: &str,
        name: &str,
    ) -> (Option<String>, Option<String>, Option<String>) {
        let Some(r) = self
            .records
            .iter()
            .filter(|r| {
                r.record.machine_id != self.this_machine
                    && r.record
                        .packages
                        .get(manager)
                        .is_some_and(|names| names.iter().any(|n| n == name))
            })
            .max_by_key(|r| r.record.last_sync)
        else {
            return (None, None, None);
        };
        let commit = GitBackend::new(self.sync_path.clone())
            .file_log(&format!("machines/{}.json", r.record.machine_id), 1)
            .ok()
            .and_then(|log| log.into_iter().next())
            .map(|entry| entry.commit_hash);
        (
            Some(r.record.machine_id.clone()),
            commit,
            r.signer.as_ref().map(signing::fingerprint),
        )
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

    let policy = PackagePolicy::load();
    let local_taps: HashSet<String> = brew
        .list_taps()
        .await
        .map(|t| t.into_iter().collect())
        .unwrap_or_default();
    let taps = std::mem::take(&mut brew_packages.taps);
    brew_packages.taps = gate_taps(&policy, trust, &local_taps, taps, queued);

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

/// Keep the taps that are tapped already or may be tapped, and queue the rest. A tap is
/// added only when a trusted record lists it, like a formula, and its tap policy allows it.
fn gate_taps(
    policy: &PackagePolicy,
    trust: &Trust,
    local_taps: &HashSet<String>,
    taps: Vec<String>,
    queued: &mut Vec<InboxItem>,
) -> Vec<String> {
    let mut allowed = Vec::new();
    for tap in taps {
        if local_taps.contains(&tap) {
            allowed.push(tap);
            continue;
        }
        if trust.settled("brew_taps", &tap) {
            continue;
        }
        let checks = Checks {
            untrusted_tap: !policy.tap_trusted(&tap),
            ..trust.checks("brew_taps", &tap, None, None)
        };
        let reasons = inbox::reasons(checks);
        if reasons.is_empty() {
            allowed.push(tap);
        } else {
            queued.push(trust.item("brew_taps", &tap, None, None, reasons));
        }
    }
    allowed
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
        let tap = brew.tap_for(&name, manager == "brew_casks").await;
        let mut checks = trust.checks(manager, &name, None, tap.as_deref());
        checks.untrusted_tap = !tap.as_deref().is_some_and(|t| policy.tap_trusted(t));
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
    let missing: Vec<(String, Option<String>, String)> = manifest_entries(def.ecosystem, &manifest)
        .into_iter()
        .filter(|(name, _, _)| {
            !removed_packages.contains(name)
                && !local_packages.contains(name)
                && !trust.settled(def.state_key, name)
        })
        .map(|entry| trust.trusted_pin(def, entry))
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
    let trust = Trust::load(config, sync_path, machine_id)?;
    let candidates = manifest_entries(def.ecosystem, &lines.join("\n"))
        .into_iter()
        .filter(|(name, _, _)| !trust.settled(state_key, name))
        .collect();
    let mut queued = Vec::new();
    let allowed = gate_simple(def, manager.as_ref(), &trust, candidates, &mut queued).await;
    report_held(&inbox::add(queued)?);
    Ok(allowed)
}

/// Name, pinned version and text of each manifest line that names a registry release.
/// Other lines never reach the trust checks, OSV or a package manager.
fn manifest_entries(ecosystem: Ecosystem, manifest: &str) -> Vec<(String, Option<String>, String)> {
    manifest
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .filter_map(|line| {
            let (name, version) = parse_pin(ecosystem, line);
            let checked = crate::packages::validate_name(ecosystem, &name).and_then(|()| {
                version
                    .as_deref()
                    .map_or(Ok(()), |v| crate::packages::validate_version(ecosystem, v))
            });
            match checked {
                Ok(()) => Some((name, version, line.to_string())),
                Err(e) => {
                    let message = format!("Skipping manifest line: {}", e);
                    if crate::packages::policy::first_warning(&message) {
                        Output::warning(&message);
                    }
                    None
                }
            }
        })
        .collect()
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
            ..trust.checks(def.state_key, &name, version.as_deref(), None)
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
    fn test_dist_tags_never_reach_the_union_or_an_install() {
        let mut machine = MachineState::new("x");
        machine
            .packages
            .insert("npm".to_string(), vec!["example".to_string()]);
        machine.package_versions.insert(
            "npm".to_string(),
            HashMap::from([("example".to_string(), "latest".to_string())]),
        );
        machine.validate().unwrap();
        assert!(machine.package_versions["npm"].is_empty());
        assert_eq!(
            manifest_entries(
                Ecosystem::Npm,
                "example@latest\nrange@^1.0.0\nok@1.0.0\nlegacy\n"
            ),
            vec![
                (
                    "ok".to_string(),
                    Some("1.0.0".to_string()),
                    "ok@1.0.0".to_string()
                ),
                ("legacy".to_string(), None, "legacy".to_string()),
            ]
        );
    }

    fn new_key() -> ssh_key::PrivateKey {
        ssh_key::PrivateKey::random(&mut ssh_key::rand_core::OsRng, ssh_key::Algorithm::Ed25519)
            .unwrap()
    }

    fn repo(dir: &Path) -> GitBackend {
        let status = std::process::Command::new("git")
            .args(["init", "-q", "-b", "main"])
            .current_dir(dir)
            .status()
            .unwrap();
        assert!(status.success());
        std::fs::create_dir_all(dir.join("manifests")).unwrap();
        GitBackend::new(dir.to_path_buf())
    }

    /// Write `id`'s record listing npm packages at versions, signed with `key` when given.
    fn record(dir: &Path, id: &str, npm: &[(&str, &str)], key: Option<&ssh_key::PrivateKey>) {
        let mut machine = MachineState::new(id);
        machine.packages.insert(
            "npm".to_string(),
            npm.iter().map(|(name, _)| name.to_string()).collect(),
        );
        machine.package_versions.insert(
            "npm".to_string(),
            npm.iter()
                .map(|(name, version)| (name.to_string(), version.to_string()))
                .collect(),
        );
        machine.save_to_repo(dir).unwrap();
        if let Some(key) = key {
            signing::sign_record(dir, id, key).unwrap();
        }
    }

    /// The npm lines an export from these records writes.
    fn exported_npm(dir: &Path) -> Vec<(String, Option<String>)> {
        let machines = MachineState::list_all(dir).unwrap();
        manifest_lines(
            Ecosystem::Npm,
            MachineState::compute_union_packages(&machines).get("npm"),
            MachineState::compute_union_versions(&machines).get("npm"),
        )
        .lines()
        .map(|line| parse_pin(Ecosystem::Npm, line))
        .collect()
    }

    fn trust_as(dir: &Path, me: &str, key: &ssh_key::PrivateKey, store: &TrustStore) -> Trust {
        Trust {
            inbox: Inbox::default(),
            provenance: Provenance::load(
                dir,
                me,
                key.public_key(),
                store,
                &mut Generations::default(),
            ),
            auto_install_from_trusted: true,
        }
    }

    fn held(trust: &Trust, name: &str, version: Option<&str>) -> Vec<Reason> {
        inbox::reasons(trust.checks("npm", name, version, None))
    }

    /// Machines `me` and `t`, each trusting the other.
    fn two_machines() -> (
        tempfile::TempDir,
        ssh_key::PrivateKey,
        ssh_key::PrivateKey,
        TrustStore,
    ) {
        let tmp = tempfile::TempDir::new().unwrap();
        repo(tmp.path());
        let (me, t) = (new_key(), new_key());
        let mut store = TrustStore::default();
        store.trust("me", me.public_key()).unwrap();
        store.trust("t", t.public_key()).unwrap();
        (tmp, me, t, store)
    }

    #[test]
    fn test_attack_a_union_laundering_is_not_trusted() {
        let (tmp, me, t, store) = two_machines();
        let path = tmp.path();
        record(path, "me", &[("left-pad", "1.0.0")], Some(&me));
        record(path, "t", &[("good", "1.0.0")], Some(&t));
        record(path, "phantom", &[("evilpkg", "6.6.6")], None);
        assert!(exported_npm(path).contains(&("evilpkg".to_string(), Some("6.6.6".to_string()))));
        // The exporting machine signs the commit that carries the union
        GitBackend::new(path.to_path_buf())
            .commit_with_key("sync", "test", Some(&me))
            .unwrap();

        let here = trust_as(path, "me", &me, &store);
        assert_eq!(
            held(&here, "evilpkg", Some("6.6.6")),
            vec![Reason::Unsigned]
        );
        assert!(held(&here, "good", Some("1.0.0")).is_empty());
        assert!(held(&here, "left-pad", Some("1.0.0")).is_empty());
        let there = trust_as(path, "t", &t, &store);
        assert_eq!(
            held(&there, "evilpkg", Some("6.6.6")),
            vec![Reason::Unsigned]
        );
        assert!(held(&there, "left-pad", Some("1.0.0")).is_empty());
    }

    #[test]
    fn test_attack_b_only_the_trusted_version_is_trusted() {
        let (tmp, me, t, mut store) = two_machines();
        let path = tmp.path();
        let u = new_key();
        store.trust("u", u.public_key()).unwrap();
        record(path, "t", &[("example", "1.0.0")], Some(&t));
        record(path, "u", &[("example", "1.5.0")], Some(&u));
        record(path, "x", &[("example", "2.0.0")], None);
        assert_eq!(
            exported_npm(path),
            vec![("example".to_string(), Some("2.0.0".to_string()))]
        );

        let trust = trust_as(path, "me", &me, &store);
        assert_eq!(
            held(&trust, "example", Some("2.0.0")),
            vec![Reason::Unsigned]
        );
        assert_eq!(held(&trust, "example", None), vec![Reason::Unsigned]);
        assert!(held(&trust, "example", Some("1.0.0")).is_empty());
        assert!(held(&trust, "example", Some("1.5.0")).is_empty());
    }

    #[test]
    fn test_manifest_cannot_pick_an_older_trusted_version() {
        let (tmp, me, t, mut store) = two_machines();
        let path = tmp.path();
        let u = new_key();
        store.trust("u", u.public_key()).unwrap();
        record(path, "t", &[("example", "1.0.0")], Some(&t));
        record(path, "u", &[("example", "1.5.0")], Some(&u));
        let trust = trust_as(path, "me", &me, &store);
        let def = &SIMPLE_MANAGERS[0];
        // The manifest pins the older version that a trusted record also lists
        let line = |l: &str| manifest_entries(Ecosystem::Npm, l).remove(0);
        assert_eq!(
            trust.trusted_pin(def, line("example@1.0.0")),
            (
                "example".to_string(),
                Some("1.5.0".to_string()),
                "example@1.5.0".to_string()
            )
        );
        assert_eq!(
            trust.trusted_pin(def, line("example")).1.as_deref(),
            Some("1.5.0")
        );
        // A package no trusted record lists keeps the manifest's pin and is held
        let other = trust.trusted_pin(def, line("other@6.6.6"));
        assert_eq!(other.1.as_deref(), Some("6.6.6"));
        assert_eq!(
            held(&trust, "other", other.1.as_deref()),
            vec![Reason::Unsigned]
        );
    }

    #[test]
    fn test_taps_need_a_trusted_record_too() {
        let (tmp, me, t, store) = two_machines();
        let path = tmp.path();
        let mut machine = MachineState::new("t");
        machine.packages.insert(
            "brew_taps".to_string(),
            vec!["listed/tap".to_string(), "other/tap".to_string()],
        );
        machine.save_to_repo(path).unwrap();
        signing::sign_record(path, "t", &t).unwrap();
        let trust = trust_as(path, "me", &me, &store);
        let policy = crate::packages::PackagePolicy {
            min_release_age_days: 7,
            allow_scripts: Vec::new(),
            trusted_taps: vec!["listed/tap".to_string(), "added/tap".to_string()],
            approved_from_taps: Vec::new(),
        };
        let local = HashSet::from(["local/tap".to_string()]);
        let mut queued = Vec::new();
        let taps = ["listed/tap", "added/tap", "other/tap", "local/tap"]
            .map(String::from)
            .to_vec();
        let allowed = gate_taps(&policy, &trust, &local, taps, &mut queued);
        assert_eq!(allowed, vec!["listed/tap", "local/tap"]);
        // Allowed by policy, but no trusted record lists it: a repo writer added it
        assert_eq!(queued[0].name, "added/tap");
        assert_eq!(queued[0].reasons, vec![Reason::Unsigned]);
        assert_eq!(queued[1].name, "other/tap");
        assert_eq!(queued[1].reasons, vec![Reason::UntrustedTap]);
    }

    #[test]
    fn test_attack_c_commit_signatures_grant_no_trust() {
        let (tmp, me, _, store) = two_machines();
        let path = tmp.path();
        let git = GitBackend::new(path.to_path_buf());
        record(path, "me", &[("left-pad", "1.0.0")], Some(&me));
        git.commit_with_key("sync", "test", Some(&me)).unwrap();

        // A replayed attacker commit, signed with this machine's key, edits its record
        record(
            path,
            "me",
            &[("left-pad", "1.0.0"), ("evilpkg", "1.0.0")],
            None,
        );
        std::fs::write(path.join("manifests/npm.txt"), "evilpkg@1.0.0\n").unwrap();
        git.commit_with_key("replayed", "test", Some(&me)).unwrap();

        let trust = trust_as(path, "me", &me, &store);
        assert_eq!(
            held(&trust, "evilpkg", Some("1.0.0")),
            vec![Reason::Unsigned]
        );
    }

    #[test]
    fn test_replayed_older_record_is_not_trusted() {
        let (tmp, me, t, store) = two_machines();
        let path = tmp.path();
        let write = |generation: u64, version: &str| {
            let mut machine = MachineState::new("t");
            machine
                .packages
                .insert("npm".to_string(), vec!["example".to_string()]);
            machine.package_versions.insert(
                "npm".to_string(),
                HashMap::from([("example".to_string(), version.to_string())]),
            );
            machine.generation = generation;
            machine.save_to_repo(path).unwrap();
            signing::sign_record(path, "t", &t).unwrap();
        };
        let entry = |version: &str| {
            (
                "npm".to_string(),
                "example".to_string(),
                Some(version.to_string()),
            )
        };
        let mut seen = Generations::default();
        write(5, "2.0.0");
        let provenance = Provenance::load(path, "me", me.public_key(), &store, &mut seen);
        assert_eq!(provenance.signer(&entry("2.0.0")), Signer::Trusted);
        // An older record and signature by the same key, restored from git history
        write(3, "1.0.0");
        let provenance = Provenance::load(path, "me", me.public_key(), &store, &mut seen);
        assert_eq!(provenance.signer(&entry("1.0.0")), Signer::None);
    }

    #[test]
    fn test_record_signed_with_another_machines_key_is_untrusted() {
        let (tmp, me, t, store) = two_machines();
        let path = tmp.path();
        record(path, "y", &[("thing", "1.0.0")], Some(&t));
        record(path, "stranger", &[("other", "1.0.0")], Some(&new_key()));
        // This machine's own record signed by another key does not count as its own
        record(path, "me", &[("mine", "1.0.0")], Some(&t));

        let trust = trust_as(path, "me", &me, &store);
        assert_eq!(
            held(&trust, "thing", Some("1.0.0")),
            vec![Reason::UntrustedSigner]
        );
        assert_eq!(
            held(&trust, "other", Some("1.0.0")),
            vec![Reason::UntrustedSigner]
        );
        assert_eq!(
            held(&trust, "mine", Some("1.0.0")),
            vec![Reason::UntrustedSigner]
        );
        let item = trust.item("npm", "thing", None, None, Vec::new());
        assert_eq!(item.source_machine.as_deref(), Some("y"));
        assert_eq!(item.signer, Some(signing::fingerprint(t.public_key())));
    }

    #[test]
    fn test_approval_does_not_carry_to_another_version() {
        let (tmp, me, _, store) = two_machines();
        let mut trust = trust_as(tmp.path(), "me", &me, &store);
        let item = trust.item(
            "npm",
            "example",
            Some("1.0.0".to_string()),
            None,
            vec![Reason::Unsigned],
        );
        trust.inbox.add(item.clone());
        trust.inbox.approve(&item).unwrap();
        assert!(held(&trust, "example", Some("1.0.0")).is_empty());
        assert_eq!(
            held(&trust, "example", Some("6.6.6")),
            vec![Reason::Unsigned]
        );
        assert_eq!(held(&trust, "example", None), vec![Reason::Unsigned]);
    }

    #[test]
    fn test_attack_e_trust_does_not_depend_on_earlier_syncs() {
        let (tmp, me, t, store) = two_machines();
        let path = tmp.path();
        record(path, "t", &[("good", "1.0.0")], Some(&t));
        // A failed or deferred install leaves nothing behind, so the next sync decides the same
        for _ in 0..2 {
            let trust = trust_as(path, "me", &me, &store);
            assert!(held(&trust, "good", Some("1.0.0")).is_empty());
        }
        let off = Trust {
            auto_install_from_trusted: false,
            ..trust_as(path, "me", &me, &store)
        };
        assert_eq!(held(&off, "good", Some("1.0.0")), vec![Reason::Unsigned]);

        // An item held only for trust is checked again on the next sync
        let mut trust = trust_as(path, "me", &me, &store);
        let item = trust.item("npm", "good", None, None, vec![Reason::Unsigned]);
        trust.inbox.add(item.clone());
        assert!(!trust.settled("npm", "good"));
        trust.inbox.add(InboxItem {
            reasons: vec![Reason::Unsigned, Reason::CooldownUnsupported],
            ..item
        });
        assert!(trust.settled("npm", "good"));
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

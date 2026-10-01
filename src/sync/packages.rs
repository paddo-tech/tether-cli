use crate::cli::Output;
use crate::config::Config;
use crate::packages::inbox::{self, Checks, Inbox, InboxItem, Kind, Reason, Signer};
use crate::packages::osv;
use crate::packages::pin::{format_pin, parse_pin};
use crate::packages::{
    normalize_formula_name, BrewManager, BrewfilePackages, Cooldown, Ecosystem, PackageManager,
    PackagePolicy,
};
use crate::sync::signing::{self, TrustStore};
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
    let own_key = signing::load_or_create(mid)?;
    let held_machines = inbox::queue_machine_keys(sync_path, mid)?;
    let head = GitBackend::new(sync_path.to_path_buf()).head_commit()?;
    let introduced = match &state.signatures_checked {
        Some(since) => introductions(sync_path, since, &TrustStore::load()?, own_key.public_key())?,
        None => HashMap::new(),
    };
    state.signatures_checked = head;
    let trust = Trust {
        inbox: Inbox::load()?,
        provenance: Provenance::load(sync_path, mid, own_key.public_key()),
        introduced,
        auto_install_from_trusted: config.packages.auto_install_from_trusted,
    };

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
    for item in &outcome.queued {
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
    outcome.queued.splice(0..0, held_machines);

    Ok(outcome)
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
    introduced: HashMap<(String, String), Introduced>,
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

    fn introduced(&self, manager: &str, name: &str) -> Option<&Introduced> {
        self.introduced
            .get(&(manager.to_string(), name.to_string()))
    }

    fn checks(&self, manager: &str, name: &str) -> Checks {
        Checks {
            from_this_machine: self.provenance.listed_here(manager, name),
            approved: self.inbox.is_approved(manager, name),
            auto_install_from_trusted: self.auto_install_from_trusted,
            signer: self
                .introduced(manager, name)
                .map_or(Signer::None, |i| i.signer),
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
        let (source_machine, commit, signer) = match self.introduced(manager, name) {
            Some(i) => (
                i.machine
                    .clone()
                    .or_else(|| self.provenance.source(manager, name).0),
                Some(i.commit.clone()),
                i.fingerprint.clone(),
            ),
            None => {
                let (machine, commit) = self.provenance.source(manager, name);
                (machine, commit, None)
            }
        };
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

/// Which machines list a package, and the commit that last changed the source machine's state.
struct Provenance {
    machines: Vec<MachineState>,
    /// This machine's record, only when this machine signed the last change to it. A
    /// record's own `machine_id` proves nothing: anyone who can push can write one.
    this_record: Option<MachineState>,
    this_machine: String,
    sync_path: std::path::PathBuf,
}

impl Provenance {
    fn load(sync_path: &Path, this_machine: &str, own_key: &PublicKey) -> Self {
        let git = GitBackend::new(sync_path.to_path_buf());
        let signed_here = git
            .file_log(&format!("machines/{}.json", this_machine), 1)
            .ok()
            .and_then(|log| log.into_iter().next())
            .and_then(|entry| {
                let repo = git2::Repository::open(sync_path).ok()?;
                signing::commit_signer(&repo, git2::Oid::from_str(&entry.commit_hash).ok()?)
            })
            .is_some_and(|key| key.key_data() == own_key.key_data());
        Self {
            machines: MachineState::list_all(sync_path).unwrap_or_default(),
            this_record: signed_here
                .then(|| MachineState::load_from_repo(sync_path, this_machine).ok()?)
                .flatten(),
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
        self.this_record
            .as_ref()
            .is_some_and(|m| Self::lists(m, manager, name))
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

/// The newest pulled commit that added a manifest entry, and who signed it.
#[derive(Debug, Clone, PartialEq)]
struct Introduced {
    commit: String,
    signer: Signer,
    /// Fingerprint of the valid signature's key, trusted or not
    fingerprint: Option<String>,
    /// Trust store name of the signing key
    machine: Option<String>,
}

/// Manager key, name and pinned version of one manifest line.
type Entry = (String, String, Option<String>);

/// Every manifest entry in a commit's tree.
fn manifest_entries(repo: &git2::Repository, commit: &git2::Commit) -> HashSet<Entry> {
    let mut entries = HashSet::new();
    let Ok(tree) = commit.tree() else {
        return entries;
    };
    let read = |file: &str| -> Option<String> {
        let entry = tree.get_path(&Path::new("manifests").join(file)).ok()?;
        let blob = repo.find_blob(entry.id()).ok()?;
        String::from_utf8(blob.content().to_vec()).ok()
    };
    if let Some(text) = read("Brewfile") {
        let brew = BrewfilePackages::parse(&text);
        for (key, names) in [
            ("brew_taps", brew.taps),
            ("brew_formulae", brew.formulae),
            ("brew_casks", brew.casks),
        ] {
            entries.extend(names.into_iter().map(|n| (key.to_string(), n, None)));
        }
    }
    for def in SIMPLE_MANAGERS {
        if let Some(text) = read(def.manifest_file) {
            for line in text.lines().map(str::trim).filter(|l| !l.is_empty()) {
                let (name, version) = parse_pin(def.ecosystem, line);
                entries.insert((def.state_key.to_string(), name, version));
            }
        }
    }
    entries
}

/// For each manifest entry that is in HEAD but not in `since`, find the newest commit after
/// `since` that added it and verify that commit's signature. Keyed by (manager, name).
/// A `since` the repo no longer has yields nothing, so those packages wait in the inbox.
fn introductions(
    sync_path: &Path,
    since: &str,
    store: &TrustStore,
    own_key: &PublicKey,
) -> Result<HashMap<(String, String), Introduced>> {
    let repo = git2::Repository::open(sync_path)?;
    let head = repo.head()?.peel_to_commit()?;
    let Some(old) = git2::Oid::from_str(since)
        .ok()
        .and_then(|oid| repo.find_commit(oid).ok())
    else {
        return Ok(HashMap::new());
    };

    let old_entries = manifest_entries(&repo, &old);
    let mut targets: Vec<Entry> = manifest_entries(&repo, &head)
        .into_iter()
        .filter(|e| !old_entries.contains(e))
        .collect();

    let mut walk = repo.revwalk()?;
    walk.push(head.id())?;
    walk.hide(old.id())?;
    walk.set_sorting(git2::Sort::TOPOLOGICAL | git2::Sort::TIME)?;

    let mut found = HashMap::new();
    for oid in walk {
        if targets.is_empty() {
            break;
        }
        let commit = repo.find_commit(oid?)?;
        let parent_entries = commit
            .parent(0)
            .map(|p| manifest_entries(&repo, &p))
            .unwrap_or_default();
        let added: HashSet<Entry> = manifest_entries(&repo, &commit)
            .into_iter()
            .filter(|e| !parent_entries.contains(e))
            .collect();
        if !targets.iter().any(|t| added.contains(t)) {
            continue;
        }
        let key = signing::commit_signer(&repo, commit.id());
        let trusted = key
            .as_ref()
            .is_some_and(|k| k.key_data() == own_key.key_data() || store.machine_for(k).is_some());
        let introduced = Introduced {
            commit: commit.id().to_string(),
            signer: match &key {
                None => Signer::None,
                Some(_) if trusted => Signer::Trusted,
                Some(_) => Signer::Untrusted,
            },
            fingerprint: key.as_ref().map(signing::fingerprint),
            machine: key
                .as_ref()
                .and_then(|k| store.machine_for(k))
                .map(str::to_string),
        };
        targets.retain(|t| {
            if !added.contains(t) {
                return true;
            }
            found.insert((t.0.clone(), t.1.clone()), introduced.clone());
            false
        });
    }
    Ok(found)
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

    #[test]
    fn test_introductions_verify_the_commit_that_added_each_entry() {
        let tmp = tempfile::TempDir::new().unwrap();
        let path = tmp.path();
        let git = repo(path);
        let (own, trusted, stranger) = (new_key(), new_key(), new_key());
        let mut store = TrustStore::default();
        store.trust("t", trusted.public_key());
        let commit = |npm: &str, brew: &str, key: Option<&ssh_key::PrivateKey>| {
            std::fs::write(path.join("manifests/npm.txt"), npm).unwrap();
            std::fs::write(path.join("manifests/Brewfile"), brew).unwrap();
            git.commit_with_key("sync", "test", key).unwrap();
        };

        commit("a@1.0.0\n", "", None);
        let since = git.head_commit().unwrap().unwrap();
        commit("a@2.0.0\nb@1.0.0\n", "brew \"jq\"\n", Some(&trusted));
        commit("a@2.0.0\nb@1.0.0\nc@1.0.0\n", "brew \"jq\"\n", None);
        commit(
            "a@2.0.0\nb@1.0.0\nc@1.0.0\nd@1.0.0\n",
            "brew \"jq\"\n",
            Some(&stranger),
        );
        commit(
            "a@2.0.0\nb@1.0.0\nc@1.0.0\nd@1.0.0\ne@1.0.0\n",
            "brew \"jq\"\n",
            Some(&own),
        );

        let found = introductions(path, &since, &store, own.public_key()).unwrap();
        let signer = |manager: &str, name: &str| {
            found
                .get(&(manager.to_string(), name.to_string()))
                .map(|i| i.signer)
        };
        assert_eq!(signer("npm", "a"), Some(Signer::Trusted));
        assert_eq!(signer("npm", "b"), Some(Signer::Trusted));
        assert_eq!(signer("brew_formulae", "jq"), Some(Signer::Trusted));
        assert_eq!(signer("npm", "c"), Some(Signer::None));
        assert_eq!(signer("npm", "d"), Some(Signer::Untrusted));
        assert_eq!(signer("npm", "e"), Some(Signer::Trusted));
        let b = &found[&("npm".to_string(), "b".to_string())];
        assert_eq!(b.machine.as_deref(), Some("t"));
        assert_eq!(
            b.fingerprint.as_deref(),
            Some(signing::fingerprint(trusted.public_key()).as_str())
        );
        assert_eq!(found.len(), 6);

        // Only commits after `since` count, and a lost `since` checks nothing
        let head = git.head_commit().unwrap().unwrap();
        assert!(introductions(path, &head, &store, own.public_key())
            .unwrap()
            .is_empty());
        let missing = "0".repeat(40);
        assert!(introductions(path, &missing, &store, own.public_key())
            .unwrap()
            .is_empty());
    }

    #[test]
    fn test_this_machine_record_counts_only_when_this_machine_signed_it() {
        let tmp = tempfile::TempDir::new().unwrap();
        let path = tmp.path();
        let git = repo(path);
        let own = new_key();
        let mut me = MachineState::new("me");
        me.packages
            .insert("npm".to_string(), vec!["left-pad".to_string()]);
        me.save_to_repo(path).unwrap();
        git.commit_with_key("unsigned", "test", None).unwrap();
        let provenance = Provenance::load(path, "me", own.public_key());
        assert!(!provenance.listed_here("npm", "left-pad"));

        me.hostname = "signed".to_string();
        me.save_to_repo(path).unwrap();
        git.commit_with_key("signed", "test", Some(&own)).unwrap();
        assert!(Provenance::load(path, "me", own.public_key()).listed_here("npm", "left-pad"));

        // Another file claiming this machine's id adds nothing
        let mut impostor = me.clone();
        impostor
            .packages
            .insert("npm".to_string(), vec!["evil".to_string()]);
        std::fs::write(
            path.join("machines/extra.json"),
            serde_json::to_string(&impostor).unwrap(),
        )
        .unwrap();
        git.commit_with_key("impostor", "test", Some(&own)).unwrap();
        let provenance = Provenance::load(path, "me", own.public_key());
        assert!(!provenance.listed_here("npm", "evil"));
        assert!(provenance.machines.iter().all(|m| m.hostname == "signed"));
        assert_eq!(provenance.machines.len(), 1);

        // Nor does a change to this machine's record that another key signed
        me.packages
            .insert("npm".to_string(), vec!["evil".to_string()]);
        me.save_to_repo(path).unwrap();
        git.commit_with_key("forged", "test", Some(&new_key()))
            .unwrap();
        assert!(!Provenance::load(path, "me", own.public_key()).listed_here("npm", "evil"));
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
            signatures_checked: None,
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
            signatures_checked: None,
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

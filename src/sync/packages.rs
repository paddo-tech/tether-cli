use crate::cli::Output;
use crate::config::Config;
use crate::packages::inbox::{self, Checks, Inbox, InboxItem, Kind, Reason, Signer};
use crate::packages::osv;
use crate::packages::pin::{format_pin, parse_pin};
use crate::packages::resolve::resolve_version;
use crate::packages::{
    normalize_formula_name, BrewManager, BrewfilePackages, Cooldown, Ecosystem, PackageInfo,
    PackageManager, PackagePolicy,
};
use crate::sync::membership::{self, Membership};
use crate::sync::signing::{self, Generations, RecordStatus, SignedRecord, TrustStore};
use crate::sync::state::{InstallFailure, PackageState};
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

/// Add casks that need a password to the list a sync in a terminal installs. Notifies only
/// when the list changes.
pub fn defer_casks(state: &mut SyncState, casks: &[String]) -> Result<()> {
    if casks.is_empty() {
        return Ok(());
    }
    let mut all: std::collections::BTreeSet<String> =
        state.deferred_casks.iter().cloned().collect();
    all.extend(casks.iter().cloned());
    state.deferred_casks = all.into_iter().collect();

    let hash = crate::sha256_hex(state.deferred_casks.join(",").as_bytes());
    if state.deferred_casks_hash.as_ref() != Some(&hash) {
        crate::sync::notify_deferred_casks(&state.deferred_casks).ok();
        state.deferred_casks_hash = Some(hash);
        Output::info(&format!(
            "Deferred {} cask{} that need a password: {}. Run 'tether sync' in a terminal to \
             install them",
            state.deferred_casks.len(),
            if state.deferred_casks.len() == 1 {
                ""
            } else {
                "s"
            },
            state.deferred_casks.join(", ")
        ));
    }
    state.save()
}

/// What an import did beyond installing.
#[derive(Debug, Default)]
pub struct ImportOutcome {
    /// Casks that need a password; only the daemon defers them
    pub deferred_casks: Vec<String>,
    /// Packages newly held in the approval inbox
    pub queued: Vec<InboxItem>,
    /// Trusted machines whose record newly fails its signature
    pub signature_failed: Vec<String>,
    /// Why the package profiles table does not read, when this sync first met the error
    pub membership_error: Option<String>,
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
    // A record trusted since the last sync adds its packages to the manifests before this
    // import reads them, so one sync after approval installs them
    sync_packages(config, state, sync_path, machine_state, false).await?;
    let manifests_dir = sync_path.join("manifests");
    if !manifests_dir.exists() {
        return Ok(outcome);
    }

    let mid = &machine_state.machine_id;
    prune_inbox(machine_state)?;
    prune_failures(&mut state.install_failures, machine_state);
    let held_machines = inbox::queue_machine_keys(sync_path, mid)?;
    let trust = Trust::load(config, sync_path, mid)?;
    outcome.signature_failed =
        new_signature_failures(&trust.provenance.failed, &mut state.warned_signatures);
    for id in &outcome.signature_failed {
        Output::warning(&format!(
            "Record for {} fails its signature; Tether ignores it. Someone may have edited it \
             in the repo",
            id
        ));
    }
    let table = match membership::read_table(sync_path) {
        Ok(table) => {
            state.membership_error = None;
            table
        }
        Err(e) => {
            // Without the table every package would install everywhere, so none installs
            let error = e.to_string();
            Output::warning(&format!(
                "{}. Tether installs no synced packages until the file reads. Fix or delete it \
                 in the sync repo",
                error
            ));
            if state.membership_error.as_deref() != Some(error.as_str()) {
                state.membership_error = Some(error.clone());
                outcome.membership_error = Some(error);
            }
            outcome.queued = held_machines;
            return Ok(outcome);
        }
    };
    let scope = Membership::load(config, sync_path, machine_state, &table)?;
    notify_excluded(&scope, &mut state.profile_notice_shown);
    let mut gated = Gated::default();

    // Homebrew - special handling for formulae/casks/taps
    if config.is_manager_enabled(mid, "brew") {
        let (casks, installed) = import_brew(
            &manifests_dir,
            machine_state,
            BrewImport {
                daemon_mode,
                casks: import_casks(config.packages.brew.sync_casks),
                previously_deferred,
            },
            &trust,
            &scope,
            &mut gated,
            &mut state.install_failures,
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
                &scope,
                &mut gated,
                &mut state.install_failures,
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
                 machine, then run 'tether machines trust {} --fingerprint {}'",
                item.name, fingerprint, item.name, fingerprint
            ));
        }
    }
    let listed = Listed::new(&trust.provenance.records, machine_state);
    // An item for a package this machine's profile left is dropped like an unlisted one
    let needed_taps = gated.needed_taps;
    outcome.queued = inbox::settle(gated.held, &gated.passed, |m, n| {
        listed.holds(m, n)
            && (scope.includes(m, n)
                || (m == "brew_taps" && needed_taps.contains(&n.to_lowercase())))
    })?;
    report_held(&outcome.queued);
    outcome.queued.splice(0..0, held_machines);

    Ok(outcome)
}

/// Name once the packages other profiles have that this machine does not install, so an
/// upgrade from a build that installed every machine's packages does not drop them silently.
fn notify_excluded(scope: &Membership, shown: &mut bool) {
    if *shown {
        return;
    }
    if let Some(notice) = excluded_notice(scope) {
        Output::warning(&notice);
        *shown = true;
    }
}

/// Counts, not names: a machine that joins a large fleet can miss hundreds of packages.
fn excluded_notice(scope: &Membership) -> Option<String> {
    let excluded = scope.excluded_packages();
    if excluded.is_empty() {
        return None;
    }
    let mut managers: HashMap<&str, usize> = HashMap::new();
    let mut profiles: HashMap<String, usize> = HashMap::new();
    for (manager, name) in &excluded {
        *managers.entry(manager).or_default() += 1;
        for profile in scope.members(manager, name) {
            *profiles.entry(profile).or_default() += 1;
        }
    }
    let counts = |map: HashMap<&str, usize>| {
        let mut counts: Vec<(&str, usize)> = map.into_iter().collect();
        counts.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(b.0)));
        counts
            .iter()
            .map(|(name, n)| format!("{} {}", name, n))
            .collect::<Vec<_>>()
            .join(", ")
    };
    let profiles = counts(profiles.iter().map(|(p, n)| (p.as_str(), *n)).collect());
    Some(format!(
        "This machine (profile {}) installs only the packages of its profile. {} package{} of \
         other profiles {} not install here. By manager: {}. By profile: {}. Run 'tether \
         packages list --other-profiles' to see them, and 'tether packages share \
         <manager:name> --to {}' to install one here",
        scope.profile,
        excluded.len(),
        if excluded.len() == 1 { "" } else { "s" },
        if excluded.len() == 1 { "does" } else { "do" },
        counts(managers),
        profiles,
        scope.profile
    ))
}

/// The ids of failing records not reported before. `warned` keeps only records that still
/// fail, so a record that fails again later is reported again.
fn new_signature_failures(
    failed: &[(String, String)],
    warned: &mut HashSet<String>,
) -> Vec<String> {
    warned.retain(|digest| failed.iter().any(|(_, d)| d == digest));
    failed
        .iter()
        .filter(|(_, digest)| warned.insert(digest.clone()))
        .map(|(id, _)| id.clone())
        .collect()
}

/// What the checks decided for the packages one import or rollback checked. Every sync
/// checks each missing package again, held or not, because a check can change: a record
/// becomes trusted, a manager learns the release-age limit, a tap becomes trusted.
#[derive(Default)]
struct Gated {
    /// Held for approval
    held: Vec<InboxItem>,
    /// May install now, as manager key and name. A pending item for one is dropped.
    passed: Vec<(String, String)>,
    /// Taps, lowercase, that an included formula or cask names. They count as in scope, so
    /// a formula shared to this profile brings its tap
    needed_taps: HashSet<String>,
}

/// The taps the formulae and casks name, as `owner/repo/name` does, lowercase.
fn needed_taps(packages: &BrewfilePackages) -> HashSet<String> {
    packages
        .formulae
        .iter()
        .chain(&packages.casks)
        .filter_map(|name| name.rsplit_once('/'))
        .map(|(tap, _)| tap.to_lowercase())
        .filter(|tap| tap.contains('/'))
        .collect()
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

/// Failures for packages that are installed now, by any means, are settled.
fn prune_failures(failures: &mut HashMap<String, InstallFailure>, machine_state: &MachineState) {
    failures.retain(|key, _| {
        let Some((manager, name)) = key.split_once(':') else {
            return false;
        };
        !machine_state.packages.get(manager).is_some_and(|names| {
            names.iter().any(|n| {
                n == name || (manager.starts_with("brew_") && n == normalize_formula_name(name))
            })
        })
    });
}

/// Whether a package that failed before may install again now.
fn retry_due(
    failures: &HashMap<String, InstallFailure>,
    manager: &str,
    name: &str,
    version: Option<&str>,
) -> bool {
    failures
        .get(&InstallFailure::key(manager, name))
        .is_none_or(|f| f.should_retry(version, chrono::Utc::now()))
}

/// Record a failed install. Tether warns on the first failure of each version only, so a
/// package that cannot install here does not warn on every sync.
fn record_failure(
    failures: &mut HashMap<String, InstallFailure>,
    manager: &str,
    name: &str,
    version: Option<&str>,
    error: &str,
) {
    let key = InstallFailure::key(manager, name);
    let error: String = error.trim().chars().take(300).collect();
    if failures
        .get(&key)
        .is_none_or(|f| f.version.as_deref() != version)
    {
        Output::warning(&format!(
            "Failed to install {}{} ({}): {}. Tether tries again in {} hours, or when the \
             version changes. 'tether packages --list' shows failed installs",
            name,
            version.map(|v| format!(" {}", v)).unwrap_or_default(),
            manager,
            error,
            InstallFailure::RETRY_AFTER_HOURS
        ));
    } else {
        log::debug!("{} still fails to install: {}", key, error);
    }
    failures.insert(
        key,
        InstallFailure {
            version: version.map(str::to_string),
            attempted: chrono::Utc::now(),
            error,
        },
    );
}

/// Casks are macOS apps: Homebrew on Linux cannot install them.
fn import_casks(sync_casks: bool) -> bool {
    sync_casks && cfg!(target_os = "macos")
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
            signature_failed: self.provenance.source_signature_failed(manager, name),
            ..Checks::default()
        }
    }

    /// Manifests only name packages, and anyone who can push can write them. So the version
    /// comes only from trusted records: the newest one that this machine's or a trusted
    /// machine's record lists. Without one the package has no version, and the trust checks
    /// hold it.
    fn trusted_pin(
        &self,
        def: &PackageManagerDef,
        name: String,
    ) -> (String, Option<String>, String) {
        let version = self
            .provenance
            .newest_trusted_version(def.ecosystem, def.state_key, &name);
        let line = format_pin(def.ecosystem, &name, version.as_deref());
        (name, version, line)
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

/// A record with no OS data counts as this OS, so missing data never makes a pin foreign.
fn same_os(record: &MachineState) -> bool {
    record.os_family() == "unknown" || record.os_family() == std::env::consts::OS
}

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
/// against the key trusted for that record's machine id. Anyone who can push can write the
/// manifests and commits, so neither counts.
/// Each sync works this out again from the repo, so nothing carries over between syncs.
struct Provenance {
    records: Vec<SignedRecord>,
    /// Listed by this machine's record, signed with this machine's key
    own: HashSet<Entry>,
    /// Listed by another machine's record, signed with the key trusted for that machine
    trusted: HashSet<Entry>,
    /// Listed by a validly signed record whose key is not trusted for its machine
    untrusted: HashSet<Entry>,
    /// Listed by this machine's record or a trusted record from a machine on this OS
    native: HashSet<Entry>,
    /// Id and SHA-256 of each other machine's record whose id has a trusted key, but no
    /// signature by that key verifies
    failed: Vec<(String, String)>,
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
        let (mut own, mut trusted, mut untrusted, mut native) = (
            HashSet::new(),
            HashSet::new(),
            HashSet::new(),
            HashSet::new(),
        );
        let mut failed = Vec::new();
        for r in &records {
            let id = &r.record.machine_id;
            let status = signing::record_status(r, this_machine, Some(own_key), store, generations);
            match status {
                // A record that grants trust must be the newest its key has signed
                RecordStatus::Replayed => {
                    Output::warning(&format!(
                        "Ignoring machines/{}.json: its key signed a newer or different record \
                         at generation {} before. Someone may be replaying an old record",
                        id, r.record.generation
                    ));
                    continue;
                }
                // This machine signs its own record again on its next save
                RecordStatus::SignatureFailed if id != this_machine => {
                    failed.push((id.clone(), r.digest.clone()));
                }
                _ => {}
            }
            let Some(signer) = &r.signer else {
                continue;
            };
            let own_record = id == this_machine && status == RecordStatus::Trusted;
            let trusted_record = id != this_machine && status == RecordStatus::Trusted;
            if own_record || trusted_record {
                generations.accept(signer, r.record.generation, &r.digest);
            }
            if own_record || (trusted_record && same_os(&r.record)) {
                native.extend(record_entries(&r.record));
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
            native,
            failed,
            this_machine: this_machine.to_string(),
            sync_path: sync_path.to_path_buf(),
        }
    }

    /// The newest version of a package that this machine's or a trusted machine's record
    /// lists.
    fn newest_trusted_version(
        &self,
        ecosystem: Ecosystem,
        manager: &str,
        name: &str,
    ) -> Option<String> {
        self.own
            .iter()
            .chain(&self.trusted)
            .filter(|(m, n, _)| m == manager && n == name)
            .filter_map(|(_, _, version)| version.clone())
            .max_by(|a, b| crate::packages::pin::compare_versions(ecosystem, a, b))
    }

    /// Whether only machines on another OS list the newest trusted version. That version
    /// may not install here, for example a uv tool that needs a newer Python.
    fn foreign_pin(&self, ecosystem: Ecosystem, manager: &str, name: &str) -> bool {
        self.newest_trusted_version(ecosystem, manager, name)
            .is_some_and(|v| {
                !self
                    .native
                    .contains(&(manager.to_string(), name.to_string(), Some(v)))
            })
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

    /// The other machine's record that last synced and lists the package.
    fn source_record(&self, manager: &str, name: &str) -> Option<&SignedRecord> {
        self.records
            .iter()
            .filter(|r| {
                r.record.machine_id != self.this_machine
                    && r.record
                        .packages
                        .get(manager)
                        .is_some_and(|names| names.iter().any(|n| n == name))
            })
            .max_by_key(|r| r.record.last_sync)
    }

    /// Whether the package's source machine is trusted, but its record fails its signature.
    fn source_signature_failed(&self, manager: &str, name: &str) -> bool {
        self.source_record(manager, name).is_some_and(|r| {
            self.failed
                .iter()
                .any(|(id, digest)| *id == r.record.machine_id && *digest == r.digest)
        })
    }

    /// The other machine that last synced a record listing the package, the commit that
    /// last changed that record, and the fingerprint of the key that signed it.
    fn source(
        &self,
        manager: &str,
        name: &str,
    ) -> (Option<String>, Option<String>, Option<String>) {
        let Some(r) = self.source_record(manager, name) else {
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

/// How one sync imports Homebrew packages.
struct BrewImport<'a> {
    daemon_mode: bool,
    /// Whether casks install on this machine at all
    casks: bool,
    previously_deferred: &'a [String],
}

/// Import brew packages (formulae, casks, taps).
/// Casks are installed individually to detect which need password.
/// Returns (deferred_casks, installed_any) - list of casks needing password and whether any packages were installed.
/// Keep the Brewfile entries this machine may install: not removed here, in this machine's
/// scope, and valid. A tap stays when it is in scope or a kept formula or cask names it, so
/// casks that this machine never installs bring no taps.
fn select_brew(
    brew_packages: &mut BrewfilePackages,
    machine_state: &MachineState,
    scope: &Membership,
    import_casks: bool,
    gated: &mut Gated,
) {
    let removed = |manager: &str| -> HashSet<String> {
        machine_state
            .removed_packages
            .get(manager)
            .map(|v| v.iter().cloned().collect())
            .unwrap_or_default()
    };
    let (removed_formulae, removed_casks, removed_taps) = (
        removed("brew_formulae"),
        removed("brew_casks"),
        removed("brew_taps"),
    );
    brew_packages
        .formulae
        .retain(|p| !removed_formulae.contains(p) && scope.includes("brew_formulae", p));
    brew_packages
        .casks
        .retain(|p| !removed_casks.contains(p) && scope.includes("brew_casks", p));
    brew_packages.retain_valid();
    if !import_casks {
        // Drop pending inbox items for casks this machine never installs
        gated.passed.extend(
            brew_packages
                .casks
                .drain(..)
                .map(|cask| ("brew_casks".to_string(), cask)),
        );
    }
    gated.needed_taps = needed_taps(brew_packages);
    brew_packages.taps.retain(|p| {
        !removed_taps.contains(p)
            && (scope.includes("brew_taps", p) || gated.needed_taps.contains(&p.to_lowercase()))
    });
}

async fn import_brew(
    manifests_dir: &Path,
    machine_state: &MachineState,
    opts: BrewImport<'_>,
    trust: &Trust,
    scope: &Membership,
    gated: &mut Gated,
    failures: &mut HashMap<String, InstallFailure>,
) -> (Vec<String>, bool) {
    let BrewImport {
        daemon_mode,
        casks: import_casks,
        previously_deferred,
    } = opts;
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

    let mut brew_packages = BrewfilePackages::parse(&manifest);
    select_brew(
        &mut brew_packages,
        machine_state,
        scope,
        import_casks,
        gated,
    );

    let policy = PackagePolicy::load();
    let local_taps: HashSet<String> = brew
        .list_taps()
        .await
        .map(|t| t.into_iter().collect())
        .unwrap_or_default();
    let taps = std::mem::take(&mut brew_packages.taps);
    brew_packages.taps = gate_taps(&policy, trust, &local_taps, taps, gated);

    // gate_brew resolves a short name only among tapped repositories, so allowed taps come first
    for tap in &brew_packages.taps {
        if !local_taps.contains(tap) {
            if let Err(e) = brew.tap(tap).await {
                Output::warning(&format!("Failed to tap {}: {}", tap, e));
            }
        }
    }

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
        .filter(|p| retry_due(failures, "brew_formulae", p, None))
        .cloned()
        .collect();

    // Collect casks to install: missing + previously deferred that still need install
    let mut casks_to_try: Vec<_> = brew_packages
        .casks
        .iter()
        .filter(|p| !local_casks.contains(p.as_str()))
        .cloned()
        .collect();

    for deferred in previously_deferred.iter().filter(|_| import_casks) {
        if !local_casks.contains(deferred.as_str())
            && !casks_to_try.contains(deferred)
            && !machine_state
                .removed_packages
                .get("brew_casks")
                .is_some_and(|removed| removed.contains(deferred))
            && scope.includes("brew_casks", deferred)
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
        gated,
    )
    .await;
    casks_to_try.retain(|c| retry_due(failures, "brew_casks", c, None));
    let casks_to_try = gate_brew(&brew, &policy, trust, "brew_casks", casks_to_try, gated).await;

    let mut installed_any = false;

    // Install formulae via bundle (no password needed)
    if !missing_formulae.is_empty() {
        Output::info(&format!(
            "Installing {} brew formula{}: {}",
            missing_formulae.len(),
            if missing_formulae.len() == 1 { "" } else { "e" },
            missing_formulae.join(", ")
        ));

        let formulae_manifest = BrewfilePackages {
            taps: brew_packages.taps,
            formulae: missing_formulae.clone(),
            casks: Vec::new(),
        };
        let result = brew.import_manifest(&formulae_manifest.generate()).await;
        installed_any = result.is_ok();
        // brew bundle reports one exit code, so the installed list tells which formula failed
        match brew.installed_formulae().await {
            Ok(installed) => {
                for formula in &missing_formulae {
                    if installed.contains(normalize_formula_name(formula)) {
                        failures.remove(&InstallFailure::key("brew_formulae", formula));
                        continue;
                    }
                    // brew bundle installs every formula with one `brew install`, so one
                    // formula that brew refuses fails all of them; each one left gets its own
                    let error = match &result {
                        Ok(()) => "brew bundle did not install it".to_string(),
                        Err(_) => {
                            let package = PackageInfo {
                                name: formula.clone(),
                                version: None,
                            };
                            match brew.install(&package).await {
                                Ok(()) => {
                                    installed_any = true;
                                    failures.remove(&InstallFailure::key("brew_formulae", formula));
                                    continue;
                                }
                                Err(e) => e.to_string(),
                            }
                        }
                    };
                    record_failure(failures, "brew_formulae", formula, None, &error);
                }
            }
            Err(e) => Output::warning(&format!("Cannot list installed formulae: {}", e)),
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
                    failures.remove(&InstallFailure::key("brew_casks", cask));
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
                        record_failure(
                            failures,
                            "brew_casks",
                            cask,
                            None,
                            "brew install --cask failed",
                        );
                    }
                }
                Err(e) => {
                    record_failure(failures, "brew_casks", cask, None, &e.to_string());
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
    gated: &mut Gated,
) -> Vec<String> {
    let mut allowed = Vec::new();
    for tap in taps {
        if local_taps.contains(&tap) {
            allowed.push(tap);
            continue;
        }
        if trust.inbox.is_rejected("brew_taps", &tap, None, None) {
            continue;
        }
        let checks = Checks {
            untrusted_tap: !policy.tap_trusted(&tap),
            ..trust.checks("brew_taps", &tap, None, None)
        };
        let reasons = inbox::reasons(checks);
        if reasons.is_empty() {
            gated.passed.push(("brew_taps".to_string(), tap.clone()));
            allowed.push(tap);
        } else {
            gated
                .held
                .push(trust.item("brew_taps", &tap, None, None, reasons));
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
    gated: &mut Gated,
) -> Vec<String> {
    let mut allowed = Vec::new();
    for name in names {
        let tap = brew.tap_for(&name, manager == "brew_casks").await;
        if gate_brew_entry(policy, trust, manager, &name, tap, gated) {
            allowed.push(name);
        }
    }
    allowed
}

/// Whether a formula or cask that resolves to `tap` now may install; else it is held,
/// unless rejected. Decisions and items bind to that tap, trusted or not, so an approval
/// or rejection never carries over when the name moves to another tap.
fn gate_brew_entry(
    policy: &PackagePolicy,
    trust: &Trust,
    manager: &str,
    name: &str,
    tap: Option<String>,
    gated: &mut Gated,
) -> bool {
    if trust.inbox.is_rejected(manager, name, None, tap.as_deref()) {
        return false;
    }
    let checks = Checks {
        untrusted_tap: !tap.as_deref().is_some_and(|t| policy.tap_trusted(t)),
        ..trust.checks(manager, name, None, tap.as_deref())
    };
    let reasons = inbox::reasons(checks);
    if reasons.is_empty() {
        gated.passed.push((manager.to_string(), name.to_string()));
        return true;
    }
    gated
        .held
        .push(trust.item(manager, name, None, tap, reasons));
    false
}

/// Import a simple package manager (one package per line manifest)
/// Returns true if any packages were installed.
async fn import_simple_manager(
    def: &PackageManagerDef,
    manifests_dir: &Path,
    machine_state: &MachineState,
    trust: &Trust,
    scope: &Membership,
    gated: &mut Gated,
    failures: &mut HashMap<String, InstallFailure>,
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

    // Filter to only missing packages whose version the user has not rejected
    let mut missing: Vec<(String, Option<String>, String)> =
        manifest_names(def.ecosystem, &manifest)
            .into_iter()
            .filter(|name| !removed_packages.contains(name) && !local_packages.contains(name))
            .filter(|name| scope.includes(def.state_key, name))
            .map(|name| trust.trusted_pin(def, name))
            .filter(|(name, version, _)| {
                !trust
                    .inbox
                    .is_rejected(def.state_key, name, version.as_deref(), None)
            })
            .filter(|(name, version, _)| {
                retry_due(failures, def.state_key, name, version.as_deref())
            })
            .collect();
    if !missing.is_empty() {
        // A failed listing only costs an install that finds the package present
        let installed = manager.installed_names().await.unwrap_or_default();
        missing.retain(|(name, _, _)| !installed.contains(name));
    }

    if missing.is_empty() {
        return false;
    }
    // A failure is recorded against the version the manifest asked for, which the retry
    // check above compares
    let asked: HashMap<String, Option<String>> = missing
        .iter()
        .map(|(name, version, _)| (name.clone(), version.clone()))
        .collect();
    let foreign: HashSet<String> = missing
        .iter()
        .filter(|(name, _, _)| {
            trust
                .provenance
                .foreign_pin(def.ecosystem, def.state_key, name)
        })
        .map(|(name, _, _)| name.clone())
        .collect();

    let allowed = gate_simple(
        def,
        manager.as_ref(),
        trust,
        missing,
        &HashSet::new(),
        gated,
    )
    .await;

    if allowed.is_empty() {
        return false;
    }

    Output::info(&format!(
        "Installing {} {} package{}...",
        allowed.len(),
        def.display_name,
        if allowed.len() == 1 { "" } else { "s" }
    ));

    let mut installed_any = false;
    for line in allowed {
        let (name, version) = parse_pin(def.ecosystem, &line);
        let asked = asked.get(&name).cloned().flatten();
        match manager
            .install(&PackageInfo {
                name: name.clone(),
                version,
            })
            .await
        {
            Ok(()) => {
                installed_any = true;
                failures.remove(&InstallFailure::key(def.state_key, &name));
            }
            Err(e) => {
                let error = e.to_string();
                let first_line = error_line(&error);
                let min_age = PackagePolicy::load().min_release_age_days;
                let age =
                    refused_version(&error) && too_new(def, &name, asked.as_deref(), min_age).await;
                // Only a release that this machine cannot take falls back to an older one
                if !age && !foreign.contains(&name) {
                    record_failure(failures, def.state_key, &name, asked.as_deref(), &error);
                    continue;
                }
                let reason = if age {
                    Reason::PinnedTooNew
                } else {
                    Reason::OtherOsVersion
                };
                match fallback(
                    def,
                    manager.as_ref(),
                    trust,
                    &name,
                    asked.as_deref(),
                    reason,
                )
                .await
                {
                    Ok(Fallback::Installed(version)) => {
                        installed_any = true;
                        failures.remove(&InstallFailure::key(def.state_key, &name));
                        let why = if age {
                            format!("is newer than the release-age limit of {} days", min_age)
                        } else {
                            format!(
                                "a machine on another OS lists and which failed here: {}",
                                first_line
                            )
                        };
                        Output::info(&format!(
                            "Installed {} {} as approved, instead of {}, which {}",
                            name,
                            version,
                            asked.as_deref().unwrap_or("the pinned version"),
                            why
                        ));
                    }
                    Ok(Fallback::Held(item)) => {
                        let older = item.version.as_deref().unwrap_or_default();
                        let message = if age {
                            format!(
                                "{} {} is newer than the release-age limit of {} days. The \
                                 older release {} waits in the inbox for approval",
                                name,
                                asked.as_deref().unwrap_or_default(),
                                min_age,
                                older
                            )
                        } else {
                            format!(
                                "{} {} cannot be installed here: {}. The release {} waits in \
                                 the inbox for approval",
                                name,
                                asked.as_deref().unwrap_or_default(),
                                first_line,
                                older
                            )
                        };
                        record_failure(failures, def.state_key, &name, asked.as_deref(), &message);
                        // The held item replaces the pass the failed version got
                        gated
                            .passed
                            .retain(|(m, n)| m != def.state_key || *n != name);
                        gated.held.push(*item);
                    }
                    Err(fallback) => record_failure(
                        failures,
                        def.state_key,
                        &name,
                        asked.as_deref(),
                        &format!("{}; no fallback release: {}", error, fallback),
                    ),
                }
            }
        }
    }
    installed_any
}

/// Whether the registry shows that the pinned release came out within the release-age
/// limit, so that the limit is why it failed. A publish time that Tether cannot read does
/// not count.
async fn too_new(def: &PackageManagerDef, name: &str, pinned: Option<&str>, min_age: u32) -> bool {
    let Some(pinned) = pinned.filter(|_| min_age > 0) else {
        return false;
    };
    matches!(
        crate::packages::resolve::old_enough(def.state_key, def.ecosystem, name, pinned, min_age)
            .await,
        Ok(false)
    )
}

/// Whether a manager's install error says that no release matched the version, as npm, pnpm,
/// bun and uv report a release that the release-age limit holds back. Other failures, such as
/// a full disk, the network or an install script, never make an older release install.
fn refused_version(error: &str) -> bool {
    let error = error.to_ascii_lowercase();
    [
        // npm: "code ETARGET", "notarget No matching version found for x@1 with a date before"
        "etarget",
        "notarget",
        "no matching version",
        // pnpm
        "no_mature_matching_version",
        "no_matching_version",
        // bun: "No version matching \"1.0.0\" found for specifier"
        "no version matching",
        // uv: "No solution found ... there is no version of x==1.0.0"
        "no solution found",
        "there is no version of",
        "minimum release age",
        "minimum-release-age",
        "minimumreleaseage",
    ]
    .iter()
    .any(|p| error.contains(p))
}

/// The line of a manager's error that says what failed. Managers print progress first,
/// such as bun's "Resolving dependencies".
fn error_line(error: &str) -> &str {
    let lines: Vec<&str> = error
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect();
    lines
        .iter()
        .find(|l| l.to_ascii_lowercase().contains("error"))
        .or(lines.last())
        .copied()
        .unwrap_or_default()
}

/// What happened to the release that suits this machine.
#[derive(Debug)]
enum Fallback {
    /// The user approved that release before, so it installed
    Installed(String),
    /// No trusted record lists that release, so it waits for approval
    Held(Box<InboxItem>),
}

/// Find the release an unpinned line would get on this machine, after a version that only
/// machines on another OS list failed. That release gets the release-age limit, rejections
/// and OSV, and installs only when the user approved it.
async fn fallback(
    def: &PackageManagerDef,
    manager: &dyn PackageManager,
    trust: &Trust,
    name: &str,
    failed: Option<&str>,
    reason: Reason,
) -> Result<Fallback> {
    let min_age = PackagePolicy::load().min_release_age_days;
    let version = resolve_version(def.state_key, def.ecosystem, name, min_age).await?;
    if failed == Some(version.as_str()) {
        anyhow::bail!("it is the newest release that suits this machine");
    }
    let advisories = osv::advisories(def.ecosystem, &[(name.to_string(), Some(version.clone()))])
        .await
        .into_iter()
        .next()
        .unwrap_or_default();
    let decided = fallback_decision(def, trust, name, version, advisories, reason)?;
    if let Fallback::Installed(version) = &decided {
        manager
            .install(&PackageInfo {
                name: name.to_string(),
                version: Some(version.clone()),
            })
            .await?;
    }
    Ok(decided)
}

/// Never install a release no trusted record lists without the user's approval of that
/// exact release. A malicious release is held, and approval cannot install it.
fn fallback_decision(
    def: &PackageManagerDef,
    trust: &Trust,
    name: &str,
    version: String,
    advisories: Vec<String>,
    reason: Reason,
) -> Result<Fallback> {
    let key = def.state_key;
    if trust.inbox.is_rejected(key, name, Some(&version), None) {
        anyhow::bail!("you rejected {} {}", name, version);
    }
    if trust.inbox.holds_malicious(key, name, Some(&version)) {
        anyhow::bail!("the inbox holds {} {} as malicious", name, version);
    }
    let malicious = advisories.iter().any(|id| osv::is_malicious(id));
    if !malicious && trust.inbox.is_approved(key, name, Some(&version), None) {
        if !advisories.is_empty() {
            Output::warning(&format!(
                "{} {} has known vulnerabilities: {}",
                name,
                version,
                advisories.join(", ")
            ));
        }
        return Ok(Fallback::Installed(version));
    }
    let reason = if malicious { Reason::Malicious } else { reason };
    let mut item = trust.item(key, name, Some(version), None, vec![reason]);
    item.advisories = advisories;
    Ok(Fallback::Held(Box::new(item)))
}

/// A package a rollback would install, at the version it had at the snapshot.
pub struct RollbackLine {
    pub name: String,
    pub version: Option<String>,
    /// The newest version a trusted machine record lists now
    pub trusted: Option<String>,
}

impl RollbackLine {
    /// Rollback is the user's choice to go back, so it may install a version other than the
    /// newest trusted one, but only when the user confirms that version in a terminal.
    pub fn needs_confirmation(&self) -> bool {
        self.version.is_some() && self.version != self.trusted
    }
}

/// The trust checks for the packages a rollback would install, loaded once.
pub struct RollbackGate {
    def: &'static PackageManagerDef,
    trust: Trust,
}

impl RollbackGate {
    /// The caller holds the sync lock.
    pub fn load(
        config: &Config,
        sync_path: &Path,
        machine_id: &str,
        state_key: &str,
    ) -> Result<Self> {
        let Some(def) = SIMPLE_MANAGERS.iter().find(|d| d.state_key == state_key) else {
            anyhow::bail!("Rollback is not supported for {}", state_key);
        };
        Ok(Self {
            def,
            trust: Trust::load(config, sync_path, machine_id)?,
        })
    }

    /// `packages` are names with the version each had at the snapshot, if known.
    pub fn plan(&self, packages: Vec<(String, Option<String>)>) -> Vec<RollbackLine> {
        packages
            .into_iter()
            .filter(|(name, version)| {
                registry_release(self.def.ecosystem, name, version.as_deref())
            })
            .map(|(name, version)| RollbackLine {
                trusted: self.trust.provenance.newest_trusted_version(
                    self.def.ecosystem,
                    self.def.state_key,
                    &name,
                ),
                name,
                version,
            })
            .collect()
    }

    /// The version to install: the snapshot's when the user confirmed it, else the newest
    /// trusted one, as a sync picks. Without a trusted one the snapshot's version stays,
    /// and the trust checks hold it.
    fn pin(&self, line: RollbackLine, confirmed: bool) -> (String, Option<String>, String) {
        let version = match line.trusted {
            Some(trusted) if !confirmed => Some(trusted),
            _ => line.version,
        };
        let spec = format_pin(self.def.ecosystem, &line.name, version.as_deref());
        (line.name, version, spec)
    }

    /// Check each line like a synced import. A version the user confirmed counts as
    /// approved, but a malicious one is still refused. Returns the install specs; the rest
    /// go to the approval inbox.
    pub async fn gate(
        &self,
        lines: Vec<RollbackLine>,
        confirmed: &HashSet<String>,
    ) -> Result<Vec<String>> {
        let Some(manager) = crate::packages::manager_for_key(self.def.state_key) else {
            anyhow::bail!("Rollback is not supported for {}", self.def.state_key);
        };
        let key = self.def.state_key;
        let candidates = lines
            .into_iter()
            .map(|line| {
                let confirmed = confirmed.contains(&line.name);
                (confirmed, self.pin(line, confirmed))
            })
            .filter(|(confirmed, (name, version, _))| {
                *confirmed
                    || !self
                        .trust
                        .inbox
                        .is_rejected(key, name, version.as_deref(), None)
            })
            .map(|(_, entry)| entry)
            .collect();
        let mut gated = Gated::default();
        let allowed = gate_simple(
            self.def,
            manager.as_ref(),
            &self.trust,
            candidates,
            confirmed,
            &mut gated,
        )
        .await;
        // A snapshot may name packages no record lists now, and their items stay
        report_held(&inbox::settle(gated.held, &gated.passed, |_, _| true)?);
        Ok(allowed)
    }
}

/// Whether a name and version name a registry release. Other entries never reach the trust
/// checks, OSV or a package manager.
fn registry_release(ecosystem: Ecosystem, name: &str, version: Option<&str>) -> bool {
    let checked = crate::packages::validate_name(ecosystem, name)
        .and_then(|()| version.map_or(Ok(()), |v| crate::packages::validate_version(ecosystem, v)));
    match checked {
        Ok(()) => true,
        Err(e) => {
            let message = format!("Skipping manifest line: {}", e);
            if crate::packages::policy::first_warning(&message) {
                Output::warning(&message);
            }
            false
        }
    }
}

/// The package names in a manifest. Manifests stay names-only, as 1.x reads them. A
/// pre-release 2.0 build wrote pinned lines: Tether drops their version, because only
/// trusted records give versions.
fn manifest_names(ecosystem: Ecosystem, manifest: &str) -> Vec<String> {
    crate::packages::pin::manifest_names(ecosystem, manifest)
        .into_iter()
        .filter(|name| registry_release(ecosystem, name, None))
        .collect()
}

/// How the release age of a package was checked.
#[derive(Debug, Clone, Copy, PartialEq)]
enum AgeCheck {
    /// The manager holds to the limit, or the release is old enough
    Met,
    /// The pinned release is newer than the limit
    TooNew,
    /// The manager cannot hold to the limit, and the registry could not tell
    Unchecked,
}

/// The release age of a package whose manager may not hold to the limit. Tether checks it
/// itself from the registry: a resolved release already meets the limit, and a pinned one
/// must be old enough by its publish time. `pinned` is that publish-time check.
fn age_check(cooldown_unsupported: bool, resolved: bool, pinned: Option<Result<bool>>) -> AgeCheck {
    if !cooldown_unsupported {
        return AgeCheck::Met;
    }
    match pinned {
        Some(Ok(true)) => AgeCheck::Met,
        Some(Ok(false)) => AgeCheck::TooNew,
        Some(Err(_)) => AgeCheck::Unchecked,
        None if resolved => AgeCheck::Met,
        None => AgeCheck::Unchecked,
    }
}

/// Keep the lines that may install and queue the rest with their OSV advisories.
/// `confirmed` names packages whose version the user confirmed just now.
async fn gate_simple(
    def: &PackageManagerDef,
    manager: &dyn PackageManager,
    trust: &Trust,
    missing: Vec<(String, Option<String>, String)>,
    confirmed: &HashSet<String>,
    gated: &mut Gated,
) -> Vec<String> {
    let cooldown_unsupported = manager.cooldown().await == Cooldown::Unsupported;
    let min_age = crate::packages::PackagePolicy::load().min_release_age_days;
    let base = |name: &str, version: Option<&str>| {
        let mut checks = trust.checks(def.state_key, name, version, None);
        checks.approved |= confirmed.contains(name);
        checks
    };
    // An unpinned line installs the release resolved here, so OSV checks that release. A
    // package that waits for trust anyway is not resolved: OSV checks all its releases.
    let mut pins: Vec<(String, Option<String>)> = Vec::new();
    let mut ages = Vec::new();
    for (name, version, _) in &missing {
        if !inbox::reasons(base(name, version.as_deref())).is_empty() {
            pins.push((name.clone(), version.clone()));
            ages.push(AgeCheck::Met);
            continue;
        }
        let resolved = match version {
            Some(v) => Some(v.clone()),
            None => resolve_version(def.state_key, def.ecosystem, name, min_age)
                .await
                .ok(),
        };
        let pinned = match (cooldown_unsupported, version) {
            (true, Some(v)) => Some(
                crate::packages::resolve::old_enough(
                    def.state_key,
                    def.ecosystem,
                    name,
                    v,
                    min_age,
                )
                .await,
            ),
            _ => None,
        };
        ages.push(age_check(cooldown_unsupported, resolved.is_some(), pinned));
        pins.push((name.clone(), resolved));
    }
    let advisories = osv::advisories(def.ecosystem, &pins).await;
    let mut allowed = Vec::new();
    for ((((name, version, line), (_, resolved)), advisories), age) in
        missing.into_iter().zip(pins).zip(advisories).zip(ages)
    {
        let malicious = advisories.iter().any(|id| osv::is_malicious(id));
        let checks = Checks {
            cooldown_unsupported: age == AgeCheck::Unchecked,
            too_new: age == AgeCheck::TooNew,
            malicious: malicious && resolved.is_some(),
            malicious_unresolved: malicious && resolved.is_none(),
            ..base(&name, version.as_deref())
        };
        let reasons = inbox::reasons(checks);
        if reasons.is_empty() {
            if checks.malicious_unresolved {
                Output::warning(&format!(
                    "Installing {} as approved, although OSV lists MALICIOUS releases of it ({}) \
                     and Tether could not find the release that installs",
                    line,
                    advisories.join(", ")
                ));
            } else if !advisories.is_empty() {
                Output::warning(&format!(
                    "{} has known vulnerabilities: {}",
                    line,
                    advisories.join(", ")
                ));
            }
            gated.passed.push((def.state_key.to_string(), name.clone()));
            allowed.push(match (&version, &resolved) {
                (None, Some(resolved)) => format_pin(def.ecosystem, &name, Some(resolved)),
                _ => line,
            });
        } else {
            let mut item = trust.item(def.state_key, &name, version, None, reasons);
            item.advisories = advisories;
            gated.held.push(item);
        }
    }
    allowed
}

/// The records a manifest export reads: this machine's current record, and each other
/// record whose signature verifies against the key trusted for its machine id and is the
/// newest that key signed. Anything else could put untrusted lines in this machine's commit.
fn export_records(
    records: Vec<SignedRecord>,
    this: &MachineState,
    store: &TrustStore,
    generations: &Generations,
) -> Vec<MachineState> {
    let mut machines: Vec<MachineState> = records
        .into_iter()
        .filter(|r| {
            r.record.machine_id != this.machine_id
                && r.signer.as_ref().is_some_and(|signer| {
                    store.trusts(&r.record.machine_id, signer)
                        && generations.current(signer, r.record.generation, &r.digest)
                })
        })
        .map(|r| r.record)
        .collect();
    machines.push(this.clone());
    machines
}

/// The packages that some machine record in the repo lists, trusted or not. A manifest
/// keeps a line while any record lists its package.
#[derive(Default)]
struct Listed {
    names: HashSet<(String, String)>,
}

impl Listed {
    /// This machine's current record replaces its copy in the repo, which may still list
    /// a package it removed since.
    fn new(records: &[SignedRecord], this: &MachineState) -> Self {
        let mut listed = Self::default();
        for record in records
            .iter()
            .map(|r| &r.record)
            .filter(|r| r.machine_id != this.machine_id)
            .chain([this])
        {
            for (manager, names) in &record.packages {
                for name in names {
                    listed.names.insert((manager.clone(), name.clone()));
                }
            }
        }
        listed
    }

    fn lists(&self, manager: &str, name: &str) -> bool {
        self.names
            .contains(&(manager.to_string(), name.to_string()))
    }

    /// Whether an inbox item still concerns a listed package. A Homebrew formula or cask
    /// may be named with or without its tap.
    fn holds(&self, manager: &str, name: &str) -> bool {
        self.lists(manager, name)
            || (manager.starts_with("brew_")
                && self.names.iter().any(|(m, n)| {
                    m == manager && normalize_formula_name(n) == normalize_formula_name(name)
                }))
    }
}

/// Merge this machine's trusted packages into a manifest. Machines trust different
/// records, so none replaces the file: each adds the packages its trusted records list,
/// and keeps lines it does not trust, because a line grants no trust. A line goes only
/// when no record lists its package. The manifest stays names-only and sorted, the format
/// 1.x writes and reads, so 1.x and 2.0 machines in one repo agree on it.
fn merge_manifest(
    ecosystem: Ecosystem,
    manager: &str,
    file: Option<&str>,
    trusted: Option<&Vec<String>>,
    listed: &Listed,
) -> String {
    let mut names: std::collections::BTreeSet<String> =
        crate::packages::pin::manifest_names(ecosystem, file.unwrap_or_default())
            .into_iter()
            .filter(|name| listed.lists(manager, name))
            .collect();
    names.extend(trusted.into_iter().flatten().cloned());
    if names.is_empty() {
        String::new()
    } else {
        names.into_iter().collect::<Vec<_>>().join("\n") + "\n"
    }
}

/// The Brewfile merges like the other manifests, without versions.
fn merge_brewfile(
    file: Option<&str>,
    trusted: &HashMap<String, Vec<String>>,
    listed: &Listed,
) -> String {
    let mut packages = BrewfilePackages::parse(file.unwrap_or_default());
    for (manager, names) in [
        ("brew_taps", &mut packages.taps),
        ("brew_formulae", &mut packages.formulae),
        ("brew_casks", &mut packages.casks),
    ] {
        names.retain(|name| listed.lists(manager, name));
        names.extend(trusted.get(manager).into_iter().flatten().cloned());
        names.sort();
        names.dedup();
    }
    packages.generate()
}

/// Merge package manifests with the records this machine trusts
pub async fn sync_packages(
    config: &Config,
    state: &mut SyncState,
    sync_path: &Path,
    machine_state: &MachineState,
    dry_run: bool,
) -> Result<()> {
    let manifests_dir = sync_path.join("manifests");
    std::fs::create_dir_all(&manifests_dir)?;

    let records = signing::records(sync_path);
    let listed = Listed::new(&records, machine_state);
    let machines = export_records(
        records,
        machine_state,
        &TrustStore::load()?,
        &Generations::load()?,
    );
    let union_packages = MachineState::compute_union_packages(&machines);

    if config.packages.brew.enabled {
        let path = manifests_dir.join("Brewfile");
        let file = std::fs::read_to_string(&path).ok();
        let manifest = merge_brewfile(file.as_deref(), &union_packages, &listed);
        write_manifest(state, "brew", &path, file, manifest, dry_run)?;
    }

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
            let path = manifests_dir.join(def.manifest_file);
            let file = std::fs::read_to_string(&path).ok();
            let manifest = merge_manifest(
                def.ecosystem,
                def.state_key,
                file.as_deref(),
                union_packages.get(def.state_key),
                &listed,
            );
            write_manifest(state, def.state_key, &path, file, manifest, dry_run)?;
        }
    }

    Ok(())
}

/// Write the manifest when the merge changed it, and record when it last changed.
fn write_manifest(
    state: &mut SyncState,
    key: &str,
    path: &Path,
    file: Option<String>,
    manifest: String,
    dry_run: bool,
) -> Result<()> {
    if dry_run {
        return Ok(());
    }
    let now = chrono::Utc::now();
    let changed = file.as_deref() != Some(manifest.as_str());
    if changed {
        std::fs::write(path, &manifest)?;
    }
    let existing = state.packages.get(key);
    state.packages.insert(
        key.to_string(),
        PackageState {
            last_sync: now,
            last_modified: if changed {
                Some(now)
            } else {
                existing.and_then(|e| e.last_modified)
            },
            last_upgrade: existing.and_then(|e| e.last_upgrade),
            hash: crate::sha256_hex(manifest.as_bytes()),
        },
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn release_age_rejection_reads_as_such() {
        // Without a limit or a pin, the limit is never the reason, and no registry is asked
        let npm = &SIMPLE_MANAGERS[0];
        assert!(!too_new(npm, "example", Some("2.0.0"), 0).await);
        assert!(!too_new(npm, "example", None, 7).await);
        assert_eq!(
            error_line("bun command failed: Resolving dependencies\nerror: No version matching"),
            "error: No version matching"
        );
        assert_eq!(
            error_line("bun command failed: Resolving dependencies\nResolved, downloaded\n"),
            "Resolved, downloaded"
        );
    }

    /// Only an error that says no release matched can be the release-age limit; other
    /// install failures get no fallback release
    #[test]
    fn only_a_version_refusal_can_be_too_new() {
        for refused in [
            // npm 11.19 with --min-release-age
            "npm command failed: npm error code ETARGET\nnpm error notarget No matching \
             version found for typescript@7.1.0-dev.20261007.1 with a date before \
             10/1/2026, 2:53:47 AM.",
            " ERR_PNPM_NO_MATURE_MATCHING_VERSION  No matching version found for x@1.0.0",
            "bun command failed: Resolving dependencies\nerror: No version matching \"1.0.0\" \
             found for specifier \"x\"",
            "× No solution found when resolving tool dependencies:\n  ╰─▶ Because there is no \
             version of ruff==0.99.0",
        ] {
            assert!(refused_version(refused), "{refused}");
        }
        for other in [
            "npm error code ENOSPC\nnpm error syscall write\nnpm error no space left on device",
            "npm error code ECONNRESET\nnpm error network aborted",
            "npm error code ELIFECYCLE\nnpm error command failed\nnpm error command sh -c \
             node install.js",
            "error: EACCES: permission denied, mkdir '/usr/lib/node_modules/x'",
            "bun command failed: error: ConnectionRefused downloading package manifest x",
            "uv command failed: error: Failed to fetch: `https://pypi.org/simple/x/`",
        ] {
            assert!(!refused_version(other), "{other}");
        }
    }

    #[test]
    fn excluded_notice_counts_instead_of_naming() {
        let mut config = Config::default();
        config.machine_profiles.insert("me".into(), "server".into());
        let me = MachineState::new("me");
        let mut dev = MachineState::new("dev1");
        let mut laptop = MachineState::new("laptop1");
        dev.profile = Some("dev".into());
        laptop.profile = Some("laptop".into());
        for i in 0..200 {
            dev.packages
                .entry("brew_formulae".into())
                .or_default()
                .push(format!("formula-{i}"));
        }
        dev.packages
            .insert("npm".into(), vec!["shared".into(), "only-dev".into()]);
        laptop.packages.insert("npm".into(), vec!["shared".into()]);
        let scope = Membership::new(
            &config,
            &Default::default(),
            &me,
            &[(&dev, true), (&laptop, true)],
        );
        let notice = excluded_notice(&scope).unwrap();
        assert!(notice.contains("202 packages of other profiles do not install here"));
        assert!(
            notice.contains("By manager: brew_formulae 200, npm 2."),
            "{notice}"
        );
        assert!(
            notice.contains("By profile: dev 202, laptop 1."),
            "{notice}"
        );
        assert!(notice.contains("'tether packages list --other-profiles'"));
        assert!(notice.contains("--to server"));
        assert!(!notice.contains("formula-1"), "{notice}");

        let alone = Membership::new(&config, &Default::default(), &me, &[]);
        assert_eq!(excluded_notice(&alone), None);
    }

    #[test]
    fn manifests_are_names_only() {
        let packages = vec!["@types/node".to_string(), "left-pad".to_string()];
        let listed = Listed::default();
        assert_eq!(
            merge_manifest(Ecosystem::Npm, "npm", None, Some(&packages), &listed),
            "@types/node\nleft-pad\n"
        );
        assert_eq!(
            merge_manifest(Ecosystem::Npm, "npm", None, None, &listed),
            ""
        );
        // A pre-release build wrote pinned lines; the next write drops the versions
        for (ecosystem, manager, file, expected) in [
            (
                Ecosystem::Npm,
                "npm",
                "@scope/pkg@1.0.0\nleft-pad@2.0.0\n",
                "@scope/pkg\nleft-pad\n",
            ),
            (Ecosystem::Python, "uv", "ruff==0.6.0\n", "ruff\n"),
            (Ecosystem::Gem, "gem", "rails:7.1.0\n", "rails\n"),
        ] {
            let mut listed = Listed::default();
            for name in crate::packages::pin::manifest_names(ecosystem, file) {
                listed.names.insert((manager.to_string(), name));
            }
            assert_eq!(
                merge_manifest(ecosystem, manager, Some(file), None, &listed),
                expected
            );
        }
    }

    /// The installs 1.x (1.11.10, 1.12.0 and 1.13.1) starts from a manifest: each trimmed,
    /// non-empty line that is not an installed name or a removal, installed as the line.
    fn installs_on_1x(manifest: &str, installed: &[&str], removed: &[&str]) -> Vec<String> {
        manifest
            .lines()
            .filter(|line| {
                let pkg = line.trim();
                !pkg.is_empty() && !removed.contains(&pkg) && !installed.contains(&pkg)
            })
            .map(|s| s.to_string())
            .collect()
    }

    #[test]
    fn a_manifest_from_2_0_reads_on_1x_as_installed_names() {
        let installed = ["@scope/pkg", "left-pad", "ruff", "rails"];
        let mut listed = Listed::default();
        for (manager, name) in [
            ("npm", "@scope/pkg"),
            ("npm", "left-pad"),
            ("uv", "ruff"),
            ("gem", "rails"),
        ] {
            listed.names.insert((manager.to_string(), name.to_string()));
        }
        for (ecosystem, manager, file, trusted) in [
            (
                Ecosystem::Npm,
                "npm",
                "@scope/pkg@1.0.0\n",
                vec!["left-pad".to_string()],
            ),
            (Ecosystem::Python, "uv", "ruff==0.6.0\n", vec![]),
            (Ecosystem::Gem, "gem", "rails:7.1.0\n", vec![]),
        ] {
            let written = merge_manifest(ecosystem, manager, Some(file), Some(&trusted), &listed);
            assert!(
                installs_on_1x(&written, &installed, &[]).is_empty(),
                "1.x would reinstall from {written:?}"
            );
        }
        // A missing package installs on 1.x by its name
        let written = merge_manifest(
            Ecosystem::Npm,
            "npm",
            None,
            Some(&vec!["new".to_string()]),
            &listed,
        );
        assert_eq!(installs_on_1x(&written, &installed, &[]), ["new"]);
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
        // A manifest never gives a version, so a pinned line reads as its name only
        assert_eq!(
            manifest_names(
                Ecosystem::Npm,
                "example@latest\nrange@^1.0.0\nok@1.0.0\nlegacy\n--flag\n"
            ),
            ["example", "range", "ok", "legacy"]
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

    /// The npm lines `me` writes from these records into an empty manifest.
    fn exported_npm(dir: &Path, me: &str, store: &TrustStore) -> Vec<String> {
        let this = MachineState::list_all(dir)
            .unwrap()
            .into_iter()
            .find(|m| m.machine_id == me)
            .unwrap_or_else(|| MachineState::new(me));
        let records = signing::records(dir);
        let listed = Listed::new(&records, &this);
        let machines = export_records(records, &this, store, &Generations::default());
        merge_manifest(
            Ecosystem::Npm,
            "npm",
            None,
            MachineState::compute_union_packages(&machines).get("npm"),
            &listed,
        )
        .lines()
        .map(str::to_string)
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
        assert!(!exported_npm(path, "me", &store)
            .iter()
            .any(|name| name == "evilpkg"));
        // An older build's union, or anyone who can push, can still put it in the manifest
        std::fs::write(
            path.join("manifests/npm.txt"),
            "evilpkg@6.6.6\ngood@1.0.0\nleft-pad@1.0.0\n",
        )
        .unwrap();
        // The exporting machine signs the commit that carries the manifest
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
        // The manifest names the package only; the version comes from the trusted records,
        // so every machine that trusts t and u installs the same version
        assert_eq!(exported_npm(path, "me", &store), ["example"]);

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
    fn manifest_export_reads_only_trusted_current_records() {
        let (tmp, me, t, store) = two_machines();
        let path = tmp.path();
        let stranger = new_key();
        record(path, "me", &[("stale-own", "1.0.0")], Some(&me));
        record(path, "t", &[("good", "1.0.0")], Some(&t));
        record(path, "unsigned", &[("a", "1.0.0")], None);
        record(path, "other-key", &[("b", "1.0.0")], Some(&stranger));
        let mut this = MachineState::new("me");
        this.packages
            .insert("npm".to_string(), vec!["mine".to_string()]);

        let ids = |generations: &Generations| {
            let mut ids: Vec<String> =
                export_records(signing::records(path), &this, &store, generations)
                    .into_iter()
                    .map(|m| m.machine_id)
                    .collect();
            ids.sort();
            ids
        };
        assert_eq!(ids(&Generations::default()), ["me", "t"]);
        let me_record = export_records(
            signing::records(path),
            &this,
            &store,
            &Generations::default(),
        )
        .into_iter()
        .find(|m| m.machine_id == "me")
        .unwrap();
        assert_eq!(me_record.packages["npm"], ["mine"]);

        // A replayed record of t no longer feeds the manifest
        let mut generations = Generations::default();
        assert!(generations.accept(t.public_key(), 5, "newer"));
        assert_eq!(ids(&generations), ["me"]);
    }

    #[test]
    fn machines_with_different_trust_do_not_undo_each_others_manifest() {
        // Each machine trusts only its own record
        let a = ["foo", "baz"];
        let b = ["bar", "baz"];
        let listed = listed_npm(&[&a, &b]);
        for (a, b, expected) in [
            (&a[..1], &b[..1], "bar\nfoo\n"),
            (&a[..], &b[..], "bar\nbaz\nfoo\n"),
        ] {
            let mut file: Option<String> = None;
            let mut round = || {
                let mut writes = 0;
                for trusted in [a, b] {
                    let merged = merge_npm(file.as_deref(), trusted, &listed);
                    if file.as_deref() != Some(merged.as_str()) {
                        writes += 1;
                        file = Some(merged);
                    }
                }
                writes
            };
            assert_eq!(round(), 2);
            assert_eq!(round(), 0, "a second round without changes writes nothing");
            assert_eq!(file.as_deref(), Some(expected));
        }
    }

    #[test]
    fn an_unpinned_manifest_keeps_every_package_some_record_lists() {
        // A 1.13.1 manifest. This machine trusts only its own record; an untrusted or
        // offline machine still lists bar and baz, and no record lists gone.
        let mine = ["foo"];
        let listed = listed_npm(&[&mine, &["bar", "baz"]]);
        let merged = merge_npm(Some("bar\nbaz\nfoo\ngone\n"), &mine, &listed);
        assert_eq!(merged, "bar\nbaz\nfoo\n");
    }

    #[test]
    fn brewfile_keeps_entries_some_record_lists() {
        let mut listed = Listed::default();
        for (manager, name) in [("brew_formulae", "wget"), ("brew_casks", "zed")] {
            listed.names.insert((manager.to_string(), name.to_string()));
        }
        let trusted = HashMap::from([("brew_formulae".to_string(), vec!["jq".to_string()])]);
        let file = "tap \"old/tap\"\nbrew \"wget\"\nbrew \"gone\"\ncask \"zed\"\n";
        assert_eq!(
            merge_brewfile(Some(file), &trusted, &listed),
            "brew \"jq\"\nbrew \"wget\"\ncask \"zed\"\n"
        );
    }

    /// The packages a set of npm records list.
    fn listed_npm(records: &[&[&str]]) -> Listed {
        let mut listed = Listed::default();
        for name in records.iter().copied().flatten() {
            listed.names.insert(("npm".to_string(), name.to_string()));
        }
        listed
    }

    /// The npm manifest a machine writes when its trusted records list `trusted`.
    fn merge_npm(file: Option<&str>, trusted: &[&str], listed: &Listed) -> String {
        let names = trusted.iter().map(|n| n.to_string()).collect();
        merge_manifest(Ecosystem::Npm, "npm", file, Some(&names), listed)
    }

    /// Write `id`'s signed record from a machine on `os`, listing one npm package.
    fn record_on(dir: &Path, id: &str, os: &str, npm: (&str, &str), key: &ssh_key::PrivateKey) {
        let mut machine = MachineState::new(id);
        machine.os = os.to_string();
        machine
            .packages
            .insert("npm".to_string(), vec![npm.0.to_string()]);
        machine.package_versions.insert(
            "npm".to_string(),
            HashMap::from([(npm.0.to_string(), npm.1.to_string())]),
        );
        machine.save_to_repo(dir).unwrap();
        signing::sign_record(dir, id, key).unwrap();
    }

    #[test]
    fn a_release_for_this_os_waits_for_approval_of_that_release() {
        let (tmp, me, _, store) = two_machines();
        let mut trust = trust_as(tmp.path(), "me", &me, &store);
        let npm = &SIMPLE_MANAGERS[0];
        let decide = |trust: &Trust, advisories: &[&str]| {
            let advisories = advisories.iter().map(|a| a.to_string()).collect();
            fallback_decision(
                npm,
                trust,
                "example",
                "1.5.0".to_string(),
                advisories,
                Reason::OtherOsVersion,
            )
        };
        let held = |decision: Result<Fallback>| match decision.unwrap() {
            Fallback::Held(item) => (item.version.unwrap(), item.reasons),
            other => panic!("installed without approval: {other:?}"),
        };

        assert_eq!(
            held(decide(&trust, &["GHSA-x"])),
            ("1.5.0".to_string(), vec![Reason::OtherOsVersion])
        );
        assert_eq!(
            held(decide(&trust, &["MAL-2026-1"])).1,
            vec![Reason::Malicious]
        );
        // A pin newer than the release-age limit holds the older release the same way
        let too_new = fallback_decision(
            npm,
            &trust,
            "example",
            "1.5.0".to_string(),
            Vec::new(),
            Reason::PinnedTooNew,
        );
        assert_eq!(held(too_new).1, vec![Reason::PinnedTooNew]);

        let decision = |version: &str| inbox::Decision {
            manager: "npm".to_string(),
            name: "example".to_string(),
            version: Some(version.to_string()),
            tap: None,
            fingerprint: None,
            at: chrono::Utc::now(),
        };
        // An approval covers only the release it named
        trust.inbox.approved.push(decision("1.4.0"));
        assert!(matches!(decide(&trust, &[]).unwrap(), Fallback::Held(_)));
        trust.inbox.approved.push(decision("1.5.0"));
        assert!(matches!(
            decide(&trust, &[]).unwrap(),
            Fallback::Installed(v) if v == "1.5.0"
        ));
        // Approval never installs a malicious release
        assert_eq!(
            held(decide(&trust, &["MAL-2026-1"])).1,
            vec![Reason::Malicious]
        );

        trust.inbox.items.push(InboxItem {
            reasons: vec![Reason::Malicious],
            ..trust.item("npm", "example", Some("1.5.0".to_string()), None, vec![])
        });
        assert!(decide(&trust, &[]).is_err());
        trust.inbox.items.clear();
        trust.inbox.rejected.push(decision("1.5.0"));
        assert!(decide(&trust, &[]).is_err());
    }

    #[test]
    fn a_pin_only_another_os_lists_is_only_preferred() {
        let (tmp, me, t, mut store) = two_machines();
        let path = tmp.path();
        let u = new_key();
        store.trust("u", u.public_key()).unwrap();
        let here = std::env::consts::OS;
        record_on(path, "t", "otheros", ("example", "2.0.0"), &t);
        record_on(path, "u", here, ("example", "1.0.0"), &u);
        let trust = trust_as(path, "me", &me, &store);
        assert!(trust
            .provenance
            .foreign_pin(Ecosystem::Npm, "npm", "example"));

        // Once a machine on this OS lists the newest version, it must install as pinned
        record_on(path, "u", here, ("example", "2.0.0"), &u);
        let trust = trust_as(path, "me", &me, &store);
        assert!(!trust
            .provenance
            .foreign_pin(Ecosystem::Npm, "npm", "example"));

        // A package no trusted record lists has no trusted pin to fall back from
        assert!(!trust.provenance.foreign_pin(Ecosystem::Npm, "npm", "other"));
    }

    #[test]
    fn records_without_an_os_field_read_the_family_from_the_os_version() {
        let mut machine = MachineState::new("m");
        machine.os = String::new();
        machine.os_version = "macOS 15.5".to_string();
        assert_eq!(machine.os_family(), "macos");
        machine.os_version = "Ubuntu 24.04 LTS".to_string();
        assert_eq!(machine.os_family(), "linux");
        assert_eq!(same_os(&machine), std::env::consts::OS == "linux");
        machine.os_version = String::new();
        assert_eq!(machine.os_family(), "unknown");
        assert!(same_os(&machine));
        machine.os = "linux".to_string();
        assert_eq!(machine.os_family(), "linux");
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
        // A pre-release manifest pins the older version that a trusted record also lists
        let line = |l: &str| manifest_names(Ecosystem::Npm, l).remove(0);
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
        // A package no trusted record lists gets no version from the manifest and is held
        let other = trust.trusted_pin(def, line("other@6.6.6"));
        assert_eq!(other.1, None);
        assert_eq!(
            held(&trust, "other", other.1.as_deref()),
            vec![Reason::Unsigned]
        );
    }

    #[test]
    fn an_included_formula_brings_its_tap() {
        let packages = BrewfilePackages::parse(
            "tap \"vendor/tools\"\ntap \"other/tap\"\nbrew \"Vendor/Tools/thing\"\nbrew \"wget\"\ncask \"homebrew/cask/zoom\"\n",
        );
        let needed = needed_taps(&packages);
        assert_eq!(
            needed,
            HashSet::from(["vendor/tools".to_string(), "homebrew/cask".to_string()])
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
        let mut gated = Gated::default();
        let taps = ["listed/tap", "added/tap", "other/tap", "local/tap"]
            .map(String::from)
            .to_vec();
        let allowed = gate_taps(&policy, &trust, &local, taps.clone(), &mut gated);
        assert_eq!(allowed, vec!["listed/tap", "local/tap"]);
        // Allowed by policy, but no trusted record lists it: a repo writer added it
        assert_eq!(gated.held[0].name, "added/tap");
        assert_eq!(gated.held[0].reasons, vec![Reason::Unsigned]);
        assert_eq!(gated.held[1].name, "other/tap");
        assert_eq!(gated.held[1].reasons, vec![Reason::UntrustedTap]);

        // A held tap is checked again: once trusted, it taps and leaves the inbox
        let mut trust = trust;
        trust.inbox.settle(gated.held, &gated.passed, |_, _| true);
        let policy = crate::packages::PackagePolicy {
            trusted_taps: vec!["listed/tap".to_string(), "other/tap".to_string()],
            ..policy
        };
        let mut gated = Gated::default();
        let allowed = gate_taps(&policy, &trust, &local, taps, &mut gated);
        assert_eq!(allowed, vec!["listed/tap", "other/tap", "local/tap"]);
        trust.inbox.settle(gated.held, &gated.passed, |_, _| true);
        let pending: Vec<String> = trust.inbox.items.iter().map(|i| i.id()).collect();
        assert_eq!(pending, vec!["brew_taps:added/tap"]);
    }

    #[test]
    fn test_brew_decisions_bind_to_the_resolved_trusted_tap() {
        let (tmp, me, _, store) = two_machines();
        let mut trust = trust_as(tmp.path(), "me", &me, &store);
        let policy = crate::packages::PackagePolicy {
            min_release_age_days: 7,
            allow_scripts: Vec::new(),
            trusted_taps: vec!["vendor/one".to_string(), "vendor/two".to_string()],
            approved_from_taps: Vec::new(),
        };
        let gate = |trust: &Trust, tap: &str| {
            let mut gated = Gated::default();
            let allowed = gate_brew_entry(
                &policy,
                trust,
                "brew_formulae",
                "tool",
                Some(tap.to_string()),
                &mut gated,
            );
            (allowed, gated)
        };

        let (allowed, gated) = gate(&trust, "vendor/one");
        assert!(!allowed);
        assert_eq!(gated.held[0].tap.as_deref(), Some("vendor/one"));
        trust.inbox.add(gated.held[0].clone());
        let shown = gated.held[0].clone();
        trust.inbox.approve(&shown).unwrap();
        assert!(gate(&trust, "vendor/one").0);

        // The short name now resolves to another trusted tap: the approval does not carry
        let (allowed, gated) = gate(&trust, "vendor/two");
        assert!(!allowed);
        assert_eq!(gated.held[0].tap.as_deref(), Some("vendor/two"));
        trust.inbox.add(gated.held[0].clone());
        let shown = gated.held[0].clone();
        trust.inbox.reject(&shown).unwrap();
        assert!(gate(&trust, "vendor/two").1.held.is_empty());
        assert!(gate(&trust, "vendor/one").0);
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
        // A different record that the same key signed at the accepted generation
        write(5, "1.5.0");
        let provenance = Provenance::load(path, "me", me.public_key(), &store, &mut seen);
        assert_eq!(provenance.signer(&entry("1.5.0")), Signer::None);
    }

    #[test]
    fn trusted_records_that_fail_their_signature_are_reported() {
        let (tmp, me, t, mut store) = two_machines();
        let path = tmp.path();
        let other = new_key();
        store.trust("u", other.public_key()).unwrap();
        store.trust("v", new_key().public_key()).unwrap();
        // Tampered after signing
        record(path, "t", &[("a", "1.0.0")], Some(&t));
        let file = path.join("machines/t.json");
        let tampered = std::fs::read_to_string(&file)
            .unwrap()
            .replace("1.0.0", "0.9.0");
        std::fs::write(&file, tampered).unwrap();
        // No signature, and signed by a key not trusted for the id
        record(path, "u", &[("b", "1.0.0")], None);
        record(path, "v", &[("c", "1.0.0")], Some(&other));
        // Not trusted at all, and this machine's own record: neither is reported
        record(path, "stranger", &[("d", "1.0.0")], None);
        record(path, "me", &[("e", "1.0.0")], None);

        let trust = trust_as(path, "me", &me, &store);
        let ids: Vec<&str> = trust
            .provenance
            .failed
            .iter()
            .map(|(id, _)| id.as_str())
            .collect();
        assert_eq!(ids, ["t", "u", "v"]);
        assert_eq!(
            held(&trust, "a", Some("0.9.0")),
            vec![Reason::SignatureFailed]
        );
        assert_eq!(
            held(&trust, "b", Some("1.0.0")),
            vec![Reason::SignatureFailed]
        );
        assert_eq!(
            held(&trust, "c", Some("1.0.0")),
            vec![Reason::SignatureFailed]
        );
        assert_eq!(held(&trust, "d", Some("1.0.0")), vec![Reason::Unsigned]);
    }

    #[test]
    fn a_failing_record_is_reported_once_until_it_changes() {
        let failed = vec![("t".to_string(), "d1".to_string())];
        let mut warned = HashSet::from(["gone".to_string()]);
        assert_eq!(new_signature_failures(&failed, &mut warned), ["t"]);
        assert_eq!(warned, HashSet::from(["d1".to_string()]));
        assert!(new_signature_failures(&failed, &mut warned).is_empty());
        let changed = vec![("t".to_string(), "d2".to_string())];
        assert_eq!(new_signature_failures(&changed, &mut warned), ["t"]);
        assert!(new_signature_failures(&[], &mut warned).is_empty());
        assert!(warned.is_empty());
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
    }

    #[test]
    fn test_rollback_installs_an_older_version_only_when_confirmed() {
        let (tmp, me, t, store) = two_machines();
        let path = tmp.path();
        record(path, "t", &[("good", "2.0.0")], Some(&t));
        let gate = RollbackGate {
            def: &SIMPLE_MANAGERS[0],
            trust: trust_as(path, "me", &me, &store),
        };
        let lines = || {
            vec![
                ("good".to_string(), Some("1.0.0".to_string())),
                ("good2".to_string(), Some("1.0.0".to_string())),
                ("loose".to_string(), None),
            ]
        };
        let plan = gate.plan(lines());
        assert!(plan[0].needs_confirmation());
        assert_eq!(plan[0].trusted.as_deref(), Some("2.0.0"));
        assert!(plan[1].needs_confirmation());
        assert!(!plan[2].needs_confirmation());

        let mut plan = gate.plan(lines()).into_iter();
        let unconfirmed = gate.pin(plan.next().unwrap(), false);
        assert_eq!(unconfirmed.1.as_deref(), Some("2.0.0"));
        assert_eq!(unconfirmed.2, "good@2.0.0");
        let confirmed = gate.pin(gate.plan(lines()).remove(0), true);
        assert_eq!(confirmed.2, "good@1.0.0");
        // Without a trusted version the snapshot pin stays, and the trust checks hold it
        let untrusted = gate.pin(plan.next().unwrap(), false);
        assert_eq!(untrusted.2, "good2@1.0.0");
        assert_eq!(
            held(&gate.trust, "good2", Some("1.0.0")),
            vec![Reason::Unsigned]
        );
    }

    #[test]
    fn tether_checks_the_release_age_a_manager_cannot() {
        // npm 11.10 and later holds to the limit itself
        assert_eq!(age_check(false, false, None), AgeCheck::Met);
        // gem: an unpinned package installs the release resolved before the cutoff
        assert_eq!(age_check(true, true, None), AgeCheck::Met);
        assert_eq!(age_check(true, false, None), AgeCheck::Unchecked);
        // gem: a trusted record's version installs only once it is old enough
        assert_eq!(age_check(true, true, Some(Ok(true))), AgeCheck::Met);
        assert_eq!(age_check(true, true, Some(Ok(false))), AgeCheck::TooNew);
        assert_eq!(
            age_check(true, true, Some(Err(anyhow::anyhow!("offline")))),
            AgeCheck::Unchecked
        );
        let reasons = |age: AgeCheck| {
            inbox::reasons(Checks {
                from_this_machine: true,
                cooldown_unsupported: age == AgeCheck::Unchecked,
                too_new: age == AgeCheck::TooNew,
                ..Checks::default()
            })
        };
        assert!(reasons(AgeCheck::Met).is_empty());
        assert_eq!(reasons(AgeCheck::TooNew), [Reason::TooNew]);
        assert_eq!(reasons(AgeCheck::Unchecked), [Reason::CooldownUnsupported]);
    }

    #[test]
    fn casks_import_only_on_macos_and_when_enabled() {
        assert!(!import_casks(false));
        assert_eq!(import_casks(true), cfg!(target_os = "macos"));
    }

    #[test]
    fn failed_install_waits_a_day_unless_the_version_changes() {
        let now = chrono::Utc::now();
        let failure = InstallFailure {
            version: Some("1.0.0".to_string()),
            attempted: now,
            error: "boom".to_string(),
        };
        assert!(!failure.should_retry(Some("1.0.0"), now + chrono::Duration::hours(23)));
        assert!(failure.should_retry(Some("1.0.0"), now + chrono::Duration::hours(24)));
        assert!(failure.should_retry(Some("1.1.0"), now));
        assert!(failure.should_retry(None, now));
    }

    #[test]
    fn record_failure_keeps_the_newest_attempt_and_prune_settles_installed() {
        let mut failures = HashMap::new();
        record_failure(&mut failures, "brew_formulae", "mas", None, "macOS only");
        record_failure(&mut failures, "npm", "zx", Some("8.0.0"), "boom");
        assert!(!retry_due(&failures, "brew_formulae", "mas", None));
        assert!(retry_due(&failures, "npm", "zx", Some("8.1.0")));
        assert!(retry_due(&failures, "npm", "other", None));

        let mut machine = MachineState::new("me");
        machine
            .packages
            .insert("npm".to_string(), vec!["zx".to_string()]);
        prune_failures(&mut failures, &machine);
        assert_eq!(
            failures.keys().collect::<Vec<_>>(),
            [&InstallFailure::key("brew_formulae", "mas")]
        );
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
            install_failures: HashMap::new(),
            warned_signatures: Default::default(),
            profile_notice_shown: false,
            membership_error: None,
            config_export_hash: None,
            config_error: None,
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
            install_failures: HashMap::new(),
            warned_signatures: Default::default(),
            profile_notice_shown: false,
            membership_error: None,
            config_export_hash: None,
            config_error: None,
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

    #[test]
    fn the_excluded_packages_notice_shows_once() {
        let mut config = Config::default();
        config
            .machine_profiles
            .insert("server".to_string(), "server".to_string());
        let server = MachineState::new("server");
        let mut mac = MachineState::new("mac");
        mac.profile = Some("dev".to_string());
        let mut shown = false;

        // Nothing excluded yet, so the notice waits
        let scope = Membership::new(&config, &Default::default(), &server, &[(&mac, true)]);
        notify_excluded(&scope, &mut shown);
        assert!(!shown);

        mac.packages
            .insert("brew_casks".to_string(), vec!["zoom".to_string()]);
        let scope = Membership::new(&config, &Default::default(), &server, &[(&mac, true)]);
        assert_eq!(scope.excluded(), vec!["brew_casks:zoom"]);
        notify_excluded(&scope, &mut shown);
        assert!(shown);
    }

    #[test]
    fn casks_this_machine_skips_bring_no_taps() {
        let linux = MachineState::new("linux");
        let mut mac = MachineState::new("mac");
        mac.packages
            .insert("brew_casks".to_string(), vec!["evil/tap/app".to_string()]);
        let scope = Membership::new(
            &Config::default(),
            &Default::default(),
            &linux,
            &[(&mac, true)],
        );
        let brewfile = "tap \"evil/tap\"\ncask \"evil/tap/app\"\n";

        let mut packages = BrewfilePackages::parse(brewfile);
        let mut gated = Gated::default();
        select_brew(&mut packages, &linux, &scope, false, &mut gated);
        assert!(packages.taps.is_empty() && packages.casks.is_empty());
        assert!(gated.needed_taps.is_empty());
        assert_eq!(
            gated.passed,
            [("brew_casks".to_string(), "evil/tap/app".to_string())]
        );

        let mut packages = BrewfilePackages::parse(brewfile);
        let mut gated = Gated::default();
        select_brew(&mut packages, &linux, &scope, true, &mut gated);
        assert_eq!(packages.taps, ["evil/tap"]);
    }
}

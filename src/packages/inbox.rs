use super::{manager_for_key, osv, BrewManager, PackageInfo, PackageManager};
use crate::cli::Output;
use crate::sync::signing::{self, TrustStore};
use anyhow::{bail, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use ssh_key::PublicKey;
use std::path::{Path, PathBuf};

/// Why a synced package waits for the user instead of installing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Reason {
    /// It came from another machine, and no trusted machine record lists this exact version,
    /// or `packages.auto_install_from_trusted` is off.
    Unsigned,
    /// A machine record that lists it is validly signed, by a key this machine does not
    /// trust for that machine.
    UntrustedSigner,
    /// It is a tap outside the trusted taps, or a formula or cask from one.
    UntrustedTap,
    /// The installed manager cannot enforce `packages.min_release_age_days`.
    CooldownUnsupported,
    /// OSV lists a `MAL-` advisory for it. Approval cannot override this.
    Malicious,
    /// A machine signs its record with a key this machine has not trusted for it yet.
    NewMachine,
    /// A trusted machine signs its record with a different key. The old key stays trusted
    /// until the user approves the new one.
    KeyChanged,
}

impl Reason {
    pub fn label(self) -> &'static str {
        match self {
            Reason::Unsigned => "from another machine",
            Reason::UntrustedSigner => "signed by an untrusted key",
            Reason::UntrustedTap => "untrusted tap",
            Reason::CooldownUnsupported => "release age not checked",
            Reason::Malicious => "malicious (OSV)",
            Reason::NewMachine => "new machine key",
            Reason::KeyChanged => "machine key changed",
        }
    }
}

/// Manager value of a machine trust item, so its id is `machine:<machine id>`.
pub const MACHINE: &str = "machine";

/// What approving an item does.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "type")]
pub enum Kind {
    /// Install the package.
    #[default]
    Package,
    /// Trust this signing key for the machine named by the item's `name`.
    TrustMachine {
        /// OpenSSH public key line
        public_key: String,
        fingerprint: String,
    },
}

/// An item held for approval: a package, or a machine key to trust. For a package,
/// `source_machine` and `commit` identify the manifest change it came from.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InboxItem {
    #[serde(default)]
    pub kind: Kind,
    /// Machine-state key: npm, pnpm, bun, gem, uv, brew_formulae, brew_casks or brew_taps
    pub manager: String,
    pub name: String,
    #[serde(default)]
    pub version: Option<String>,
    /// Tap a Homebrew formula or cask installs from, when known
    #[serde(default)]
    pub tap: Option<String>,
    #[serde(default)]
    pub source_machine: Option<String>,
    #[serde(default)]
    pub commit: Option<String>,
    /// Fingerprint of the key that validly signed the source machine's record, trusted or not
    #[serde(default)]
    pub signer: Option<String>,
    pub reasons: Vec<Reason>,
    /// OSV advisory ids for this version, including non-blocking ones
    #[serde(default)]
    pub advisories: Vec<String>,
    pub first_seen: DateTime<Utc>,
}

impl InboxItem {
    /// A request to trust `key` as `machine_id`.
    pub fn machine(machine_id: &str, key: &PublicKey, reason: Reason) -> Result<Self> {
        Ok(Self {
            kind: Kind::TrustMachine {
                public_key: key.to_openssh()?,
                fingerprint: signing::fingerprint(key),
            },
            manager: MACHINE.to_string(),
            name: machine_id.to_string(),
            version: None,
            tap: None,
            source_machine: Some(machine_id.to_string()),
            commit: None,
            signer: None,
            reasons: vec![reason],
            advisories: Vec::new(),
            first_seen: Utc::now(),
        })
    }

    /// Stable id used by `tether packages approve|reject`.
    pub fn id(&self) -> String {
        format!("{}:{}", self.manager, self.name)
    }

    fn is(&self, manager: &str, name: &str) -> bool {
        self.manager == manager && self.name == name
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Decision {
    pub manager: String,
    pub name: String,
    #[serde(default)]
    pub version: Option<String>,
    pub at: DateTime<Utc>,
}

/// Local approval state in `~/.tether/inbox.json`. It is never synced, because approval
/// is a decision about this machine.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Inbox {
    #[serde(default)]
    pub items: Vec<InboxItem>,
    #[serde(default)]
    pub approved: Vec<Decision>,
    /// Rejections live here, not in the synced `removed_packages`, because every sync
    /// rebuilds that list from what is installed.
    #[serde(default)]
    pub rejected: Vec<Decision>,
}

impl Inbox {
    pub fn path() -> Result<PathBuf> {
        Ok(crate::home_dir()?.join(".tether").join("inbox.json"))
    }

    pub fn load() -> Result<Self> {
        let path = Self::path()?;
        if !path.exists() {
            return Ok(Self::default());
        }
        Ok(serde_json::from_str(&std::fs::read_to_string(path)?)?)
    }

    fn save(&self) -> Result<()> {
        crate::sync::atomic_write(
            &Self::path()?,
            serde_json::to_string_pretty(self)?.as_bytes(),
        )
    }

    /// Load, change and save under a lock, so the daemon and the CLI or TUI cannot
    /// overwrite each other's changes.
    pub fn update<T>(f: impl FnOnce(&mut Inbox) -> Result<T>) -> Result<T> {
        use fs2::FileExt;

        let lock_path = crate::home_dir()?.join(".tether").join("inbox.lock");
        std::fs::create_dir_all(lock_path.parent().unwrap())?;
        let lock = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(&lock_path)?;
        lock.lock_exclusive()?;
        let mut inbox = Self::load()?;
        let result = f(&mut inbox)?;
        inbox.save()?;
        Ok(result)
    }

    pub fn is_approved(&self, manager: &str, name: &str) -> bool {
        self.approved
            .iter()
            .any(|d| d.manager == manager && d.name == name)
    }

    pub fn is_rejected(&self, manager: &str, name: &str) -> bool {
        self.rejected
            .iter()
            .any(|d| d.manager == manager && d.name == name)
    }

    pub fn is_pending(&self, manager: &str, name: &str) -> bool {
        self.items.iter().any(|i| i.is(manager, name))
    }

    /// Taps the user approved, which count as trusted like `packages.brew.trusted_taps`.
    pub fn approved_taps(&self) -> impl Iterator<Item = &str> {
        self.approved
            .iter()
            .filter(|d| d.manager == "brew_taps")
            .map(|d| d.name.as_str())
    }

    /// Queue an item. Returns false when it is already pending or was rejected.
    /// A pending item takes the newer check results, so a cleared OSV report can be approved.
    pub fn add(&mut self, item: InboxItem) -> bool {
        if self.is_rejected(&item.manager, &item.name) {
            return false;
        }
        if let Some(pending) = self
            .items
            .iter_mut()
            .find(|i| i.is(&item.manager, &item.name))
        {
            *pending = InboxItem {
                first_seen: pending.first_seen,
                ..item
            };
            return false;
        }
        self.items.push(item);
        true
    }

    /// Find a pending item by id (`manager:name`) or by a name only one item has.
    pub fn find(&self, query: &str) -> Result<&InboxItem> {
        if let Some(item) = self.items.iter().find(|i| i.id() == query) {
            return Ok(item);
        }
        let mut matches = self.items.iter().filter(|i| i.name == query);
        match (matches.next(), matches.next()) {
            (Some(item), None) => Ok(item),
            (Some(_), Some(_)) => bail!("{} matches more than one item. Use manager:name", query),
            (None, _) => bail!("No inbox item {}", query),
        }
    }

    fn take(&mut self, query: &str) -> Result<InboxItem> {
        let id = self.find(query)?.id();
        let pos = self.items.iter().position(|i| i.id() == id).unwrap();
        Ok(self.items.remove(pos))
    }

    /// Approve a pending item. A Homebrew item from an untrusted tap also approves the tap,
    /// because brew cannot install it otherwise. A machine item is only removed: the
    /// trust store, not this list, records trusted keys.
    pub fn approve(&mut self, query: &str) -> Result<InboxItem> {
        let item = self.find(query)?;
        if item.reasons.contains(&Reason::Malicious) {
            bail!(
                "OSV lists {} as malicious ({}). Tether will not install it",
                item.name,
                item.advisories.join(", ")
            );
        }
        let item = self.take(query)?;
        if item.kind != Kind::Package {
            return Ok(item);
        }
        let now = Utc::now();
        self.approved.push(Decision {
            manager: item.manager.clone(),
            name: item.name.clone(),
            version: item.version.clone(),
            at: now,
        });
        if let Some(tap) = &item.tap {
            if !self.approved_taps().any(|t| t == tap) {
                self.approved.push(Decision {
                    manager: "brew_taps".to_string(),
                    name: tap.clone(),
                    version: None,
                    at: now,
                });
            }
        }
        Ok(item)
    }

    /// Reject a pending item so later syncs do not queue it again.
    pub fn reject(&mut self, query: &str) -> Result<InboxItem> {
        let item = self.take(query)?;
        self.rejected.push(Decision {
            manager: item.manager.clone(),
            name: item.name.clone(),
            version: item.version.clone(),
            at: Utc::now(),
        });
        Ok(item)
    }

    /// Queue a trust item for each machine whose record is signed by a key `store` does not
    /// trust for that machine. Trust never moves on its own: a renamed machine is a new item.
    /// Returns the newly queued items.
    pub fn hold_untrusted_keys(
        &mut self,
        store: &TrustStore,
        signers: Vec<(String, PublicKey)>,
        this_machine: &str,
    ) -> Result<Vec<InboxItem>> {
        let mut queued = Vec::new();
        for (id, key) in signers {
            if id == this_machine || store.trusts(&id, &key) {
                continue;
            }
            let old = store.key_for(&id);
            let reason = match old {
                Some(_) => Reason::KeyChanged,
                None => Reason::NewMachine,
            };
            let item = InboxItem::machine(&id, &key, reason)?;
            if !self.add(item.clone()) {
                continue;
            }
            if let Some(old) = old {
                Output::warning(&format!(
                    "SIGNING KEY CHANGED for machine {}: {} is now {}. Its record is not trusted \
                     until you approve the new key. If you did not set that machine up again, \
                     someone may be signing as it",
                    id,
                    signing::fingerprint(old),
                    signing::fingerprint(&key),
                ));
            }
            queued.push(item);
        }
        Ok(queued)
    }

    /// Drop pending items for packages that are now installed by other means. Malicious
    /// items stay, because a held upgrade is for a package that is already installed.
    pub fn prune_installed(&mut self, manager: &str, installed: impl Fn(&str) -> bool) {
        self.items.retain(|i| {
            i.manager != manager || i.reasons.contains(&Reason::Malicious) || !installed(&i.name)
        });
    }
}

/// The best signature among the machine records that list a package at its version.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Signer {
    /// No listing record is validly signed
    #[default]
    None,
    /// A listing record is validly signed, by a key not trusted for its machine
    Untrusted,
    /// A listing record is signed by the key trusted for its machine
    Trusted,
}

/// What the trust checks found for one synced package.
#[derive(Debug, Clone, Copy, Default)]
pub struct Checks {
    /// This machine's own record, signed by this machine, lists the package at its version
    pub from_this_machine: bool,
    pub approved: bool,
    pub auto_install_from_trusted: bool,
    pub signer: Signer,
    pub cooldown_unsupported: bool,
    pub untrusted_tap: bool,
    pub malicious: bool,
}

/// Reasons to hold a package back. An empty list means it may install.
pub fn reasons(checks: Checks) -> Vec<Reason> {
    let mut reasons = Vec::new();
    if checks.malicious {
        reasons.push(Reason::Malicious);
    }
    if checks.approved {
        return reasons;
    }
    if checks.untrusted_tap {
        reasons.push(Reason::UntrustedTap);
    }
    if checks.cooldown_unsupported {
        reasons.push(Reason::CooldownUnsupported);
    }
    // Another machine's package installs on its own only when a trusted machine record lists it
    if !checks.from_this_machine {
        match checks.signer {
            Signer::Trusted if checks.auto_install_from_trusted => {}
            Signer::Untrusted => reasons.push(Reason::UntrustedSigner),
            _ => reasons.push(Reason::Unsigned),
        }
    }
    reasons
}

/// Pending items, oldest first.
pub fn list() -> Result<Vec<InboxItem>> {
    Ok(Inbox::load()?.items)
}

/// Queue items and return the ones that were not already pending or rejected.
pub fn add(items: Vec<InboxItem>) -> Result<Vec<InboxItem>> {
    if items.is_empty() {
        return Ok(Vec::new());
    }
    Inbox::update(|inbox| {
        Ok(items
            .into_iter()
            .filter(|item| inbox.add(item.clone()))
            .collect())
    })
}

/// Record approval. A machine item trusts its key now. For a package, the caller installs
/// it with [`install`] or leaves it to the next sync.
pub fn approve(query: &str) -> Result<InboxItem> {
    Inbox::update(|inbox| {
        let item = inbox.approve(query)?;
        if let Kind::TrustMachine { public_key, .. } = &item.kind {
            let mut store = TrustStore::load()?;
            store.trust(&item.name, &PublicKey::from_openssh(public_key)?)?;
            store.save()?;
        }
        Ok(item)
    })
}

pub fn reject(query: &str) -> Result<InboxItem> {
    Inbox::update(|inbox| inbox.reject(query))
}

/// A machine key in this machine's trust store.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrustedMachine {
    pub machine_id: String,
    /// `SHA256:...`, as `ssh-keygen -l` prints it
    pub fingerprint: String,
}

pub fn trusted_machines() -> Result<Vec<TrustedMachine>> {
    Ok(TrustStore::load()?
        .keys
        .iter()
        .map(|(id, key)| TrustedMachine {
            machine_id: id.clone(),
            fingerprint: signing::fingerprint(key),
        })
        .collect())
}

/// Trust the key that signed `machine_id`'s record in the sync repo, and settle its inbox item.
pub fn trust_machine(sync_path: &Path, machine_id: &str) -> Result<TrustedMachine> {
    let Some((_, key)) = signing::record_signers(sync_path)
        .into_iter()
        .find(|(id, _)| id == machine_id)
    else {
        bail!("Machine {} has no signed machine record", machine_id);
    };
    Inbox::update(|inbox| {
        let mut store = TrustStore::load()?;
        store.trust(machine_id, &key)?;
        store.save()?;
        inbox.items.retain(|i| !i.is(MACHINE, machine_id));
        inbox
            .rejected
            .retain(|d| d.manager != MACHINE || d.name != machine_id);
        Ok(TrustedMachine {
            machine_id: machine_id.to_string(),
            fingerprint: signing::fingerprint(&key),
        })
    })
}

/// Remove a machine's key from the trust store. Returns false when it was not trusted.
/// Its record's key returns to the inbox on the next sync.
pub fn untrust_machine(machine_id: &str) -> Result<bool> {
    Inbox::update(|_| {
        let mut store = TrustStore::load()?;
        let removed = store.untrust(machine_id);
        store.save()?;
        Ok(removed)
    })
}

/// Queue every key that signs a machine record but is not trusted for that machine.
/// Returns the newly queued items.
pub fn queue_machine_keys(sync_path: &Path, this_machine: &str) -> Result<Vec<InboxItem>> {
    let signers = signing::record_signers(sync_path);
    Inbox::update(|inbox| inbox.hold_untrusted_keys(&TrustStore::load()?, signers, this_machine))
}

/// Names whose upgrade target OSV lists as malicious. They go to the inbox, and
/// `update_all` leaves them at the installed version. Like a failed OSV request, a manager
/// that cannot list its upgrade targets does not stop upgrades.
pub async fn hold_malicious_upgrades(manager: &dyn PackageManager) -> Vec<String> {
    if osv::ecosystem_name(manager.ecosystem()).is_none() {
        return Vec::new();
    }
    let candidates = match manager.upgrade_candidates().await {
        Ok(candidates) => candidates,
        Err(e) => {
            eprintln!(
                "Warning: {} upgrades not checked against OSV: {}",
                manager.name(),
                e
            );
            return Vec::new();
        }
    };
    let pins: Vec<(String, Option<String>)> = candidates
        .into_iter()
        .map(|(name, version)| (name, Some(version)))
        .collect();
    let found = osv::advisories(manager.ecosystem(), &pins).await;
    let mut held = Vec::new();
    let mut items = Vec::new();
    for ((name, version), advisories) in pins.into_iter().zip(found) {
        if !advisories.iter().any(|id| osv::is_malicious(id)) {
            continue;
        }
        eprintln!(
            "Warning: Skipping {} upgrade of {} to {}: OSV lists it as malicious ({})",
            manager.name(),
            name,
            version.as_deref().unwrap_or_default(),
            advisories.join(", ")
        );
        held.push(name.clone());
        items.push(InboxItem {
            kind: Kind::Package,
            manager: manager.name().to_string(),
            name,
            version,
            tap: None,
            source_machine: None,
            commit: None,
            signer: None,
            reasons: vec![Reason::Malicious],
            advisories,
            first_seen: Utc::now(),
        });
    }
    if let Err(e) = add(items) {
        eprintln!("Warning: Could not hold malicious upgrades: {}", e);
    }
    held
}

/// Install an approved item now. `interactive` lets a cask prompt for a password.
/// A machine item has nothing to install.
/// OSV is asked again, because an advisory can appear after the item was queued.
pub async fn install(item: &InboxItem, interactive: bool) -> Result<()> {
    if item.kind != Kind::Package {
        return Ok(());
    }
    let package = PackageInfo {
        name: item.name.clone(),
        version: item.version.clone(),
    };
    if let Some(manager) = manager_for_key(&item.manager) {
        let pin = [(item.name.clone(), item.version.clone())];
        let found = osv::advisories(manager.ecosystem(), &pin).await;
        let malicious: Vec<&String> = found[0].iter().filter(|id| osv::is_malicious(id)).collect();
        if !malicious.is_empty() {
            bail!(
                "OSV lists {} as malicious ({}). Tether will not install it",
                item.name,
                malicious
                    .iter()
                    .map(|s| s.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            );
        }
    }
    match item.manager.as_str() {
        "brew_taps" => BrewManager::new().tap(&item.name).await,
        "brew_formulae" => BrewManager::new().install(&package).await,
        "brew_casks" => {
            if BrewManager::new()
                .install_cask(&item.name, interactive)
                .await?
            {
                Ok(())
            } else {
                bail!(
                    "{} needs a password. Run `tether sync` in a terminal",
                    item.name
                )
            }
        }
        key => match manager_for_key(key) {
            Some(manager) => manager.install(&package).await,
            None => bail!("Unknown package manager {}", key),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(manager: &str, name: &str) -> InboxItem {
        InboxItem {
            kind: Kind::Package,
            manager: manager.to_string(),
            name: name.to_string(),
            version: Some("1.0.0".to_string()),
            tap: None,
            source_machine: Some("other".to_string()),
            commit: Some("abc123".to_string()),
            signer: None,
            reasons: vec![Reason::Unsigned],
            advisories: Vec::new(),
            first_seen: Utc::now(),
        }
    }

    #[test]
    fn add_skips_pending_and_rejected() {
        let mut inbox = Inbox::default();
        assert!(inbox.add(item("npm", "left-pad")));
        let mut cleared = item("npm", "left-pad");
        cleared.reasons = vec![Reason::CooldownUnsupported];
        assert!(!inbox.add(cleared));
        assert_eq!(inbox.items[0].reasons, vec![Reason::CooldownUnsupported]);
        assert!(inbox.add(item("pnpm", "left-pad")));
        inbox.reject("npm:left-pad").unwrap();
        assert!(inbox.is_rejected("npm", "left-pad"));
        assert!(!inbox.add(item("npm", "left-pad")));
    }

    #[test]
    fn find_by_id_or_unique_name() {
        let mut inbox = Inbox::default();
        inbox.add(item("npm", "a"));
        inbox.add(item("npm", "b"));
        inbox.add(item("pnpm", "b"));
        assert_eq!(inbox.find("a").unwrap().id(), "npm:a");
        assert_eq!(inbox.find("pnpm:b").unwrap().id(), "pnpm:b");
        assert!(inbox.find("b").is_err());
        assert!(inbox.find("c").is_err());
    }

    #[test]
    fn approve_moves_item_and_trusts_its_tap() {
        let mut inbox = Inbox::default();
        let mut formula = item("brew_formulae", "bd");
        formula.tap = Some("gastownhall/beads".to_string());
        inbox.add(formula);
        let approved = inbox.approve("bd").unwrap();
        assert_eq!(approved.name, "bd");
        assert!(inbox.items.is_empty());
        assert!(inbox.is_approved("brew_formulae", "bd"));
        assert_eq!(
            inbox.approved_taps().collect::<Vec<_>>(),
            vec!["gastownhall/beads"]
        );
    }

    #[test]
    fn approve_refuses_malicious() {
        let mut inbox = Inbox::default();
        let mut bad = item("npm", "nx");
        bad.reasons = vec![Reason::Malicious];
        bad.advisories = vec!["MAL-2025-41443".to_string()];
        inbox.add(bad);
        assert!(inbox.approve("nx").is_err());
        assert!(inbox.is_pending("npm", "nx"));
    }

    #[test]
    fn prune_drops_installed_items_for_that_manager() {
        let mut inbox = Inbox::default();
        inbox.add(item("npm", "a"));
        inbox.add(item("pnpm", "a"));
        inbox.add(InboxItem {
            reasons: vec![Reason::Malicious],
            ..item("npm", "b")
        });
        inbox.prune_installed("npm", |name| name == "a" || name == "b");
        let left: Vec<String> = inbox.items.iter().map(|i| i.id()).collect();
        assert_eq!(left, vec!["pnpm:a", "npm:b"]);
    }

    #[test]
    fn reasons_follow_the_trust_model() {
        let other = Checks {
            auto_install_from_trusted: true,
            ..Checks::default()
        };
        assert_eq!(reasons(other), vec![Reason::Unsigned]);
        let trusted = Checks {
            signer: Signer::Trusted,
            ..other
        };
        assert!(reasons(trusted).is_empty());
        assert_eq!(
            reasons(Checks {
                auto_install_from_trusted: false,
                ..trusted
            }),
            vec![Reason::Unsigned]
        );
        assert_eq!(
            reasons(Checks {
                signer: Signer::Untrusted,
                ..other
            }),
            vec![Reason::UntrustedSigner]
        );
        assert!(reasons(Checks {
            from_this_machine: true,
            ..other
        })
        .is_empty());
        assert_eq!(
            reasons(Checks {
                cooldown_unsupported: true,
                untrusted_tap: true,
                ..trusted
            }),
            vec![Reason::UntrustedTap, Reason::CooldownUnsupported]
        );
        assert!(reasons(Checks {
            approved: true,
            cooldown_unsupported: true,
            ..other
        })
        .is_empty());
        assert_eq!(
            reasons(Checks {
                approved: true,
                malicious: true,
                ..other
            }),
            vec![Reason::Malicious]
        );
    }

    fn key() -> PublicKey {
        ssh_key::PrivateKey::random(&mut ssh_key::rand_core::OsRng, ssh_key::Algorithm::Ed25519)
            .unwrap()
            .public_key()
            .clone()
    }

    #[test]
    fn new_machine_keys_wait_for_trust() {
        let (own, b, c) = (key(), key(), key());
        let mut store = TrustStore::default();
        store.trust("me", &own).unwrap();
        store.trust("b", &b).unwrap();
        let mut inbox = Inbox::default();
        let signers = vec![
            ("me".to_string(), own.clone()),
            ("b".to_string(), b.clone()),
            ("c".to_string(), c.clone()),
        ];
        let queued = inbox
            .hold_untrusted_keys(&store, signers.clone(), "me")
            .unwrap();
        assert_eq!(queued.len(), 1);
        assert_eq!(queued[0].id(), "machine:c");
        assert_eq!(queued[0].reasons, vec![Reason::NewMachine]);
        assert!(inbox
            .hold_untrusted_keys(&store, signers, "me")
            .unwrap()
            .is_empty());

        let approved = inbox.approve("machine:c").unwrap();
        assert!(inbox.items.is_empty());
        assert!(inbox.approved.is_empty());
        let Kind::TrustMachine { fingerprint, .. } = approved.kind else {
            panic!("not a machine item");
        };
        assert_eq!(fingerprint, signing::fingerprint(&c));
    }

    #[test]
    fn changed_machine_key_waits_without_untrusting() {
        let (b, b2) = (key(), key());
        let mut store = TrustStore::default();
        store.trust("b", &b).unwrap();
        let before = store.clone();
        let mut inbox = Inbox::default();
        let queued = inbox
            .hold_untrusted_keys(&store, vec![("b".to_string(), b2.clone())], "me")
            .unwrap();
        assert_eq!(store, before);
        assert_eq!(queued[0].reasons, vec![Reason::KeyChanged]);
        inbox
            .hold_untrusted_keys(&store, vec![("b".to_string(), b2.clone())], "me")
            .unwrap();
        assert_eq!(inbox.items.len(), 1);
        assert_eq!(inbox.items[0].reasons, vec![Reason::KeyChanged]);
    }

    #[test]
    fn renamed_machine_needs_new_trust() {
        let b = key();
        let mut store = TrustStore::default();
        store.trust("b", &b).unwrap();
        let mut inbox = Inbox::default();
        let queued = inbox
            .hold_untrusted_keys(&store, vec![("b-new".to_string(), b.clone())], "me")
            .unwrap();
        assert_eq!(queued[0].id(), "machine:b-new");
        assert_eq!(queued[0].reasons, vec![Reason::NewMachine]);
        assert_eq!(store.machine_for(&b), Some("b"));
        assert!(store.key_for("b-new").is_none());
    }

    #[test]
    fn inbox_roundtrips_through_json() {
        let mut inbox = Inbox::default();
        inbox.add(item("uv", "ruff"));
        inbox.approve("ruff").unwrap();
        inbox.add(item("gem", "rails"));
        let json = serde_json::to_string(&inbox).unwrap();
        assert_eq!(serde_json::from_str::<Inbox>(&json).unwrap(), inbox);
        assert!(json.contains("\"unsigned\""));
    }
}

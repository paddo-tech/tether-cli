use super::{manager_for_key, osv, resolve, BrewManager, PackageInfo, PackageManager, Upgrade};
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
    /// The installed manager cannot enforce `packages.min_release_age_days`, and Tether could
    /// not check the release age in the registry either.
    CooldownUnsupported,
    /// The installed manager cannot enforce `packages.min_release_age_days`, and the registry
    /// shows that the pinned release is newer than the limit. The hold ends when it is old
    /// enough.
    TooNew,
    /// OSV lists a `MAL-` advisory for it. Approval cannot override this.
    Malicious,
    /// OSV lists a `MAL-` advisory for the version an upgrade would install. Approval cannot
    /// override this. The hold covers only that version, and ends when that version is no
    /// longer the upgrade target.
    MaliciousUpgrade,
    /// OSV lists a `MAL-` advisory for some release of it, and Tether could not find the
    /// release an unpinned install would pick. Approval overrides this, after a warning.
    MaliciousUnresolved,
    /// A machine signs its record with a key this machine has not trusted for it yet.
    NewMachine,
    /// A trusted machine signs its record with a different key. The old key stays trusted
    /// until the user approves the new one.
    KeyChanged,
    /// The machine it came from is trusted, but its record fails its signature, so someone
    /// may have edited the record in the repo.
    SignatureFailed,
    /// The version that only machines on another OS list failed to install here. The item
    /// holds the release that suits this machine, which no trusted record lists.
    OtherOsVersion,
    /// A trusted record pins a release newer than `packages.min_release_age_days`, so it
    /// failed to install here. The item holds the newest release older than the limit, which
    /// no trusted record lists.
    PinnedTooNew,
}

impl Reason {
    /// The one name of the reason, in the CLI and in the dashboard badges. Short, so a badge
    /// fits in a list row; the dashboard details explain each one.
    pub fn label(self) -> &'static str {
        match self {
            Reason::Unsigned => "from another machine",
            Reason::UntrustedSigner => "untrusted key",
            Reason::UntrustedTap => "untrusted tap",
            Reason::CooldownUnsupported => "age not checked",
            Reason::TooNew => "too new",
            Reason::Malicious => "malicious",
            Reason::MaliciousUpgrade => "malicious upgrade",
            Reason::MaliciousUnresolved => "malicious releases",
            Reason::NewMachine => "new machine key",
            Reason::KeyChanged => "key changed",
            Reason::SignatureFailed => "signature failed",
            Reason::OtherOsVersion => "other OS version",
            Reason::PinnedTooNew => "pinned too new",
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

    /// OSV lists the held version as malicious, so approval cannot install it.
    pub fn malicious(&self) -> bool {
        self.reasons
            .iter()
            .any(|r| matches!(r, Reason::Malicious | Reason::MaliciousUpgrade))
    }

    /// The source machine is trusted, but its record fails its signature.
    pub fn signature_failed(&self) -> bool {
        self.reasons.contains(&Reason::SignatureFailed)
    }

    /// Whether one answer for many items may approve it. A machine key, a malicious
    /// package, a package from a record that fails its signature and a package without a
    /// binding each need their own.
    pub fn bulk_approvable(&self) -> bool {
        self.kind == Kind::Package
            && !self.malicious()
            && !self.signature_failed()
            && self.binding().is_some()
    }

    /// What a sync can replace under the item's id and a review must name: the key
    /// fingerprint, the version, the tap of a formula or cask, or a tap's own name. None for
    /// a package without a version or tap, such as one from a 1.x record.
    pub fn binding(&self) -> Option<&str> {
        match &self.kind {
            Kind::TrustMachine { fingerprint, .. } => Some(fingerprint),
            Kind::Package => self
                .version
                .as_deref()
                .or(self.tap.as_deref())
                .or((self.manager == "brew_taps").then_some(self.name.as_str())),
        }
    }

    /// The machine an item comes from: the machine a key belongs to, or the machine whose
    /// record lists a package. None when no other machine's record lists it.
    pub fn from_machine(&self) -> Option<&str> {
        match self.kind {
            Kind::TrustMachine { .. } => Some(&self.name),
            Kind::Package => self.source_machine.as_deref(),
        }
    }
}

/// Items from one machine, held for the same reasons. A new machine can bring hundreds of
/// packages, and a few groups are easier to review than each item.
#[derive(Debug, Clone, PartialEq)]
pub struct Group {
    pub machine: Option<String>,
    pub reasons: Vec<Reason>,
    /// Indices into the items, in their order
    pub items: Vec<usize>,
}

/// Order items so each group's items are next to each other, keeping their order within a
/// group.
pub fn sort_by_group(items: &mut [InboxItem]) {
    items.sort_by(|a, b| {
        a.from_machine()
            .cmp(&b.from_machine())
            .then_with(|| reason_labels(a).cmp(&reason_labels(b)))
    });
}

fn reason_labels(item: &InboxItem) -> Vec<&'static str> {
    item.reasons.iter().map(|r| r.label()).collect()
}

/// The groups of `items`, in the order their first items appear.
pub fn groups(items: &[InboxItem]) -> Vec<Group> {
    let mut groups: Vec<Group> = Vec::new();
    for (i, item) in items.iter().enumerate() {
        let machine = item.from_machine().map(str::to_string);
        match groups
            .iter_mut()
            .find(|g| g.machine == machine && g.reasons == item.reasons)
        {
            Some(group) => group.items.push(i),
            None => groups.push(Group {
                machine,
                reasons: item.reasons.clone(),
                items: vec![i],
            }),
        }
    }
    groups
}

/// What "approve all from `machine`" covers: its items that one answer may approve, and the
/// number of its packages that stay held, as malicious or from a failed signature.
pub fn approvable_from(items: &[InboxItem], machine: &str) -> (Vec<InboxItem>, usize) {
    let packages = items
        .iter()
        .filter(|i| i.kind == Kind::Package && i.from_machine() == Some(machine));
    let held = packages.clone().filter(|i| !i.bulk_approvable()).count();
    (
        packages.filter(|i| i.bulk_approvable()).cloned().collect(),
        held,
    )
}

impl InboxItem {
    /// Fingerprint of the key a machine item asks to trust.
    fn fingerprint(&self) -> Option<&str> {
        match &self.kind {
            Kind::TrustMachine { fingerprint, .. } => Some(fingerprint),
            Kind::Package => None,
        }
    }

    /// Whether approving `self` grants what approving `other` would: the same key for a
    /// machine, the same version and tap for a package, held for the same reasons.
    fn same_request(&self, other: &InboxItem) -> bool {
        self.id() == other.id()
            && self.kind == other.kind
            && self.version == other.version
            && self.tap == other.tap
            && self.reasons == other.reasons
    }
}

/// An approval or rejection. It covers only this version, for a Homebrew formula or cask
/// only this tap, and for a machine only this key: a different one is a new decision.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Decision {
    pub manager: String,
    pub name: String,
    #[serde(default)]
    pub version: Option<String>,
    #[serde(default)]
    pub tap: Option<String>,
    /// Key fingerprint of a rejected machine item
    #[serde(default)]
    pub fingerprint: Option<String>,
    pub at: DateTime<Utc>,
}

impl Decision {
    fn of(item: &InboxItem) -> Self {
        Self {
            manager: item.manager.clone(),
            name: item.name.clone(),
            version: item.version.clone(),
            tap: item.tap.clone(),
            fingerprint: item.fingerprint().map(str::to_string),
            at: Utc::now(),
        }
    }

    fn covers(
        &self,
        manager: &str,
        name: &str,
        version: Option<&str>,
        tap: Option<&str>,
        fingerprint: Option<&str>,
    ) -> bool {
        self.manager == manager
            && self.name == name
            && self.version.as_deref() == version
            && self.tap.as_deref() == tap
            && self.fingerprint.as_deref() == fingerprint
    }
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

    pub fn is_approved(
        &self,
        manager: &str,
        name: &str,
        version: Option<&str>,
        tap: Option<&str>,
    ) -> bool {
        self.approved
            .iter()
            .any(|d| d.covers(manager, name, version, tap, None))
    }

    /// Whether the user rejected this version and tap of a package.
    pub fn is_rejected(
        &self,
        manager: &str,
        name: &str,
        version: Option<&str>,
        tap: Option<&str>,
    ) -> bool {
        self.rejected
            .iter()
            .any(|d| d.covers(manager, name, version, tap, None))
    }

    /// Whether the user rejected exactly what `item` asks for: its version and tap, or its key.
    fn rejects(&self, item: &InboxItem) -> bool {
        self.rejected.iter().any(|d| {
            d.covers(
                &item.manager,
                &item.name,
                item.version.as_deref(),
                item.tap.as_deref(),
                item.fingerprint(),
            )
        })
    }

    pub fn is_pending(&self, manager: &str, name: &str) -> bool {
        self.items.iter().any(|i| i.is(manager, name))
    }

    /// Whether this package waits here as malicious. A held upgrade blocks only its own
    /// version, or any install whose version is unknown.
    pub fn holds_malicious(&self, manager: &str, name: &str, version: Option<&str>) -> bool {
        self.items.iter().any(|i| {
            i.is(manager, name)
                && (i.reasons.contains(&Reason::Malicious)
                    || (i.reasons.contains(&Reason::MaliciousUpgrade)
                        && (version.is_none() || version == i.version.as_deref())))
        })
    }

    /// Drop this manager's held upgrades whose version is no longer the upgrade target, for
    /// example because a later release replaced it or the package moved past it. `targets`
    /// is every outdated package with the version an upgrade would install.
    pub fn clear_upgrade_holds(&mut self, manager: &str, targets: &[(String, String)]) {
        self.items.retain(|i| {
            i.manager != manager
                || !i.reasons.contains(&Reason::MaliciousUpgrade)
                || targets
                    .iter()
                    .any(|(name, version)| *name == i.name && i.version.as_ref() == Some(version))
        });
    }

    /// Taps the user approved as tap items, which count as trusted like
    /// `packages.brew.trusted_taps`.
    pub fn approved_taps(&self) -> impl Iterator<Item = &str> {
        self.approved
            .iter()
            .filter(|d| d.manager == "brew_taps")
            .map(|d| d.name.as_str())
    }

    /// Homebrew formulae and casks the user approved from a tap that is not trusted, as
    /// manager key, name and tap.
    pub fn approved_from_taps(&self) -> impl Iterator<Item = (&str, &str, &str)> {
        self.approved
            .iter()
            .filter_map(|d| Some((d.manager.as_str(), d.name.as_str(), d.tap.as_deref()?)))
    }

    /// Queue an item. Returns true when it is new, or replaces a pending item that asks for
    /// something else, such as another version or key: that is a new decision, so it is
    /// reported again. A pending item takes the newer check results, so a cleared OSV report
    /// can be approved.
    pub fn add(&mut self, item: InboxItem) -> bool {
        if self.rejects(&item) {
            return false;
        }
        if let Some(pending) = self
            .items
            .iter_mut()
            .find(|i| i.is(&item.manager, &item.name))
        {
            let changed = !pending.same_request(&item);
            *pending = InboxItem {
                first_seen: if changed {
                    item.first_seen
                } else {
                    pending.first_seen
                },
                ..item
            };
            return changed;
        }
        self.items.push(item);
        true
    }

    /// Apply one sync's checks: drop the pending items for packages that now pass or that no
    /// machine record lists any more, and queue the held ones. `listed` tells whether some
    /// record lists a package. Returns the held items that are new or changed.
    pub fn settle(
        &mut self,
        held: Vec<InboxItem>,
        passed: &[(String, String)],
        listed: impl Fn(&str, &str) -> bool,
    ) -> Vec<InboxItem> {
        self.items.retain(|i| {
            !passed.iter().any(|(manager, name)| i.is(manager, name))
                && (i.kind != Kind::Package || listed(&i.manager, &i.name))
        });
        held.into_iter()
            .filter(|item| self.add(item.clone()))
            .collect()
    }

    /// Find a pending item by id (`manager:name`) or by a name only one item has.
    pub fn find(&self, query: &str) -> Result<&InboxItem> {
        let query = super::normalize_id(query);
        let query = query.as_str();
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

    /// Approve the pending item the user reviewed. A sync can replace an item under the same
    /// id, for example with another key or version, so an item that no longer matches
    /// `reviewed` is refused. Approving a Homebrew formula or cask does not trust its tap.
    /// A machine item is only removed: the trust store, not this list, records trusted keys.
    pub fn approve(&mut self, reviewed: &InboxItem) -> Result<InboxItem> {
        let id = reviewed.id();
        let item = self.reviewed(reviewed)?;
        if item.malicious() {
            bail!(
                "OSV lists {} as malicious ({}). Tether will not install it",
                item.name,
                item.advisories.join(", ")
            );
        }
        let item = self.take(&id)?;
        if item.kind != Kind::Package {
            return Ok(item);
        }
        self.approved.push(Decision::of(&item));
        Ok(item)
    }

    /// The pending item under `reviewed`'s id, if a sync has not replaced it since review.
    fn reviewed(&self, reviewed: &InboxItem) -> Result<&InboxItem> {
        let id = reviewed.id();
        let Some(item) = self.items.iter().find(|i| i.id() == id) else {
            bail!("No inbox item {}", id);
        };
        if !item.same_request(reviewed) {
            bail!("{} changed since you reviewed it. Review it again", id);
        }
        Ok(item)
    }

    /// Reject the pending item the user reviewed, refused like [`Inbox::approve`] when a
    /// sync replaced it. The rejection covers only that version and tap, or that key, so a
    /// different one is queued again as a new item.
    pub fn reject(&mut self, reviewed: &InboxItem) -> Result<InboxItem> {
        let id = self.reviewed(reviewed)?.id();
        let item = self.take(&id)?;
        self.rejected.push(Decision::of(&item));
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
                     someone may be signing as it. Run 'tether machines show {}' on that \
                     machine and compare the fingerprint",
                    id,
                    signing::fingerprint(old),
                    signing::fingerprint(&key),
                    id,
                ));
            }
            queued.push(item);
        }
        Ok(queued)
    }

    /// Drop pending items for packages that are now installed by other means. Malicious
    /// items stay, because a held upgrade is for a package that is already installed.
    pub fn prune_installed(&mut self, manager: &str, installed: impl Fn(&str) -> bool) {
        self.items
            .retain(|i| i.manager != manager || i.malicious() || !installed(&i.name));
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
    /// The source machine is trusted, but its record fails its signature
    pub signature_failed: bool,
    pub cooldown_unsupported: bool,
    pub too_new: bool,
    pub untrusted_tap: bool,
    pub malicious: bool,
    pub malicious_unresolved: bool,
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
    if checks.malicious_unresolved {
        reasons.push(Reason::MaliciousUnresolved);
    }
    if checks.untrusted_tap {
        reasons.push(Reason::UntrustedTap);
    }
    if checks.cooldown_unsupported {
        reasons.push(Reason::CooldownUnsupported);
    }
    if checks.too_new {
        reasons.push(Reason::TooNew);
    }
    // Another machine's package installs on its own only when a trusted machine record lists it
    if !checks.from_this_machine {
        match checks.signer {
            Signer::Trusted if checks.auto_install_from_trusted => {}
            Signer::None | Signer::Untrusted if checks.signature_failed => {
                reasons.push(Reason::SignatureFailed)
            }
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

/// See [`Inbox::settle`].
pub fn settle(
    held: Vec<InboxItem>,
    passed: &[(String, String)],
    listed: impl Fn(&str, &str) -> bool,
) -> Result<Vec<InboxItem>> {
    let unlisted = |inbox: &Inbox| {
        inbox
            .items
            .iter()
            .any(|i| i.kind == Kind::Package && !listed(&i.manager, &i.name))
    };
    if held.is_empty() && passed.is_empty() && !unlisted(&Inbox::load()?) {
        return Ok(Vec::new());
    }
    Inbox::update(|inbox| Ok(inbox.settle(held, passed, &listed)))
}

/// Record approval of the item the user reviewed, as shown to them. A machine item trusts
/// its key now. For a package, the caller installs it with [`install`] or leaves it to the
/// next sync.
pub fn approve(reviewed: &InboxItem) -> Result<InboxItem> {
    Inbox::update(|inbox| {
        let item = inbox.approve(reviewed)?;
        if let Kind::TrustMachine { public_key, .. } = &item.kind {
            let mut store = TrustStore::load()?;
            store.trust(&item.name, &PublicKey::from_openssh(public_key)?)?;
            store.save()?;
        }
        Ok(item)
    })
}

/// Record rejection of the item the user reviewed, as shown to them.
pub fn reject(reviewed: &InboxItem) -> Result<InboxItem> {
    Inbox::update(|inbox| inbox.reject(reviewed))
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

/// The key that signs `machine_id`'s record, if its fingerprint is the one the user checked.
/// The record can change between a warning and the command, so trust never goes to a key
/// the user was not shown.
fn shown_key(
    signers: Vec<(String, PublicKey)>,
    machine_id: &str,
    shown: &str,
) -> Result<PublicKey> {
    let Some((_, key)) = signers.into_iter().find(|(id, _)| id == machine_id) else {
        bail!(
            "Machine {} has no signed machine record. Run 'tether sync' on that machine first",
            machine_id
        );
    };
    let current = signing::fingerprint(&key);
    if current != shown {
        bail!(
            "Machine {} now signs with key {}, not {}. Check the key on that machine",
            machine_id,
            current,
            shown
        );
    }
    Ok(key)
}

/// The fingerprint of the key that signs `machine_id`'s record now.
pub fn signing_fingerprint(sync_path: &Path, machine_id: &str) -> Option<String> {
    signing::record_signers(sync_path)
        .into_iter()
        .find(|(id, _)| id == machine_id)
        .map(|(_, key)| signing::fingerprint(&key))
}

/// Trust the key that signs `machine_id`'s record in the sync repo, when its fingerprint is
/// `fingerprint`, and settle its inbox item.
pub fn trust_machine(
    sync_path: &Path,
    machine_id: &str,
    fingerprint: &str,
) -> Result<TrustedMachine> {
    let key = shown_key(signing::record_signers(sync_path), machine_id, fingerprint)?;
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
/// `update_all` leaves them at the installed version.
pub async fn hold_malicious_upgrades(
    manager: &dyn PackageManager,
    upgrades: &[Upgrade],
) -> Vec<String> {
    if osv::ecosystem_name(manager.ecosystem()).is_none() {
        return Vec::new();
    }
    let candidates: Vec<(String, String)> = upgrades
        .iter()
        .map(|u| (u.name.clone(), u.target.clone()))
        .collect();
    let pins: Vec<(String, Option<String>)> = candidates
        .iter()
        .map(|(name, version)| (name.clone(), Some(version.clone())))
        .collect();
    let found = osv::advisories(manager.ecosystem(), &pins).await;
    let mut held = Vec::new();
    let mut items = Vec::new();
    for ((name, version), advisories) in pins.into_iter().zip(found) {
        if !advisories.iter().any(|id| osv::is_malicious(id)) {
            continue;
        }
        Output::warning(&format!(
            "Skipping {} upgrade of {} to {}: OSV lists it as malicious ({})",
            manager.name(),
            name,
            version.as_deref().unwrap_or_default(),
            advisories.join(", ")
        ));
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
            reasons: vec![Reason::MaliciousUpgrade],
            advisories,
            first_seen: Utc::now(),
        });
    }
    let saved = Inbox::update(|inbox| {
        inbox.clear_upgrade_holds(manager.name(), &candidates);
        for item in items {
            inbox.add(item);
        }
        Ok(())
    });
    if let Err(e) = saved {
        Output::warning(&format!("Could not hold malicious upgrades: {}", e));
    }
    held
}

/// The release an unpinned install of a package would pick now, under the release-age limit.
/// None for Homebrew, or when the registry cannot tell.
pub async fn release_to_install(manager_key: &str, name: &str) -> Option<String> {
    let manager = manager_for_key(manager_key)?;
    let min_age = super::PackagePolicy::load().min_release_age_days;
    resolve::resolve_version(manager_key, manager.ecosystem(), name, min_age)
        .await
        .ok()
}

/// OSV could not check the release that would install, and the caller needs an answer before
/// it installs. `version` is the release that would install, when Tether found it.
#[derive(Debug, thiserror::Error)]
#[error("OSV could not check {name}: {error}")]
pub struct OsvUnchecked {
    pub name: String,
    pub error: String,
    pub version: Option<String>,
}

/// Refuse a package that OSV lists as malicious, or that this inbox holds as malicious.
/// Without `version`, OSV checks the release an unpinned install would pick now, and the
/// caller installs that release. Returns the version to install. With `osv_required`, a
/// release OSV could not check is refused too, as [`OsvUnchecked`], so the user can decide.
/// Homebrew has no OSV ecosystem.
pub async fn check_osv(
    manager_key: &str,
    name: &str,
    version: Option<&str>,
    osv_required: bool,
) -> Result<Option<String>> {
    let inbox = Inbox::load()?;
    let held = |version: Option<&str>| -> Result<()> {
        if inbox.holds_malicious(manager_key, name, version) {
            bail!(
                "The inbox holds {} {} as malicious. Tether will not install it",
                name,
                version.unwrap_or_default()
            );
        }
        Ok(())
    };
    let Some(manager) = manager_for_key(manager_key) else {
        held(version)?;
        return Ok(version.map(str::to_string));
    };
    let (version, unresolved) = match version {
        Some(v) => (Some(v.to_string()), None),
        None => {
            let min_age = super::PackagePolicy::load().min_release_age_days;
            match resolve::resolve_version(manager_key, manager.ecosystem(), name, min_age).await {
                Ok(v) => (Some(v), None),
                Err(e) => (None, Some(e.to_string())),
            }
        }
    };
    held(version.as_deref())?;
    let pin = [(name.to_string(), version.clone())];
    let (found, errors) = osv::query(manager.ecosystem(), &pin).await;
    let malicious: Vec<&str> = found[0]
        .iter()
        .filter(|id| osv::is_malicious(id))
        .map(String::as_str)
        .collect();
    let error = match (unresolved, malicious.is_empty()) {
        (None, false) => bail!(
            "OSV lists {} {} as malicious ({}). Tether will not install it",
            name,
            version.as_deref().unwrap_or_default(),
            malicious.join(", ")
        ),
        // An unpinned query returns every release's advisories, so this proves nothing about
        // the release that installs: the user decides, after a loud warning.
        (Some(e), false) => Some(format!(
            "Tether could not find the release that would install ({}), and OSV lists \
             MALICIOUS releases of it ({})",
            e,
            malicious.join(", ")
        )),
        _ => errors.into_iter().next(),
    };
    match error {
        Some(error) if osv_required => Err(OsvUnchecked {
            name: name.to_string(),
            error,
            version,
        }
        .into()),
        Some(error) => {
            Output::warning(&format!("{}: {}", name, error));
            Ok(version)
        }
        None => Ok(version),
    }
}

/// What a manual install puts on this machine: the release, and the tap of a formula or cask.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ManualInstall {
    pub version: Option<String>,
    pub tap: Option<String>,
}

/// The gates of a sync for a package that the user installs by hand, as `tether packages
/// install` and the dashboard's Import do. The user chose the package, so no trusted record
/// must list it. Every other gate applies: a package the inbox holds, for any reason, needs
/// approval; a rejected version or tap stays rejected; a tap or a formula or cask from a tap
/// that is not trusted goes to the inbox; the release is the one a sync resolves under the
/// release-age limit; and [`check_osv`] checks it. The caller holds the sync lock, so no
/// sync changes the inbox between these checks and the install.
pub async fn check_manual_install(
    manager_key: &str,
    name: &str,
    osv_required: bool,
) -> Result<ManualInstall> {
    use crate::sync::membership::canonical_id;
    let inbox = Inbox::load()?;
    let id = canonical_id(manager_key, name);
    if inbox
        .items
        .iter()
        .any(|i| i.kind == Kind::Package && canonical_id(&i.manager, &i.name) == id)
    {
        bail!(
            "{} waits in the inbox. Review it with 'tether packages inbox', then run 'tether \
             packages approve {}'",
            id,
            id
        );
    }
    let policy = super::PackagePolicy::load();
    let untrusted_tap = |tap: Option<String>| -> Result<()> {
        add(vec![InboxItem {
            kind: Kind::Package,
            manager: manager_key.to_string(),
            name: name.to_string(),
            version: None,
            tap: tap.clone(),
            source_machine: None,
            commit: None,
            signer: None,
            reasons: vec![Reason::UntrustedTap],
            advisories: Vec::new(),
            first_seen: Utc::now(),
        }])?;
        bail!(
            "{} is from tap {}, which is not trusted. It waits in the inbox: review it with \
             'tether packages inbox'",
            id,
            tap.as_deref().unwrap_or(name)
        )
    };
    match manager_key {
        "brew_taps" => {
            if inbox.is_rejected(manager_key, name, None, None) {
                bail!("You rejected tap {}", name);
            }
            if !policy.tap_trusted(name) {
                untrusted_tap(None)?;
            }
            Ok(ManualInstall::default())
        }
        "brew_formulae" | "brew_casks" => {
            let cask = manager_key == "brew_casks";
            let Some(tap) = BrewManager::new().tap_for(name, cask).await else {
                bail!("Cannot find the tap of {}", name);
            };
            if inbox.is_rejected(manager_key, name, None, Some(&tap)) {
                bail!("You rejected {} from tap {}", name, tap);
            }
            if !policy.brew_allowed(manager_key, name, &tap) {
                untrusted_tap(Some(tap.clone()))?;
            }
            Ok(ManualInstall {
                version: None,
                tap: Some(tap),
            })
        }
        _ => {
            let version = check_osv(manager_key, name, None, osv_required).await?;
            let Some(manager) = manager_for_key(manager_key) else {
                bail!("Unknown package manager {}", manager_key);
            };
            // As a sync, a manager that cannot enforce the limit installs only a release
            // whose age Tether checked
            if version.is_none()
                && policy.min_release_age_days > 0
                && manager.cooldown().await == super::Cooldown::Unsupported
            {
                bail!(
                    "Tether could not check the release age of {}, and this {} cannot enforce \
                     packages.min_release_age_days",
                    name,
                    manager.name()
                );
            }
            if inbox.is_rejected(manager_key, name, version.as_deref(), None) {
                bail!(
                    "You rejected {} {}",
                    name,
                    version.as_deref().unwrap_or_default()
                );
            }
            Ok(ManualInstall { version, tap: None })
        }
    }
}

/// Install a package another machine lists, as [`check_manual_install`] found it. The
/// caller holds the sync lock: a sync uninstalls a package this machine removed, so the
/// install also takes it off that list, and no sync may save the record meanwhile.
/// `interactive` lets a cask ask for a password.
pub async fn install_from_machine(
    manager: &str,
    name: &str,
    checked: ManualInstall,
    interactive: bool,
) -> Result<()> {
    let item = InboxItem {
        kind: Kind::Package,
        manager: manager.to_string(),
        name: name.to_string(),
        version: checked.version,
        tap: checked.tap,
        source_machine: None,
        commit: None,
        signer: None,
        reasons: Vec::new(),
        advisories: Vec::new(),
        first_seen: Utc::now(),
    };
    install(&item, interactive).await?;
    let sync_path = crate::sync::SyncEngine::sync_path()?;
    let machine_id = crate::sync::SyncState::load()?.machine_id;
    let Some(mut record) = signing::own_record(&sync_path, &machine_id)? else {
        return Ok(());
    };
    let Some(removed) = record.removed_packages.get_mut(manager) else {
        return Ok(());
    };
    let id = crate::sync::membership::canonical_id(manager, name);
    let before = removed.len();
    removed.retain(|n| crate::sync::membership::canonical_id(manager, n) != id);
    if removed.len() == before {
        return Ok(());
    }
    if removed.is_empty() {
        record.removed_packages.remove(manager);
    }
    signing::save_record(&sync_path, &record)
}

/// Install an approved item now. `interactive` lets a cask prompt for a password.
/// A machine item has nothing to install. The caller asks [`check_osv`] first, before it
/// approves the item, because an advisory can appear after the item was queued.
pub async fn install(item: &InboxItem, interactive: bool) -> Result<()> {
    if item.kind != Kind::Package {
        return Ok(());
    }
    let package = PackageInfo {
        name: item.name.clone(),
        version: item.version.clone(),
    };
    // The approval covers the tap the name resolved to at review, not the tap it resolves to now
    let brew_name = super::brew::qualified_name(&item.name, item.tap.as_deref());
    match item.manager.as_str() {
        "brew_taps" => BrewManager::new().tap(&item.name).await,
        "brew_formulae" => {
            BrewManager::new()
                .install(&PackageInfo {
                    name: brew_name,
                    version: None,
                })
                .await
        }
        "brew_casks" => {
            if BrewManager::new()
                .install_cask(&brew_name, interactive)
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
    fn items_group_by_machine_and_reasons() {
        let from = |machine: Option<&str>, name: &str, reasons: Vec<Reason>| InboxItem {
            source_machine: machine.map(str::to_string),
            reasons,
            ..item("gem", name)
        };
        let key = InboxItem {
            kind: Kind::TrustMachine {
                public_key: String::new(),
                fingerprint: "SHA256:x".to_string(),
            },
            source_machine: None,
            reasons: vec![Reason::NewMachine],
            ..item(MACHINE, "laptop")
        };
        let mut items = vec![
            from(Some("laptop"), "a", vec![Reason::Unsigned]),
            from(Some("desk"), "b", vec![Reason::Unsigned]),
            key,
            from(Some("laptop"), "c", vec![Reason::Unsigned]),
            from(Some("laptop"), "evil", vec![Reason::Malicious]),
            from(None, "d", vec![Reason::Unsigned]),
        ];
        sort_by_group(&mut items);
        let names: Vec<&str> = items.iter().map(|i| i.name.as_str()).collect();
        assert_eq!(names, ["d", "b", "a", "c", "evil", "laptop"]);
        let groups: Vec<(Option<String>, Vec<usize>)> = groups(&items)
            .into_iter()
            .map(|g| (g.machine, g.items))
            .collect();
        assert_eq!(
            groups,
            [
                (None, vec![0]),
                (Some("desk".to_string()), vec![1]),
                (Some("laptop".to_string()), vec![2, 3]),
                (Some("laptop".to_string()), vec![4]),
                (Some("laptop".to_string()), vec![5]),
            ]
        );
        // The malicious package and the machine key need their own answers
        let (approvable, held) = approvable_from(&items, "laptop");
        let names: Vec<&str> = approvable.iter().map(|i| i.name.as_str()).collect();
        assert_eq!((names, held), (vec!["a", "c"], 1));
    }

    #[test]
    fn add_skips_pending_and_rejected() {
        let mut inbox = Inbox::default();
        assert!(inbox.add(item("npm", "left-pad")));
        let mut cleared = item("npm", "left-pad");
        cleared.advisories = vec!["GHSA-x".to_string()];
        assert!(!inbox.add(cleared));
        assert_eq!(inbox.items[0].advisories, vec!["GHSA-x"]);
        // Another version under the same id is a new decision, so it is reported again
        let swapped = InboxItem {
            version: Some("6.6.6".to_string()),
            ..item("npm", "left-pad")
        };
        assert!(inbox.add(swapped));
        assert_eq!(inbox.items[0].version.as_deref(), Some("6.6.6"));
        assert!(inbox.add(item("pnpm", "left-pad")));
        let shown = inbox.find("npm:left-pad").unwrap().clone();
        inbox.reject(&shown).unwrap();
        assert!(inbox.is_rejected("npm", "left-pad", Some("6.6.6"), None));
        assert!(!inbox.add(shown));
    }

    #[test]
    fn rejection_covers_only_the_reviewed_version_tap_or_key() {
        let mut inbox = Inbox::default();
        let shown = item("npm", "left-pad");
        inbox.add(shown.clone());
        // A sync replaced the version after review, so the rejection is refused
        inbox.add(InboxItem {
            version: Some("6.6.6".to_string()),
            ..item("npm", "left-pad")
        });
        let err = inbox.reject(&shown).unwrap_err().to_string();
        assert!(err.contains("changed since you reviewed it"), "{err}");
        assert!(inbox.rejected.is_empty());

        let shown = inbox.find("npm:left-pad").unwrap().clone();
        inbox.reject(&shown).unwrap();
        assert!(inbox.is_rejected("npm", "left-pad", Some("6.6.6"), None));
        assert!(!inbox.is_rejected("npm", "left-pad", Some("6.6.7"), None));
        assert!(inbox.add(InboxItem {
            version: Some("6.6.7".to_string()),
            ..item("npm", "left-pad")
        }));

        let mut formula = item("brew_formulae", "bd");
        formula.version = None;
        formula.tap = Some("evil/tap".to_string());
        inbox.add(formula.clone());
        inbox.reject(&formula).unwrap();
        assert!(inbox.add(InboxItem {
            tap: Some("other/tap".to_string()),
            ..formula
        }));

        // Rejecting the key shown leaves a later key pending, with its warning
        let (old, new) = (key(), key());
        let mut store = TrustStore::default();
        store.trust("b", &key()).unwrap();
        let queued = inbox
            .hold_untrusted_keys(&store, vec![("b".to_string(), old.clone())], "me")
            .unwrap();
        inbox.reject(&queued[0]).unwrap();
        assert!(inbox
            .hold_untrusted_keys(&store, vec![("b".to_string(), old)], "me")
            .unwrap()
            .is_empty());
        let queued = inbox
            .hold_untrusted_keys(&store, vec![("b".to_string(), new.clone())], "me")
            .unwrap();
        assert_eq!(queued.len(), 1);
        assert_eq!(
            queued[0].fingerprint(),
            Some(signing::fingerprint(&new).as_str())
        );
        assert_eq!(queued[0].reasons, vec![Reason::KeyChanged]);
    }

    #[test]
    fn settle_drops_items_that_pass_and_reports_changed_ones() {
        let mut inbox = Inbox::default();
        let cooldown = InboxItem {
            reasons: vec![Reason::CooldownUnsupported],
            ..item("npm", "a")
        };
        let all = |_: &str, _: &str| true;
        let new = inbox.settle(vec![cooldown.clone(), item("npm", "b")], &[], all);
        assert_eq!(new.len(), 2);
        // The same reasons again are not news
        assert!(inbox.settle(vec![cooldown.clone()], &[], all).is_empty());
        // Other reasons replace the item and report it again
        let changed = InboxItem {
            reasons: vec![Reason::Unsigned, Reason::CooldownUnsupported],
            ..cooldown
        };
        assert_eq!(inbox.settle(vec![changed.clone()], &[], all), vec![changed]);
        // Once npm enforces the limit, the package passes and its item goes
        inbox.settle(Vec::new(), &[("npm".to_string(), "a".to_string())], all);
        let left: Vec<String> = inbox.items.iter().map(|i| i.id()).collect();
        assert_eq!(left, vec!["npm:b"]);
    }

    #[test]
    fn settle_drops_items_no_record_lists_any_more() {
        let mut inbox = Inbox::default();
        let machine = InboxItem {
            kind: Kind::TrustMachine {
                public_key: String::new(),
                fingerprint: String::new(),
            },
            ..item(MACHINE, "laptop")
        };
        inbox.items = vec![item("npm", "kept"), item("npm", "gone"), machine];
        inbox.settle(Vec::new(), &[], |_, name| name == "kept");
        let left: Vec<String> = inbox.items.iter().map(|i| i.id()).collect();
        assert_eq!(left, vec!["npm:kept", "machine:laptop"]);
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
    fn approving_a_formula_does_not_trust_its_tap() {
        let mut inbox = Inbox::default();
        let mut formula = item("brew_formulae", "bd");
        formula.version = None;
        formula.tap = Some("gastownhall/beads".to_string());
        inbox.add(formula);
        let approved = inbox.approve(&inbox.find("bd").unwrap().clone()).unwrap();
        assert_eq!(approved.name, "bd");
        assert!(inbox.items.is_empty());
        assert!(inbox.is_approved("brew_formulae", "bd", None, Some("gastownhall/beads")));
        assert!(!inbox.is_approved("brew_formulae", "bd", None, Some("evil/beads")));
        assert!(!inbox.is_approved("brew_formulae", "bd", None, None));
        assert_eq!(inbox.approved_taps().count(), 0);
        assert_eq!(
            inbox.approved_from_taps().collect::<Vec<_>>(),
            vec![("brew_formulae", "bd", "gastownhall/beads")]
        );

        inbox.add(item("brew_taps", "gastownhall/beads"));
        let tap = inbox.find("brew_taps:gastownhall/beads").unwrap().clone();
        inbox.approve(&tap).unwrap();
        assert_eq!(
            inbox.approved_taps().collect::<Vec<_>>(),
            vec!["gastownhall/beads"]
        );
    }

    #[test]
    fn approval_covers_only_the_approved_version() {
        let mut inbox = Inbox::default();
        inbox.add(item("npm", "left-pad"));
        inbox.approve(&item("npm", "left-pad")).unwrap();
        assert!(inbox.is_approved("npm", "left-pad", Some("1.0.0"), None));
        assert!(!inbox.is_approved("npm", "left-pad", Some("6.6.6"), None));
        assert!(!inbox.is_approved("npm", "left-pad", None, None));
    }

    #[test]
    fn approve_refuses_malicious() {
        let mut inbox = Inbox::default();
        let mut bad = item("npm", "nx");
        bad.reasons = vec![Reason::Malicious];
        bad.advisories = vec!["MAL-2025-41443".to_string()];
        inbox.add(bad);
        let shown = inbox.find("nx").unwrap().clone();
        assert!(inbox.approve(&shown).is_err());
        assert!(inbox.is_pending("npm", "nx"));
        assert!(inbox.holds_malicious("npm", "nx", Some("1.0.0")));
        assert!(!inbox.holds_malicious("pnpm", "nx", None));
        inbox.add(item("npm", "safe"));
        assert!(!inbox.holds_malicious("npm", "safe", None));
    }

    #[test]
    fn upgrade_hold_blocks_only_its_version_until_the_target_moves() {
        let mut inbox = Inbox::default();
        let mut bad = item("npm", "zx");
        bad.version = Some("2.0.1".to_string());
        bad.reasons = vec![Reason::MaliciousUpgrade];
        inbox.add(bad.clone());
        assert!(inbox.approve(&bad).is_err());
        assert!(inbox.holds_malicious("npm", "zx", Some("2.0.1")));
        assert!(inbox.holds_malicious("npm", "zx", None));
        assert!(!inbox.holds_malicious("npm", "zx", Some("2.0.2")));

        // The flagged version is still the upgrade target, and other managers do not count
        let target = |v: &str| vec![("zx".to_string(), v.to_string())];
        inbox.clear_upgrade_holds("npm", &target("2.0.1"));
        inbox.clear_upgrade_holds("pnpm", &[]);
        assert!(inbox.is_pending("npm", "zx"));
        // A clean 2.0.2 replaced it as the target
        inbox.clear_upgrade_holds("npm", &target("2.0.2"));
        assert!(!inbox.is_pending("npm", "zx"));

        // The package is no longer outdated, so it moved past the flagged version
        inbox.add(bad);
        let mut sync_held = item("npm", "evil");
        sync_held.reasons = vec![Reason::Malicious];
        inbox.add(sync_held);
        inbox.clear_upgrade_holds("npm", &[]);
        assert!(!inbox.is_pending("npm", "zx"));
        assert!(inbox.holds_malicious("npm", "evil", Some("2.0.2")));
    }

    #[test]
    fn approval_refuses_an_item_that_changed_after_review() {
        let (b, b2) = (key(), key());
        let mut inbox = Inbox::default();
        let shown = InboxItem::machine("b", &b, Reason::NewMachine).unwrap();
        inbox.add(shown.clone());
        // A sync replaces the item under the same id with another key
        inbox.add(InboxItem::machine("b", &b2, Reason::NewMachine).unwrap());
        let err = inbox.approve(&shown).unwrap_err().to_string();
        assert!(err.contains("changed since you reviewed it"), "{err}");
        assert!(inbox.is_pending(MACHINE, "b"));

        let shown = item("npm", "left-pad");
        inbox.add(shown.clone());
        inbox.add(InboxItem {
            version: Some("6.6.6".to_string()),
            ..item("npm", "left-pad")
        });
        assert!(inbox.approve(&shown).is_err());
        let mut formula = item("brew_formulae", "bd");
        formula.tap = Some("good/tap".to_string());
        inbox.add(formula.clone());
        inbox.add(InboxItem {
            tap: Some("evil/tap".to_string()),
            ..formula.clone()
        });
        assert!(inbox.approve(&formula).is_err());
        assert!(inbox.approved.is_empty());
        assert!(inbox.approve(&item("npm", "gone")).is_err());
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
        // The source machine is trusted, but its record fails its signature
        let forged = Checks {
            signature_failed: true,
            ..other
        };
        assert_eq!(reasons(forged), vec![Reason::SignatureFailed]);
        assert_eq!(
            reasons(Checks {
                signer: Signer::Untrusted,
                ..forged
            }),
            vec![Reason::SignatureFailed]
        );
        assert!(reasons(Checks {
            signer: Signer::Trusted,
            ..forged
        })
        .is_empty());
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
        // A malicious release of an unpinned package whose install version is unknown holds
        // it, but approval overrides that, unlike a malicious release that would install
        assert_eq!(
            reasons(Checks {
                malicious_unresolved: true,
                ..trusted
            }),
            vec![Reason::MaliciousUnresolved]
        );
        assert!(reasons(Checks {
            approved: true,
            malicious_unresolved: true,
            ..other
        })
        .is_empty());
        let mut inbox = Inbox::default();
        let shown = InboxItem {
            version: None,
            reasons: vec![Reason::MaliciousUnresolved],
            ..item("npm", "chalk")
        };
        inbox.add(shown.clone());
        assert!(inbox.approve(&shown).is_ok());
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

        let approved = inbox.approve(&queued[0]).unwrap();
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
    fn trust_command_takes_only_the_key_the_user_saw() {
        let (b, b2) = (key(), key());
        let signers = vec![("b".to_string(), b2.clone())];
        assert!(shown_key(signers.clone(), "b", &signing::fingerprint(&b)).is_err());
        assert!(shown_key(signers.clone(), "c", &signing::fingerprint(&b2)).is_err());
        let key = shown_key(signers, "b", &signing::fingerprint(&b2)).unwrap();
        assert_eq!(key.key_data(), b2.key_data());
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
        inbox.approve(&item("uv", "ruff")).unwrap();
        inbox.add(item("gem", "rails"));
        let json = serde_json::to_string(&inbox).unwrap();
        assert_eq!(serde_json::from_str::<Inbox>(&json).unwrap(), inbox);
        assert!(json.contains("\"unsigned\""));
    }
}

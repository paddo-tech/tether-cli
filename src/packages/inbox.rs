use super::{manager_for_key, osv, BrewManager, PackageInfo, PackageManager};
use anyhow::{bail, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Why a synced package waits for the user instead of installing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Reason {
    /// It came from another machine, and nothing yet proves that machine made the change.
    Unsigned,
    /// It is a tap outside the trusted taps, or a formula or cask from one.
    UntrustedTap,
    /// The installed manager cannot enforce `packages.min_release_age_days`.
    CooldownUnsupported,
    /// OSV lists a `MAL-` advisory for it. Approval cannot override this.
    Malicious,
}

impl Reason {
    pub fn label(self) -> &'static str {
        match self {
            Reason::Unsigned => "from another machine",
            Reason::UntrustedTap => "untrusted tap",
            Reason::CooldownUnsupported => "release age not checked",
            Reason::Malicious => "malicious (OSV)",
        }
    }
}

/// A package held for approval. `source_machine` and `commit` identify the manifest
/// change it came from, so signature checks can verify that commit later.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InboxItem {
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
    pub reasons: Vec<Reason>,
    /// OSV advisory ids for this version, including non-blocking ones
    #[serde(default)]
    pub advisories: Vec<String>,
    pub first_seen: DateTime<Utc>,
}

impl InboxItem {
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
    pub fn add(&mut self, item: InboxItem) -> bool {
        if self.is_pending(&item.manager, &item.name) || self.is_rejected(&item.manager, &item.name)
        {
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
    /// because brew cannot install it otherwise.
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

    /// Drop pending items for packages that are now installed by other means.
    pub fn prune_installed(&mut self, manager: &str, installed: impl Fn(&str) -> bool) {
        self.items
            .retain(|i| i.manager != manager || !installed(&i.name));
    }
}

/// What the trust checks found for one synced package.
#[derive(Debug, Clone, Copy, Default)]
pub struct Checks {
    /// This machine's own state lists the package
    pub from_this_machine: bool,
    pub approved: bool,
    pub auto_install_from_trusted: bool,
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
    // Commit signatures do not exist yet, so another machine's package installs on its own only when auto_install_from_trusted opts in
    if !checks.from_this_machine && !checks.auto_install_from_trusted {
        reasons.push(Reason::Unsigned);
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

/// Record approval. The caller installs the item with [`install`] or leaves it to the next sync.
pub fn approve(query: &str) -> Result<InboxItem> {
    Inbox::update(|inbox| inbox.approve(query))
}

pub fn reject(query: &str) -> Result<InboxItem> {
    Inbox::update(|inbox| inbox.reject(query))
}

/// Install an approved item now. `interactive` lets a cask prompt for a password.
/// OSV is asked again, because an advisory can appear after the item was queued.
pub async fn install(item: &InboxItem, interactive: bool) -> Result<()> {
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
            manager: manager.to_string(),
            name: name.to_string(),
            version: Some("1.0.0".to_string()),
            tap: None,
            source_machine: Some("other".to_string()),
            commit: Some("abc123".to_string()),
            reasons: vec![Reason::Unsigned],
            advisories: Vec::new(),
            first_seen: Utc::now(),
        }
    }

    #[test]
    fn add_skips_pending_and_rejected() {
        let mut inbox = Inbox::default();
        assert!(inbox.add(item("npm", "left-pad")));
        assert!(!inbox.add(item("npm", "left-pad")));
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
        inbox.prune_installed("npm", |name| name == "a");
        assert_eq!(inbox.items.len(), 1);
        assert_eq!(inbox.items[0].manager, "pnpm");
    }

    #[test]
    fn reasons_follow_the_trust_model() {
        let other = Checks::default();
        assert_eq!(reasons(other), vec![Reason::Unsigned]);
        assert!(reasons(Checks {
            auto_install_from_trusted: true,
            ..other
        })
        .is_empty());
        assert!(reasons(Checks {
            from_this_machine: true,
            ..other
        })
        .is_empty());
        assert_eq!(
            reasons(Checks {
                auto_install_from_trusted: true,
                cooldown_unsupported: true,
                untrusted_tap: true,
                ..other
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

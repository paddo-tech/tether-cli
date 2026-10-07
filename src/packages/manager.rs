use super::pin::{manifest_names, parse_pin};
use super::{validate_name, validate_version, Cooldown, Ecosystem};
use anyhow::Result;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::cmp::Ordering;
use std::collections::HashSet;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PackageInfo {
    pub name: String,
    pub version: Option<String>,
}

/// A package that an outdated check lists, with the installed version and the version an
/// upgrade would install. A held package stays at `current` whatever its target.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Upgrade {
    pub name: String,
    pub current: Option<String>,
    pub target: String,
    pub hold: Option<Hold>,
    /// A Homebrew cask, which brew upgrades with `--cask`
    pub cask: bool,
}

/// Why an upgrade leaves a package alone.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Hold {
    /// No release of the package is older than the release-age limit
    NoMatureRelease,
    /// The user pinned the package, for example with `uv tool install name==1.0`
    Pinned,
}

impl Upgrade {
    pub fn new(name: &str, current: Option<&str>, target: &str) -> Self {
        Self {
            name: name.to_string(),
            current: current.map(str::to_string),
            target: target.to_string(),
            hold: None,
            cask: false,
        }
    }

    pub fn held(name: &str, current: Option<&str>, hold: Hold) -> Self {
        Self {
            hold: Some(hold),
            ..Self::new(name, current, current.unwrap_or_default())
        }
    }

    /// The release-age limit can make the target older than the installed version, and an
    /// upgrade must never downgrade. An unknown installed version cannot prove the target
    /// newer, except for Homebrew, which has no release-age limit.
    pub fn moves_forward(&self, ecosystem: Ecosystem) -> bool {
        self.hold.is_none() && self.order(ecosystem) == Some(Ordering::Greater)
    }

    pub fn is_downgrade(&self, ecosystem: Ecosystem) -> bool {
        self.hold.is_none() && self.order(ecosystem) == Some(Ordering::Less)
    }

    /// The target against the installed version, by the ecosystem's own rules: a PEP 440
    /// `1.0.dev1` is older than `1.0a1`, and a Homebrew revision `_1` is newer than its release.
    fn order(&self, ecosystem: Ecosystem) -> Option<Ordering> {
        match ecosystem {
            // brew lists only packages it finds outdated, and never installs an older release.
            // A cask whose version is `latest` cannot compare, and brew still upgrades it.
            Ecosystem::Brew | Ecosystem::BrewTap => Some(
                self.current
                    .as_deref()
                    .and_then(|current| compare_brew_versions(&self.target, current))
                    .unwrap_or(Ordering::Greater),
            ),
            _ => Some(super::pin::compare_versions(
                ecosystem,
                &self.target,
                self.current.as_deref()?,
            )),
        }
    }

    /// One line on why a held package stays, or `None` when it is not held.
    pub fn hold_note(&self) -> Option<String> {
        let current = self.current.as_deref().unwrap_or("?");
        self.hold.as_ref().map(|hold| match hold {
            Hold::NoMatureRelease => format!(
                "{} skipped: no release is older than the release-age limit",
                self.name
            ),
            Hold::Pinned => format!("{} pinned at {}, not upgraded", self.name, current),
        })
    }
}

/// Order two Homebrew versions: numeric dotted parts first, then a suffix. A suffix such as
/// `-rc.1` or `rc1` sorts before the release, and a revision such as `_1` after it. `+build`
/// metadata does not count. `None` when a version does not start with a number, such as a
/// cask's `latest`.
pub fn compare_brew_versions(a: &str, b: &str) -> Option<Ordering> {
    fn split(v: &str) -> Option<(Vec<u64>, &str)> {
        let v = v.trim().trim_start_matches('v');
        let v = v.split('+').next().unwrap_or(v);
        let end = v
            .find(|c: char| !c.is_ascii_digit() && c != '.')
            .unwrap_or(v.len());
        let core = v[..end].trim_end_matches('.');
        if core.is_empty() {
            return None;
        }
        let parts = core
            .split('.')
            .map(|p| p.parse::<u64>().ok())
            .collect::<Option<Vec<u64>>>()?;
        Some((parts, &v[end..]))
    }
    /// Rank and text of the suffix. A Homebrew revision such as `_1` sorts after the release.
    fn suffix(raw: &str) -> (u8, &str) {
        if let Some(rev) = raw
            .strip_prefix('_')
            .filter(|r| !r.is_empty() && r.bytes().all(|b| b.is_ascii_digit()))
        {
            return (2, rev);
        }
        let s = raw.trim_start_matches(['-', '.', '_']);
        let rank = match s {
            "" => 1,
            s if s.starts_with("post") => 2,
            _ => 0,
        };
        (rank, s)
    }
    let (core_a, raw_a) = split(a)?;
    let (core_b, raw_b) = split(b)?;
    let ((rank_a, suffix_a), (rank_b, suffix_b)) = (suffix(raw_a), suffix(raw_b));
    for i in 0..core_a.len().max(core_b.len()) {
        let x = core_a.get(i).copied().unwrap_or(0);
        let y = core_b.get(i).copied().unwrap_or(0);
        if x != y {
            return Some(x.cmp(&y));
        }
    }
    Some(
        rank_a
            .cmp(&rank_b)
            .then_with(|| natural_cmp(suffix_a, suffix_b)),
    )
}

/// The upgrades a plan lists: candidates whose target is newer than the installed version.
/// A candidate whose target is older stays at its installed version with a warning. A held
/// package stays too; `tether upgrade` prints why in its plan, so here it goes to the daemon
/// log only.
pub async fn planned_upgrades(manager: &dyn PackageManager) -> Result<Vec<Upgrade>> {
    let (forward, kept): (Vec<Upgrade>, Vec<Upgrade>) = manager
        .upgrade_candidates()
        .await?
        .into_iter()
        .partition(|u| u.moves_forward(manager.ecosystem()));
    for note in kept.iter().filter_map(Upgrade::hold_note) {
        log::info!("{}: {}", manager.name(), note);
    }
    for upgrade in kept.iter().filter(|u| u.is_downgrade(manager.ecosystem())) {
        crate::cli::Output::warning(&format!(
            "Kept {} {} at {}: the release-age limit allows only {}",
            manager.name(),
            upgrade.name,
            upgrade.current.as_deref().unwrap_or_default(),
            upgrade.target
        ));
    }
    Ok(forward)
}

/// Plan the upgrades and install them, as the daemon's daily update does. A failed plan
/// upgrades nothing.
pub async fn update_all(manager: &dyn PackageManager) -> Result<()> {
    manager.refresh().await?;
    let planned = planned_upgrades(manager).await?;
    install_upgrades(manager, planned).await
}

/// Install exactly the planned upgrades, without the ones whose target OSV lists as
/// malicious. Those go to the inbox and stay at their installed version.
pub async fn install_upgrades(manager: &dyn PackageManager, planned: Vec<Upgrade>) -> Result<()> {
    let held = super::inbox::hold_malicious_upgrades(manager, &planned).await;
    let planned: Vec<Upgrade> = planned
        .into_iter()
        .filter(|u| !held.contains(&u.name))
        .collect();
    if planned.is_empty() {
        return Ok(());
    }
    manager.upgrade(&planned).await
}

/// Compare digit runs as numbers, so `rc.10` sorts after `rc.9`.
fn natural_cmp(a: &str, b: &str) -> Ordering {
    fn runs(s: &str) -> Vec<String> {
        let mut runs: Vec<String> = Vec::new();
        for c in s.chars() {
            match runs.last_mut() {
                Some(run)
                    if run.starts_with(|r: char| r.is_ascii_digit()) == c.is_ascii_digit() =>
                {
                    run.push(c)
                }
                _ => runs.push(c.to_string()),
            }
        }
        runs
    }
    let (ra, rb) = (runs(a), runs(b));
    for (x, y) in ra.iter().zip(&rb) {
        let order = match (x.parse::<u64>(), y.parse::<u64>()) {
            (Ok(x), Ok(y)) => x.cmp(&y),
            _ => x.cmp(y),
        };
        if order != Ordering::Equal {
            return order;
        }
    }
    ra.len().cmp(&rb.len())
}

#[async_trait]
pub trait PackageManager: Send + Sync {
    /// List all installed packages (legacy method, kept for compatibility)
    async fn list_installed(&self) -> Result<Vec<PackageInfo>>;

    /// Install a specific package (legacy method, kept for compatibility)
    async fn install(&self, package: &PackageInfo) -> Result<()>;

    /// Check if this package manager is available on the system
    async fn is_available(&self) -> bool;

    /// Get the name of this package manager
    fn name(&self) -> &str;

    /// Registry naming rules this manager's packages follow
    fn ecosystem(&self) -> Ecosystem;

    /// Flags that enforce `packages.min_release_age_days` with the installed tool version
    async fn cooldown(&self) -> Cooldown;

    /// Export installed packages to a manifest file using native tooling
    /// Returns the content of the manifest as a String
    async fn export_manifest(&self) -> Result<String> {
        let packages = self.list_installed().await?;
        let manifest = packages
            .iter()
            .map(|p| p.name.as_str())
            .collect::<Vec<_>>()
            .join("\n");
        Ok(manifest)
    }

    /// Import packages from a manifest file using native tooling
    /// The manifest_content is the content that was previously exported
    async fn import_manifest(&self, manifest_content: &str) -> Result<()> {
        let packages: Vec<PackageInfo> = manifest_content
            .lines()
            .map(|line| line.trim())
            .filter(|line| !line.is_empty())
            .map(|line| {
                let (name, version) = parse_pin(self.ecosystem(), line);
                PackageInfo { name, version }
            })
            .filter(|p| {
                let checked = validate_name(self.ecosystem(), &p.name).and_then(|()| {
                    p.version
                        .as_deref()
                        .map_or(Ok(()), |v| validate_version(self.ecosystem(), v))
                });
                match checked {
                    Ok(()) => true,
                    Err(e) => {
                        crate::cli::Output::warning(&format!(
                            "Skipping {} entry: {}",
                            self.name(),
                            e
                        ));
                        false
                    }
                }
            })
            .collect();

        if packages.is_empty() {
            return Ok(());
        }

        let installed = self.list_installed().await?;
        let installed_names: HashSet<_> = installed.iter().map(|p| p.name.as_str()).collect();

        for package in packages {
            if !installed_names.contains(package.name.as_str()) {
                if let Err(e) = self.install(&package).await {
                    crate::cli::Output::warning(&format!(
                        "Failed to install {}: {}",
                        package.name, e
                    ));
                }
            }
        }

        Ok(())
    }

    /// Remove packages not in the manifest
    async fn remove_unlisted(&self, manifest_content: &str) -> Result<()> {
        let names = manifest_names(self.ecosystem(), manifest_content);
        let desired: HashSet<&str> = names.iter().map(String::as_str).collect();

        if desired.is_empty() {
            return Ok(());
        }

        let installed = self.list_installed().await?;

        for pkg in installed {
            if !desired.contains(pkg.name.as_str()) {
                if let Err(e) = self.uninstall(&pkg.name).await {
                    crate::cli::Output::warning(&format!(
                        "Failed to uninstall {}: {}",
                        pkg.name, e
                    ));
                }
            }
        }

        Ok(())
    }

    /// Refresh the data the outdated check reads, before a plan.
    async fn refresh(&self) -> Result<()> {
        Ok(())
    }

    /// Install exactly these upgrades, each at its target version. Callers pass a planned,
    /// non-empty list: an upgrade command without names would upgrade every package.
    async fn upgrade(&self, planned: &[Upgrade]) -> Result<()>;

    /// Installed packages with their versions, for the report after an upgrade.
    async fn installed_versions(&self) -> Result<Vec<PackageInfo>> {
        self.list_installed().await
    }

    /// Packages the manager's outdated check lists, with the version an upgrade would
    /// install. A target can be older than the installed version; `update_all` skips those.
    async fn upgrade_candidates(&self) -> Result<Vec<Upgrade>> {
        anyhow::bail!("{} cannot list upgrade candidates", self.name())
    }

    /// Compute a hash of the current manifest for change detection
    async fn compute_manifest_hash(&self) -> Result<String> {
        let manifest = self.export_manifest().await?;
        Ok(crate::sha256_hex(manifest.as_bytes()))
    }

    /// Uninstall a package by name
    async fn uninstall(&self, package: &str) -> Result<()>;

    /// Get packages that depend on this package (reverse dependencies)
    /// Default implementation returns empty (most managers can't query this)
    async fn get_dependents(&self, _package: &str) -> Result<Vec<String>> {
        Ok(vec![])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    /// A manager that records the upgrades it is asked to install.
    struct Fake {
        candidates: Option<Vec<Upgrade>>,
        upgraded: Mutex<Vec<Vec<String>>>,
    }

    #[async_trait]
    impl PackageManager for Fake {
        async fn list_installed(&self) -> Result<Vec<PackageInfo>> {
            Ok(Vec::new())
        }
        async fn install(&self, _: &PackageInfo) -> Result<()> {
            Ok(())
        }
        async fn is_available(&self) -> bool {
            true
        }
        fn name(&self) -> &str {
            "fake"
        }
        // Homebrew has no OSV ecosystem, so no OSV request leaves the test
        fn ecosystem(&self) -> Ecosystem {
            Ecosystem::Brew
        }
        async fn cooldown(&self) -> Cooldown {
            Cooldown::Off
        }
        async fn upgrade(&self, planned: &[Upgrade]) -> Result<()> {
            self.upgraded
                .lock()
                .unwrap()
                .push(planned.iter().map(|u| u.name.clone()).collect());
            Ok(())
        }
        async fn upgrade_candidates(&self) -> Result<Vec<Upgrade>> {
            self.candidates
                .clone()
                .ok_or_else(|| anyhow::anyhow!("outdated check failed"))
        }
        async fn uninstall(&self, _: &str) -> Result<()> {
            Ok(())
        }
    }

    #[tokio::test]
    async fn a_failed_plan_upgrades_nothing() {
        let fake = Fake {
            candidates: None,
            upgraded: Mutex::new(Vec::new()),
        };
        assert!(update_all(&fake).await.is_err());
        // Before, gem ran `gem update` without names here, which updates every gem
        assert!(fake.upgraded.lock().unwrap().is_empty());

        let fake = Fake {
            candidates: Some(Vec::new()),
            upgraded: Mutex::new(Vec::new()),
        };
        update_all(&fake).await.unwrap();
        assert!(fake.upgraded.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn only_the_planned_upgrades_install() {
        let fake = Fake {
            candidates: Some(vec![
                Upgrade::new("a", Some("1.0"), "2.0"),
                Upgrade::new("b", Some("2.0"), "1.0"),
            ]),
            upgraded: Mutex::new(Vec::new()),
        };
        update_all(&fake).await.unwrap();
        assert_eq!(*fake.upgraded.lock().unwrap(), vec![vec!["a".to_string()]]);
    }

    #[test]
    fn versions_compare_numerically_with_prereleases_first() {
        let lt = |a: &str, b: &str| compare_brew_versions(a, b) == Some(Ordering::Less);
        assert!(lt("0.62.0", "0.63.0"));
        assert!(lt("1.9.0", "1.10.0"));
        assert!(lt("1.0.0-rc.9", "1.0.0-rc.10"));
        assert!(lt("1.0.0-beta.1", "1.0.0"));
        assert!(lt("1.0rc1", "1.0"));
        assert!(lt("1.0", "1.0.post1"));
        assert!(lt("5.0", "v6.1"));
        assert!(lt("1.2.3", "1.2.3_1"));
        assert!(lt("1.2.3_1", "1.2.3_2"));
        assert!(lt("1.2.3_9", "1.2.4"));
        assert_eq!(compare_brew_versions("1.0", "1.0.0"), Some(Ordering::Equal));
        assert_eq!(
            compare_brew_versions("1.0.0+b1", "1.0.0"),
            Some(Ordering::Equal)
        );
        assert_eq!(compare_brew_versions("latest", "1.0.0"), None);
    }

    #[test]
    fn only_a_newer_known_target_moves_forward() {
        let npm = Ecosystem::Npm;
        assert!(Upgrade::new("a", Some("0.62.0"), "0.63.0").moves_forward(npm));
        let older = Upgrade::new("a", Some("0.63.0"), "0.62.0");
        assert!(!older.moves_forward(npm));
        assert!(older.is_downgrade(npm));
        assert!(!Upgrade::new("a", Some("1.0.0"), "1.0.0").moves_forward(npm));
        assert!(!Upgrade::new("a", None, "1.0.0").moves_forward(npm));
    }

    #[test]
    fn direction_follows_each_ecosystem() {
        let py = Ecosystem::Python;
        assert!(Upgrade::new("a", Some("1.0.dev1"), "1.0a1").moves_forward(py));
        assert!(Upgrade::new("a", Some("1.0a1"), "1.0.dev1").is_downgrade(py));
        assert!(Upgrade::new("a", Some("1.0a1.dev2"), "1.0a1").moves_forward(py));
        assert!(Upgrade::new("a", Some("3.0"), "1!2.0").moves_forward(py));
        assert!(Upgrade::new("a", Some("1!2.0"), "3.0").is_downgrade(py));
        let npm = Ecosystem::Npm;
        assert!(Upgrade::new("a", Some("1.0.0-rc.1"), "1.0.0").moves_forward(npm));
        assert!(Upgrade::new("a", Some("1.0.0-beta.2"), "1.0.0-beta.10").moves_forward(npm));
        assert!(Upgrade::new("a", Some("1.0.0"), "1.0.0-rc.1").is_downgrade(npm));
        let gem = Ecosystem::Gem;
        assert!(Upgrade::new("a", Some("1.0.pre"), "1.0").moves_forward(gem));
        assert!(Upgrade::new("a", Some("1.0"), "1.0.rc2").is_downgrade(gem));
        let brew = Ecosystem::Brew;
        assert!(Upgrade::new("a", Some("1.2.3"), "1.2.3_1").moves_forward(brew));
        // brew lists only outdated packages, so a `latest` cask upgrades and shows in the plan
        assert!(Upgrade::new("a", Some("latest"), "latest").moves_forward(brew));
        assert!(Upgrade::new("a", None, "2.0").moves_forward(brew));
    }
}

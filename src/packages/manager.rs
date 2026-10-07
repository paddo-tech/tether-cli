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
/// upgrade would install.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Upgrade {
    pub name: String,
    pub current: Option<String>,
    pub target: String,
}

impl Upgrade {
    pub fn new(name: &str, current: Option<&str>, target: &str) -> Self {
        Self {
            name: name.to_string(),
            current: current.map(str::to_string),
            target: target.to_string(),
        }
    }

    /// The release-age limit can make the target older than the installed version, and an
    /// upgrade must never downgrade. An unknown installed version cannot prove the target newer.
    pub fn moves_forward(&self) -> bool {
        self.current.as_deref().is_some_and(|current| {
            compare_versions(&self.target, current) == Some(Ordering::Greater)
        })
    }

    pub fn is_downgrade(&self) -> bool {
        self.current
            .as_deref()
            .is_some_and(|current| compare_versions(&self.target, current) == Some(Ordering::Less))
    }
}

/// Order two versions of the forms npm, PyPI, RubyGems and Homebrew use: numeric dotted
/// parts first, then a suffix. A suffix such as `-rc.1`, `rc1` or `.pre` sorts before the
/// release, `post` sorts after it, and `+build` metadata does not count. `None` when a
/// version does not start with a number.
pub fn compare_versions(a: &str, b: &str) -> Option<Ordering> {
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
        Some((parts, v[end..].trim_start_matches(['-', '.', '_'])))
    }
    fn suffix_rank(s: &str) -> u8 {
        match s {
            "" => 1,
            s if s.starts_with("post") => 2,
            _ => 0,
        }
    }
    let (core_a, suffix_a) = split(a)?;
    let (core_b, suffix_b) = split(b)?;
    for i in 0..core_a.len().max(core_b.len()) {
        let x = core_a.get(i).copied().unwrap_or(0);
        let y = core_b.get(i).copied().unwrap_or(0);
        if x != y {
            return Some(x.cmp(&y));
        }
    }
    Some(
        suffix_rank(suffix_a)
            .cmp(&suffix_rank(suffix_b))
            .then_with(|| natural_cmp(suffix_a, suffix_b)),
    )
}

/// The upgrades `update_all` installs: candidates whose target is newer than the installed
/// version, without the ones OSV lists as malicious. A candidate whose target is older
/// stays at its installed version with a warning.
pub async fn planned_upgrades(manager: &dyn PackageManager) -> Result<Vec<Upgrade>> {
    let (forward, kept): (Vec<Upgrade>, Vec<Upgrade>) = manager
        .upgrade_candidates()
        .await?
        .into_iter()
        .partition(Upgrade::moves_forward);
    for upgrade in kept.iter().filter(|u| u.is_downgrade()) {
        crate::cli::Output::warning(&format!(
            "Kept {} {} at {}: the release-age limit allows only {}",
            manager.name(),
            upgrade.name,
            upgrade.current.as_deref().unwrap_or_default(),
            upgrade.target
        ));
    }
    let held = super::inbox::hold_malicious_upgrades(manager, &forward).await;
    Ok(forward
        .into_iter()
        .filter(|u| !held.contains(&u.name))
        .collect())
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

    /// Update all installed packages to latest versions
    async fn update_all(&self) -> Result<()>;

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

    #[test]
    fn versions_compare_numerically_with_prereleases_first() {
        let lt = |a: &str, b: &str| compare_versions(a, b) == Some(Ordering::Less);
        assert!(lt("0.62.0", "0.63.0"));
        assert!(lt("1.9.0", "1.10.0"));
        assert!(lt("1.0.0-rc.9", "1.0.0-rc.10"));
        assert!(lt("1.0.0-beta.1", "1.0.0"));
        assert!(lt("1.0rc1", "1.0"));
        assert!(lt("1.0", "1.0.post1"));
        assert!(lt("5.0", "v6.1"));
        assert_eq!(compare_versions("1.0", "1.0.0"), Some(Ordering::Equal));
        assert_eq!(compare_versions("1.0.0+b1", "1.0.0"), Some(Ordering::Equal));
        assert_eq!(compare_versions("latest", "1.0.0"), None);
    }

    #[test]
    fn only_a_newer_known_target_moves_forward() {
        assert!(Upgrade::new("a", Some("0.62.0"), "0.63.0").moves_forward());
        let older = Upgrade::new("a", Some("0.63.0"), "0.62.0");
        assert!(!older.moves_forward());
        assert!(older.is_downgrade());
        assert!(!Upgrade::new("a", Some("1.0.0"), "1.0.0").moves_forward());
        assert!(!Upgrade::new("a", None, "1.0.0").moves_forward());
    }
}

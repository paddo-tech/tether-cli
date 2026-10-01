use super::pin::{manifest_names, parse_pin};
use super::{validate_name, validate_version, Cooldown, Ecosystem};
use anyhow::Result;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PackageInfo {
    pub name: String,
    pub version: Option<String>,
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
                        eprintln!("Warning: Skipping {} entry: {}", self.name(), e);
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
                    eprintln!("Warning: Failed to install {}: {}", package.name, e);
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
                    eprintln!("Warning: Failed to uninstall {}: {}", pkg.name, e);
                }
            }
        }

        Ok(())
    }

    /// Update all installed packages to latest versions
    async fn update_all(&self) -> Result<()>;

    /// Packages `update_all` would change, with the version each would move to.
    async fn upgrade_candidates(&self) -> Result<Vec<(String, String)>> {
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

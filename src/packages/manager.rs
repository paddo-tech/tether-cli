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
        let package_names: Vec<&str> = manifest_content
            .lines()
            .map(|line| line.trim())
            .filter(|line| !line.is_empty())
            .collect();

        if package_names.is_empty() {
            return Ok(());
        }

        let installed = self.list_installed().await?;
        let installed_names: HashSet<_> = installed.iter().map(|p| p.name.as_str()).collect();

        for name in package_names {
            if !installed_names.contains(name) {
                if let Err(e) = self
                    .install(&PackageInfo {
                        name: name.to_string(),
                        version: None,
                    })
                    .await
                {
                    eprintln!("Warning: Failed to install {}: {}", name, e);
                }
            }
        }

        Ok(())
    }

    /// Remove packages not in the manifest
    async fn remove_unlisted(&self, manifest_content: &str) -> Result<()> {
        let desired: HashSet<&str> = manifest_content
            .lines()
            .map(|l| l.trim())
            .filter(|l| !l.is_empty())
            .collect();

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

    /// Compute a hash of the current manifest for change detection
    async fn compute_manifest_hash(&self) -> Result<String> {
        let manifest = self.export_manifest().await?;
        use sha2::{Digest, Sha256};
        Ok(format!("{:x}", Sha256::digest(manifest.as_bytes())))
    }

    /// Uninstall a package by name
    async fn uninstall(&self, package: &str) -> Result<()>;

    /// Get packages that depend on this package (reverse dependencies)
    /// Default implementation returns empty (most managers can't query this)
    async fn get_dependents(&self, _package: &str) -> Result<Vec<String>> {
        Ok(vec![])
    }
}

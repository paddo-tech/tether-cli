use anyhow::Result;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};

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
    async fn export_manifest(&self) -> Result<String>;

    /// Import packages from a manifest file using native tooling
    /// The manifest_content is the content that was previously exported
    async fn import_manifest(&self, manifest_content: &str) -> Result<()>;

    /// Compute a hash of the current manifest for change detection
    async fn compute_manifest_hash(&self) -> Result<String> {
        let manifest = self.export_manifest().await?;
        use sha2::{Digest, Sha256};
        Ok(format!("{:x}", Sha256::digest(manifest.as_bytes())))
    }
}

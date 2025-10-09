use super::{PackageInfo, PackageManager};
use anyhow::Result;
use async_trait::async_trait;
use serde::Deserialize;
use tokio::process::Command;

#[derive(Debug, Deserialize)]
struct PnpmPackage {
    name: Option<String>,
    version: Option<String>,
}

pub struct PnpmManager;

impl PnpmManager {
    pub fn new() -> Self {
        Self
    }

    async fn run_pnpm(&self, args: &[&str]) -> Result<String> {
        let output = Command::new("pnpm").args(args).output().await?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(anyhow::anyhow!("pnpm command failed: {}", stderr));
        }

        Ok(String::from_utf8(output.stdout)?)
    }
}

impl Default for PnpmManager {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl PackageManager for PnpmManager {
    async fn list_installed(&self) -> Result<Vec<PackageInfo>> {
        let output = self
            .run_pnpm(&["list", "-g", "--depth=0", "--json"])
            .await?;

        let packages_data: Vec<PnpmPackage> = serde_json::from_str(&output)?;

        let mut packages = Vec::new();
        for pkg in packages_data {
            if let Some(name) = pkg.name {
                // Skip pnpm itself
                if name != "pnpm" {
                    packages.push(PackageInfo {
                        name,
                        version: pkg.version,
                    });
                }
            }
        }

        packages.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(packages)
    }

    async fn install(&self, package: &PackageInfo) -> Result<()> {
        let pkg_spec = if let Some(version) = &package.version {
            format!("{}@{}", package.name, version)
        } else {
            package.name.clone()
        };

        self.run_pnpm(&["add", "-g", &pkg_spec]).await?;
        Ok(())
    }

    async fn is_available(&self) -> bool {
        which::which("pnpm").is_ok()
    }

    fn name(&self) -> &str {
        "pnpm"
    }

    async fn export_manifest(&self) -> Result<String> {
        // Get list of installed packages
        let packages = self.list_installed().await?;

        // Create simple newline-delimited list of package names
        // Format: package_name (no versions, let pnpm install latest)
        let manifest = packages
            .iter()
            .map(|p| p.name.as_str())
            .collect::<Vec<_>>()
            .join("\n");

        Ok(manifest)
    }

    async fn import_manifest(&self, manifest_content: &str) -> Result<()> {
        // Parse package names from manifest
        let package_names: Vec<&str> = manifest_content
            .lines()
            .map(|line| line.trim())
            .filter(|line| !line.is_empty())
            .collect();

        if package_names.is_empty() {
            return Ok(()); // Nothing to install
        }

        // Get currently installed packages
        let installed = self.list_installed().await?;
        let installed_names: std::collections::HashSet<_> =
            installed.iter().map(|p| p.name.as_str()).collect();

        // Install missing packages
        for name in package_names {
            if !installed_names.contains(name) {
                // Install the package
                let output = Command::new("pnpm")
                    .args(["add", "-g", name])
                    .output()
                    .await?;

                if !output.status.success() {
                    let stderr = String::from_utf8_lossy(&output.stderr);
                    // Log warning but continue with other packages
                    eprintln!("Warning: Failed to install {}: {}", name, stderr);
                }
            }
        }

        Ok(())
    }
}

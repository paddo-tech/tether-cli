use super::{PackageInfo, PackageManager};
use anyhow::Result;
use async_trait::async_trait;
use serde::Deserialize;
use std::collections::HashMap;
use tokio::process::Command;

#[derive(Debug, Deserialize)]
struct NpmListOutput {
    dependencies: Option<HashMap<String, NpmPackage>>,
}

#[derive(Debug, Deserialize)]
struct NpmPackage {
    version: String,
}

pub struct NpmManager;

impl NpmManager {
    pub fn new() -> Self {
        Self
    }

    async fn run_npm(&self, args: &[&str]) -> Result<String> {
        let output = Command::new("npm").args(args).output().await?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(anyhow::anyhow!("npm command failed: {}", stderr));
        }

        Ok(String::from_utf8(output.stdout)?)
    }
}

impl Default for NpmManager {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl PackageManager for NpmManager {
    async fn list_installed(&self) -> Result<Vec<PackageInfo>> {
        let output = self.run_npm(&["list", "-g", "--depth=0", "--json"]).await?;

        let list: NpmListOutput = serde_json::from_str(&output)?;

        let mut packages = Vec::new();
        if let Some(deps) = list.dependencies {
            for (name, pkg) in deps {
                // Skip npm itself
                if name != "npm" {
                    packages.push(PackageInfo {
                        name,
                        version: Some(pkg.version),
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

        self.run_npm(&["install", "-g", &pkg_spec]).await?;
        Ok(())
    }

    async fn is_available(&self) -> bool {
        which::which("npm").is_ok()
    }

    fn name(&self) -> &str {
        "npm"
    }

    async fn export_manifest(&self) -> Result<String> {
        // Get list of installed packages
        let packages = self.list_installed().await?;

        // Create simple newline-delimited list of package names
        // Format: package_name (no versions, let npm install latest)
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
                let output = Command::new("npm")
                    .args(["install", "-g", name])
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

    async fn update_all(&self) -> Result<()> {
        let output = Command::new("npm").args(["update", "-g"]).output().await?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(anyhow::anyhow!("npm update failed: {}", stderr));
        }

        Ok(())
    }
}

use super::{PackageInfo, PackageManager};
use anyhow::Result;
use async_trait::async_trait;
use std::path::PathBuf;
use tokio::process::Command;

pub struct BrewManager;

impl BrewManager {
    pub fn new() -> Self {
        Self
    }

    async fn run_brew(&self, args: &[&str]) -> Result<String> {
        let output = Command::new("brew").args(args).output().await?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(anyhow::anyhow!("brew command failed: {}", stderr));
        }

        Ok(String::from_utf8(output.stdout)?)
    }

    /// Get a temporary file path for Brewfile operations
    fn temp_brewfile_path() -> Result<PathBuf> {
        let home =
            home::home_dir().ok_or_else(|| anyhow::anyhow!("Could not find home directory"))?;
        Ok(home.join(".tether").join("Brewfile.tmp"))
    }
}

impl Default for BrewManager {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl PackageManager for BrewManager {
    async fn list_installed(&self) -> Result<Vec<PackageInfo>> {
        let output = self.run_brew(&["list", "--formula", "-1"]).await?;

        let mut packages = Vec::new();
        for line in output.lines() {
            let name = line.trim();
            if !name.is_empty() {
                packages.push(PackageInfo {
                    name: name.to_string(),
                    version: None, // Version not needed with Brewfile approach
                });
            }
        }

        Ok(packages)
    }

    async fn install(&self, package: &PackageInfo) -> Result<()> {
        self.run_brew(&["install", &package.name]).await?;
        Ok(())
    }

    async fn is_available(&self) -> bool {
        which::which("brew").is_ok()
    }

    fn name(&self) -> &str {
        "brew"
    }

    async fn export_manifest(&self) -> Result<String> {
        // Use `brew bundle dump` to generate a Brewfile
        let temp_path = Self::temp_brewfile_path()?;

        // Ensure parent directory exists
        if let Some(parent) = temp_path.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }

        // Remove existing temp file if it exists
        if temp_path.exists() {
            tokio::fs::remove_file(&temp_path).await?;
        }

        // Generate Brewfile
        let output = Command::new("brew")
            .args([
                "bundle",
                "dump",
                "--file",
                temp_path
                    .to_str()
                    .ok_or_else(|| anyhow::anyhow!("Invalid path for Brewfile: {:?}", temp_path))?,
            ])
            .output()
            .await?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(anyhow::anyhow!("brew bundle dump failed: {}", stderr));
        }

        // Read the generated Brewfile
        let content = tokio::fs::read_to_string(&temp_path).await?;

        // Clean up temp file
        let _ = tokio::fs::remove_file(&temp_path).await;

        Ok(content)
    }

    async fn import_manifest(&self, manifest_content: &str) -> Result<()> {
        // Write manifest to temporary Brewfile
        let temp_path = Self::temp_brewfile_path()?;

        // Ensure parent directory exists
        if let Some(parent) = temp_path.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }

        tokio::fs::write(&temp_path, manifest_content).await?;

        // Use `brew bundle install` to install packages from Brewfile
        // --no-upgrade: don't upgrade existing packages (faster, less disruptive)
        let output = Command::new("brew")
            .args([
                "bundle",
                "install",
                "--no-upgrade",
                "--file",
                temp_path
                    .to_str()
                    .ok_or_else(|| anyhow::anyhow!("Invalid path for Brewfile: {:?}", temp_path))?,
            ])
            .env("HOMEBREW_NO_AUTO_UPDATE", "1")
            .env("NONINTERACTIVE", "1")
            .output()
            .await?;

        // Clean up temp file
        let _ = tokio::fs::remove_file(&temp_path).await;

        // brew bundle may return non-zero even if most packages installed
        // (e.g., one cask failed). Log but don't fail.
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            if !stderr.trim().is_empty() {
                eprintln!("Warning: brew bundle had issues: {}", stderr);
            }
        }

        Ok(())
    }

    async fn remove_unlisted(&self, manifest_content: &str) -> Result<()> {
        // Parse manifest to get desired packages
        let desired: std::collections::HashSet<&str> = manifest_content
            .lines()
            .filter_map(|line| {
                let line = line.trim();
                // Parse Brewfile format: brew "package" or cask "package"
                if line.starts_with("brew \"") || line.starts_with("cask \"") {
                    line.split('"').nth(1)
                } else {
                    None
                }
            })
            .collect();

        if desired.is_empty() {
            return Ok(());
        }

        // Get installed packages
        let installed = self.list_installed().await?;

        // Remove packages not in manifest
        for pkg in installed {
            if !desired.contains(pkg.name.as_str()) {
                let output = Command::new("brew")
                    .args(["uninstall", &pkg.name])
                    .output()
                    .await?;

                if !output.status.success() {
                    let stderr = String::from_utf8_lossy(&output.stderr);
                    eprintln!("Warning: Failed to uninstall {}: {}", pkg.name, stderr);
                }
            }
        }

        Ok(())
    }

    async fn update_all(&self) -> Result<()> {
        // Check if there are any packages to update
        let packages = self.list_installed().await?;
        if packages.is_empty() {
            return Ok(());
        }

        // Update Homebrew itself and upgrade all packages
        Command::new("brew").args(["update"]).output().await?;

        let output = Command::new("brew").args(["upgrade"]).output().await?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(anyhow::anyhow!("brew upgrade failed: {}", stderr));
        }

        Ok(())
    }
}

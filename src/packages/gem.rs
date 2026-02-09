use super::{PackageInfo, PackageManager};
use anyhow::Result;
use async_trait::async_trait;
use tokio::process::Command;

pub struct GemManager;

impl GemManager {
    pub fn new() -> Self {
        Self
    }

    async fn run_gem(&self, args: &[&str]) -> Result<String> {
        let output = Command::new(super::resolve_program("gem"))
            .args(args)
            .output()
            .await?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(anyhow::anyhow!("gem command failed: {}", stderr));
        }

        Ok(String::from_utf8(output.stdout)?)
    }
}

impl Default for GemManager {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl PackageManager for GemManager {
    async fn list_installed(&self) -> Result<Vec<PackageInfo>> {
        // List local gems (includes user-installed gems in ~/.gem)
        let output = self.run_gem(&["list", "--local", "--no-versions"]).await?;

        let mut packages = Vec::new();

        for line in output.lines() {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }

            // Skip system gems indicators
            if line.starts_with("***") || line.contains("LOCAL GEMS") {
                continue;
            }

            // Gem list format is just gem names, one per line
            packages.push(PackageInfo {
                name: line.to_string(),
                version: None,
            });
        }

        packages.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(packages)
    }

    async fn install(&self, package: &PackageInfo) -> Result<()> {
        let pkg_spec = if let Some(version) = &package.version {
            format!("{}:{}", package.name, version)
        } else {
            package.name.clone()
        };

        // Install to user directory (no sudo needed)
        // This automatically installs to ~/.gem when user doesn't have system write access
        self.run_gem(&["install", &pkg_spec, "--user-install"])
            .await?;
        Ok(())
    }

    async fn is_available(&self) -> bool {
        which::which("gem").is_ok()
    }

    fn name(&self) -> &str {
        "gem"
    }

    async fn update_all(&self) -> Result<()> {
        let packages = self.list_installed().await?;
        if packages.is_empty() {
            return Ok(());
        }

        let output = Command::new(super::resolve_program("gem"))
            .args(["update", "--user-install"])
            .output()
            .await?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(anyhow::anyhow!("gem update failed: {}", stderr));
        }

        Ok(())
    }

    async fn uninstall(&self, package: &str) -> Result<()> {
        let output = Command::new(super::resolve_program("gem"))
            .args(["uninstall", package, "-x", "-a"])
            .output()
            .await?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(anyhow::anyhow!("gem uninstall failed: {}", stderr));
        }

        Ok(())
    }

    async fn get_dependents(&self, package: &str) -> Result<Vec<String>> {
        // gem dependency -R shows reverse dependencies
        let output = Command::new(super::resolve_program("gem"))
            .args(["dependency", "-R", package])
            .output()
            .await?;

        if !output.status.success() {
            return Ok(vec![]);
        }

        let stdout = String::from_utf8_lossy(&output.stdout);
        let mut dependents = Vec::new();

        // Parse output - look for "Used by" section
        let mut in_used_by = false;
        for line in stdout.lines() {
            if line.contains("Used by") {
                in_used_by = true;
                continue;
            }
            if in_used_by {
                let trimmed = line.trim();
                if trimmed.is_empty() || !trimmed.starts_with(' ') {
                    break;
                }
                // Extract gem name (format: "  gemname-version")
                if let Some(name) = trimmed.split_whitespace().next() {
                    if !name.is_empty() {
                        dependents.push(name.to_string());
                    }
                }
            }
        }

        Ok(dependents)
    }
}

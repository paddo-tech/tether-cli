use super::{PackageInfo, PackageManager};
use anyhow::Result;
use async_trait::async_trait;
use tokio::process::Command;

pub struct UvManager;

impl UvManager {
    pub fn new() -> Self {
        Self
    }

    async fn run_uv(&self, args: &[&str]) -> Result<String> {
        let output = Command::new("uv").args(args).output().await?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(anyhow::anyhow!("uv command failed: {}", stderr));
        }

        Ok(String::from_utf8(output.stdout)?)
    }
}

impl Default for UvManager {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl PackageManager for UvManager {
    async fn list_installed(&self) -> Result<Vec<PackageInfo>> {
        let output = self.run_uv(&["tool", "list"]).await?;

        // Parse output format:
        // black v24.10.0
        //     - black
        //     - blackd
        // ruff v0.6.0
        //     - ruff
        let mut packages = Vec::new();
        for line in output.lines() {
            // Tool names are on lines that don't start with whitespace or '-'
            // (lines starting with '-' are tool metadata, e.g. "- git-fame")
            if !line.starts_with(' ')
                && !line.starts_with('\t')
                && !line.starts_with('-')
                && !line.is_empty()
            {
                // Parse "toolname vX.Y.Z" - first token is name
                let name = line.split_whitespace().next().unwrap_or("").to_string();
                if !name.is_empty() {
                    let version = line
                        .split_whitespace()
                        .nth(1)
                        .map(|v| v.trim_start_matches('v').to_string());
                    packages.push(PackageInfo { name, version });
                }
            }
        }

        packages.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(packages)
    }

    async fn install(&self, package: &PackageInfo) -> Result<()> {
        self.run_uv(&["tool", "install", &package.name]).await?;
        Ok(())
    }

    async fn is_available(&self) -> bool {
        which::which("uv").is_ok()
    }

    fn name(&self) -> &str {
        "uv"
    }

    async fn export_manifest(&self) -> Result<String> {
        let packages = self.list_installed().await?;

        let manifest = packages
            .iter()
            .map(|p| p.name.as_str())
            .collect::<Vec<_>>()
            .join("\n");

        Ok(manifest)
    }

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
        let installed_names: std::collections::HashSet<_> =
            installed.iter().map(|p| p.name.as_str()).collect();

        for name in package_names {
            if !installed_names.contains(name) {
                let output = Command::new("uv")
                    .args(["tool", "install", name])
                    .output()
                    .await?;

                if !output.status.success() {
                    let stderr = String::from_utf8_lossy(&output.stderr);
                    eprintln!("Warning: Failed to install {}: {}", name, stderr);
                }
            }
        }

        Ok(())
    }

    async fn remove_unlisted(&self, manifest_content: &str) -> Result<()> {
        let desired: std::collections::HashSet<&str> = manifest_content
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
                let output = Command::new("uv")
                    .args(["tool", "uninstall", &pkg.name])
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
        let packages = self.list_installed().await?;
        if packages.is_empty() {
            return Ok(());
        }

        let output = Command::new("uv")
            .args(["tool", "upgrade", "--all"])
            .output()
            .await?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(anyhow::anyhow!("uv tool upgrade failed: {}", stderr));
        }

        Ok(())
    }
}

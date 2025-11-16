use super::{PackageInfo, PackageManager};
use anyhow::Result;
use async_trait::async_trait;
use tokio::process::Command;

pub struct BunManager;

impl BunManager {
    pub fn new() -> Self {
        Self
    }

    async fn run_bun(&self, args: &[&str]) -> Result<String> {
        let output = Command::new("bun").args(args).output().await?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(anyhow::anyhow!("bun command failed: {}", stderr));
        }

        Ok(String::from_utf8(output.stdout)?)
    }
}

impl Default for BunManager {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl PackageManager for BunManager {
    async fn list_installed(&self) -> Result<Vec<PackageInfo>> {
        let output = match self.run_bun(&["pm", "ls", "-g"]).await {
            Ok(out) => out,
            Err(err) => {
                let message = err.to_string();
                if message.contains("No package.json was found") {
                    // Bun hasn't created the global install metadata yet.
                    // Treat this as "no global packages" instead of failing the sync.
                    return Ok(Vec::new());
                }
                return Err(err);
            }
        };

        let mut packages = Vec::new();

        // Parse output - bun pm ls -g returns package names one per line
        for line in output.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with("bun") {
                continue;
            }

            // Extract package name (may include version like "package@1.2.3")
            let name = if let Some(idx) = line.find('@') {
                if idx > 0 {
                    line[..idx].to_string()
                } else {
                    continue;
                }
            } else {
                line.to_string()
            };

            packages.push(PackageInfo {
                name,
                version: None,
            });
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

        self.run_bun(&["add", "-g", &pkg_spec]).await?;
        Ok(())
    }

    async fn is_available(&self) -> bool {
        which::which("bun").is_ok()
    }

    fn name(&self) -> &str {
        "bun"
    }

    async fn export_manifest(&self) -> Result<String> {
        // Get list of installed packages
        let packages = self.list_installed().await?;

        // Create simple newline-delimited list of package names
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
                let output = Command::new("bun")
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

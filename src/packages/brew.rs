use super::{PackageInfo, PackageManager};
use anyhow::Result;
use async_trait::async_trait;
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
                // Get version for this package
                let version = match self.run_brew(&["info", "--json=v2", name]).await {
                    Ok(json_str) => {
                        // Parse version from info
                        if let Ok(data) = serde_json::from_str::<serde_json::Value>(&json_str) {
                            data.get("formulae")
                                .and_then(|f| f.get(0))
                                .and_then(|p| p.get("installed"))
                                .and_then(|i| i.get(0))
                                .and_then(|v| v.get("version"))
                                .and_then(|v| v.as_str())
                                .map(|s| s.to_string())
                        } else {
                            None
                        }
                    }
                    Err(_) => None,
                };

                packages.push(PackageInfo {
                    name: name.to_string(),
                    version,
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
}

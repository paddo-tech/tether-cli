use super::policy::{self, PackagePolicy};
use super::{validate_name, validate_version, Cooldown, Ecosystem, PackageInfo, PackageManager};
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

    fn policy(&self) -> PackagePolicy {
        PackagePolicy::load()
    }

    async fn version(&self) -> Option<((u64, u64, u64), bool)> {
        policy::tool_version("npm").await
    }

    /// An old npm runs without cooldown args after one warning.
    async fn cooldown_args(&self) -> Vec<String> {
        let cooldown = self.cooldown().await;
        if cooldown == Cooldown::Unsupported {
            policy::warn_unsupported_once("npm", self.policy().min_release_age_days);
        }
        cooldown.args().to_vec()
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
        validate_name(Ecosystem::Npm, &package.name)?;
        let pkg_spec = if let Some(version) = &package.version {
            validate_version(version)?;
            format!("{}@{}", package.name, version)
        } else {
            package.name.clone()
        };

        let major = self.version().await.map_or(0, |((major, _, _), _)| major);
        let mut args = vec!["install".to_string(), "-g".to_string()];
        args.extend(self.cooldown_args().await);
        args.extend(policy::npm_script_args(
            &self.policy(),
            &package.name,
            major,
        ));
        args.push(pkg_spec);
        let args: Vec<&str> = args.iter().map(String::as_str).collect();
        self.run_npm(&args).await?;
        Ok(())
    }

    async fn is_available(&self) -> bool {
        which::which("npm").is_ok()
    }

    fn name(&self) -> &str {
        "npm"
    }

    fn ecosystem(&self) -> Ecosystem {
        Ecosystem::Npm
    }

    async fn cooldown(&self) -> Cooldown {
        policy::npm_cooldown(self.policy().min_release_age_days, self.version().await)
    }

    async fn update_all(&self) -> Result<()> {
        let packages = self.list_installed().await?;
        if packages.is_empty() {
            return Ok(());
        }

        let cooldown = self.cooldown_args().await;
        let package_policy = self.policy();
        let major = self.version().await.map_or(0, |((major, _, _), _)| major);

        let names: Vec<String> = packages
            .into_iter()
            .map(|p| p.name)
            .filter(|name| match validate_name(Ecosystem::Npm, name) {
                Ok(()) => true,
                Err(e) => {
                    eprintln!("Warning: Skipping npm entry: {}", e);
                    false
                }
            })
            .collect();
        let (scripted, plain): (Vec<String>, Vec<String>) = names
            .into_iter()
            .partition(|name| package_policy.scripts_allowed(name));

        // Allowlisted packages update in a second run so only they get scripts
        for batch in [plain, scripted] {
            if batch.is_empty() {
                continue;
            }
            let mut script_args: Vec<String> = batch
                .iter()
                .flat_map(|name| policy::npm_script_args(&package_policy, name, major))
                .collect();
            script_args.dedup();
            let output = Command::new("npm")
                .args(["update", "-g"])
                .args(&cooldown)
                .args(&script_args)
                .args(&batch)
                .output()
                .await?;

            if !output.status.success() {
                let stderr = String::from_utf8_lossy(&output.stderr);
                return Err(anyhow::anyhow!("npm update failed: {}", stderr));
            }
        }

        Ok(())
    }

    async fn uninstall(&self, package: &str) -> Result<()> {
        validate_name(Ecosystem::Npm, package)?;
        let output = Command::new("npm")
            .args(["uninstall", "-g", package])
            .output()
            .await?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(anyhow::anyhow!("npm uninstall failed: {}", stderr));
        }

        Ok(())
    }
}

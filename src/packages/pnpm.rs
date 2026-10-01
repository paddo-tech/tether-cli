use super::command;
use super::policy::{self, PackagePolicy};
use super::{
    command_error_message, validate_name, validate_version, Cooldown, Ecosystem, PackageInfo,
    PackageManager,
};
use anyhow::Result;
use async_trait::async_trait;
use serde_json::Value;

pub struct PnpmManager;

impl PnpmManager {
    pub fn new() -> Self {
        Self
    }

    fn policy(&self) -> PackagePolicy {
        PackagePolicy::load()
    }

    /// An old pnpm runs without cooldown args after one warning.
    async fn cooldown_args(&self) -> Vec<String> {
        let cooldown = self.cooldown().await;
        if cooldown == Cooldown::Unsupported {
            policy::warn_unsupported_once("pnpm", self.policy().min_release_age_days);
        }
        cooldown.args().to_vec()
    }

    async fn run_pnpm(&self, args: &[&str]) -> Result<String> {
        let output = command("pnpm")?.args(args).output().await?;

        if !output.status.success() {
            return Err(anyhow::anyhow!(
                "pnpm command failed: {}",
                command_error_message(&output)
            ));
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

        let json: Value = serde_json::from_str(&output)?;
        let mut packages = Vec::new();

        if let Value::Array(entries) = json {
            for entry in entries {
                if let Some(deps) = entry.get("dependencies").and_then(Value::as_object) {
                    for (name, dep_info) in deps {
                        if name == "pnpm" {
                            continue;
                        }

                        let version = dep_info
                            .get("version")
                            .and_then(Value::as_str)
                            .map(|s| s.to_string());

                        packages.push(PackageInfo {
                            name: name.to_string(),
                            version,
                        });
                    }
                }
            }
        }

        packages.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(packages)
    }

    async fn install(&self, package: &PackageInfo) -> Result<()> {
        validate_name(Ecosystem::Npm, &package.name)?;
        let pkg_spec = if let Some(version) = &package.version {
            validate_version(Ecosystem::Npm, version)?;
            format!("{}@{}", package.name, version)
        } else {
            package.name.clone()
        };

        let package_policy = self.policy();
        let version = policy::tool_version("pnpm").await;
        if package_policy.scripts_allowed(&package.name) && !policy::pnpm_can_allow_build(version) {
            policy::warn_scripts_unsupported_once("pnpm", "10.4");
        }
        let mut args = vec!["add".to_string(), "-g".to_string()];
        args.extend(self.cooldown_args().await);
        args.extend(policy::pnpm_script_args(
            &package_policy,
            &package.name,
            version,
            true,
        ));
        args.push(pkg_spec);
        let args: Vec<&str> = args.iter().map(String::as_str).collect();
        self.run_pnpm(&args).await?;
        Ok(())
    }

    async fn is_available(&self) -> bool {
        which::which("pnpm").is_ok()
    }

    fn name(&self) -> &str {
        "pnpm"
    }

    fn ecosystem(&self) -> Ecosystem {
        Ecosystem::Npm
    }

    async fn cooldown(&self) -> Cooldown {
        policy::pnpm_cooldown(
            self.policy().min_release_age_days,
            policy::tool_version("pnpm").await,
        )
    }

    async fn update_all(&self) -> Result<()> {
        let packages = self.list_installed().await?;
        if packages.is_empty() {
            return Ok(());
        }

        let cooldown = self.cooldown_args().await;
        let package_policy = self.policy();
        let version = policy::tool_version("pnpm").await;
        let held = super::inbox::hold_malicious_upgrades(self).await;
        let names: Vec<String> = packages
            .into_iter()
            .map(|p| p.name)
            .filter(|name| !held.contains(name))
            .filter(|name| match validate_name(Ecosystem::Npm, name) {
                Ok(()) => true,
                Err(e) => {
                    eprintln!("Warning: Skipping pnpm entry: {}", e);
                    false
                }
            })
            .collect();
        let (scripted, plain): (Vec<String>, Vec<String>) = names
            .into_iter()
            .partition(|name| package_policy.scripts_allowed(name));
        if !scripted.is_empty() && !policy::pnpm_can_allow_build(version) {
            policy::warn_scripts_unsupported_once("pnpm", "10.4");
        }

        // Allowlisted packages update in a second run so only they get scripts
        for batch in [plain, scripted] {
            let Some(first) = batch.first() else {
                continue;
            };
            let script_args = policy::pnpm_script_args(&package_policy, first, version, false);
            if script_args.iter().any(|a| a == "--ignore-scripts")
                && !policy::pnpm_update_accepts_ignore_scripts(version)
            {
                if policy::first_warning("pnpm update --ignore-scripts") {
                    eprintln!(
                        "Warning: Skipping pnpm update: pnpm 12.0.0 to 12.3.1 cannot update with install scripts off. Upgrade pnpm to 12.3.2 or later"
                    );
                }
                continue;
            }
            let output = command("pnpm")?
                .args(["update", "-g"])
                .args(&cooldown)
                .args(script_args)
                .args(&batch)
                .output()
                .await?;

            if !output.status.success() {
                return Err(anyhow::anyhow!(
                    "pnpm update failed: {}",
                    command_error_message(&output)
                ));
            }
        }

        Ok(())
    }

    /// `wanted` is what `pnpm update -g` installs; pnpm applies the release-age limit to it.
    async fn upgrade_candidates(&self) -> Result<Vec<(String, String)>> {
        let output = command("pnpm")?
            .args(["outdated", "-g", "--format", "json"])
            .args(self.cooldown_args().await)
            .output()
            .await?;
        // pnpm exits 1 when something is outdated, so the JSON decides
        super::npm::parse_outdated_json(&output.stdout)
    }

    async fn uninstall(&self, package: &str) -> Result<()> {
        validate_name(Ecosystem::Npm, package)?;
        let output = command("pnpm")?
            .args(["remove", "-g", package])
            .output()
            .await?;

        if !output.status.success() {
            return Err(anyhow::anyhow!(
                "pnpm remove failed: {}",
                command_error_message(&output)
            ));
        }

        Ok(())
    }
}

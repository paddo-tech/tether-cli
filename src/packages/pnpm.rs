use super::command;
use super::policy::{self, PackagePolicy};
use super::{
    command_error_message, validate_name, validate_version, Cooldown, Ecosystem, PackageInfo,
    PackageManager, Upgrade,
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

    /// Adds each planned target exactly. `pnpm update --latest` would pick the newest mature
    /// release, which can be older than the installed one.
    async fn update_all(&self) -> Result<()> {
        let upgrades = super::planned_upgrades(self).await?;
        if upgrades.is_empty() {
            return Ok(());
        }

        let cooldown = self.cooldown_args().await;
        let package_policy = self.policy();
        let version = policy::tool_version("pnpm").await;
        let upgrades: Vec<Upgrade> = upgrades
            .into_iter()
            .filter(|u| {
                match validate_name(Ecosystem::Npm, &u.name)
                    .and_then(|()| validate_version(Ecosystem::Npm, &u.target))
                {
                    Ok(()) => true,
                    Err(e) => {
                        crate::cli::Output::warning(&format!("Skipping pnpm entry: {}", e));
                        false
                    }
                }
            })
            .collect();
        let (scripted, plain): (Vec<Upgrade>, Vec<Upgrade>) = upgrades
            .into_iter()
            .partition(|u| package_policy.scripts_allowed(&u.name));
        if !scripted.is_empty() && !policy::pnpm_can_allow_build(version) {
            policy::warn_scripts_unsupported_once("pnpm", "10.4");
        }

        // Allowlisted packages update in a second run so only they get scripts
        for batch in [plain, scripted] {
            let Some(first) = batch.first() else {
                continue;
            };
            // An allowlisted package's builds were approved when it was added, so it needs
            // no flag here
            let script_args =
                policy::pnpm_script_args(&package_policy, &first.name, version, false);
            let specs: Vec<String> = batch
                .iter()
                .map(|u| format!("{}@{}", u.name, u.target))
                .collect();
            let output = command("pnpm")?
                .args(["add", "-g"])
                .args(&cooldown)
                .args(script_args)
                .args(&specs)
                .output()
                .await?;

            if !output.status.success() {
                return Err(anyhow::anyhow!(
                    "pnpm add failed: {}",
                    command_error_message(&output)
                ));
            }
        }

        Ok(())
    }

    /// `pnpm add -g name@1.2.3` saves that exact version as the range, so `wanted` never
    /// moves and the target is `latest`, to which pnpm applies the release-age limit.
    async fn upgrade_candidates(&self) -> Result<Vec<Upgrade>> {
        let output = command("pnpm")?
            .args(["outdated", "-g", "--format", "json"])
            .args(self.cooldown_args().await)
            .output()
            .await?;
        // pnpm exits 1 when something is outdated, so the JSON decides. pnpm itself is not
        // a synced package, as in `list_installed`.
        let mut candidates = super::npm::parse_outdated_json(&output.stdout, true)?;
        candidates.retain(|u| u.name != "pnpm");
        Ok(candidates)
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

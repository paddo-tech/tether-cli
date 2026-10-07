use super::command;
use super::policy::{self, PackagePolicy};
use super::{
    validate_name, validate_version, Cooldown, Ecosystem, Hold, PackageInfo, PackageManager,
    Upgrade,
};
use anyhow::Result;
use async_trait::async_trait;
use serde::Deserialize;
use std::collections::HashMap;

#[derive(Debug, Deserialize)]
struct NpmListOutput {
    dependencies: Option<HashMap<String, NpmPackage>>,
}

#[derive(Debug, Deserialize)]
struct NpmPackage {
    version: String,
}

#[derive(Debug, Deserialize)]
struct OutdatedEntry {
    current: Option<String>,
    wanted: Option<String>,
    latest: Option<String>,
}

/// Packages from `npm outdated --json` or `pnpm outdated --format json` whose target differs
/// from `current`. The target is `latest` when the upgrade ignores the saved range, else
/// `wanted`. With a release-age limit the target can be older than `current`.
pub(super) fn parse_outdated_json(stdout: &[u8], latest: bool) -> Result<Vec<Upgrade>> {
    let value: serde_json::Value = serde_json::from_slice(stdout)?;
    if let Some(summary) = value.get("error").and_then(|e| e.get("summary")) {
        anyhow::bail!("{}", summary.as_str().unwrap_or("outdated check failed"));
    }
    let entries: HashMap<String, OutdatedEntry> = serde_json::from_value(value)?;
    let mut candidates: Vec<Upgrade> = entries
        .into_iter()
        .filter_map(|(name, entry)| {
            let target = if latest { entry.latest } else { entry.wanted }?;
            (entry.current.as_ref() != Some(&target))
                .then(|| Upgrade::new(&name, entry.current.as_deref(), &target))
        })
        .collect();
    candidates.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(candidates)
}

/// npm fails a whole `outdated` run with `ENOVERSIONS` when one package has no release
/// older than the release-age limit.
fn no_mature_release(stdout: &[u8]) -> bool {
    serde_json::from_slice::<serde_json::Value>(stdout)
        .is_ok_and(|v| v["error"]["code"] == "ENOVERSIONS")
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
        let output = command("npm")?.args(args).output().await?;

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
            validate_version(Ecosystem::Npm, version)?;
            format!("{}@{}", package.name, version)
        } else {
            package.name.clone()
        };

        let major = self.version().await.map_or(0, |((major, _, _), _)| major);
        let package_policy = self.policy();
        if package_policy.scripts_allowed(&package.name) && major < 12 {
            policy::warn_scripts_unsupported_once("npm", "12");
        }
        let mut args = vec!["install".to_string(), "-g".to_string()];
        args.extend(self.cooldown_args().await);
        args.extend(policy::npm_script_args(
            &package_policy,
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

    /// Installs each planned target exactly. `npm update -g` would install `wanted`, which
    /// the release-age limit can set below the installed version.
    async fn update_all(&self) -> Result<()> {
        let upgrades = super::planned_upgrades(self).await?;
        if upgrades.is_empty() {
            return Ok(());
        }

        let cooldown = self.cooldown_args().await;
        let package_policy = self.policy();
        let major = self.version().await.map_or(0, |((major, _, _), _)| major);

        let upgrades: Vec<Upgrade> = upgrades
            .into_iter()
            .filter(|u| {
                match validate_name(Ecosystem::Npm, &u.name)
                    .and_then(|()| validate_version(Ecosystem::Npm, &u.target))
                {
                    Ok(()) => true,
                    Err(e) => {
                        crate::cli::Output::warning(&format!("Skipping npm entry: {}", e));
                        false
                    }
                }
            })
            .collect();
        let (scripted, plain): (Vec<Upgrade>, Vec<Upgrade>) = upgrades
            .into_iter()
            .partition(|u| package_policy.scripts_allowed(&u.name));
        if !scripted.is_empty() && major < 12 {
            policy::warn_scripts_unsupported_once("npm", "12");
        }

        // Allowlisted packages update in a second run so only they get scripts
        for batch in [plain, scripted] {
            if batch.is_empty() {
                continue;
            }
            let mut script_args: Vec<String> = batch
                .iter()
                .flat_map(|u| policy::npm_script_args(&package_policy, &u.name, major))
                .collect();
            script_args.dedup();
            let specs: Vec<String> = batch
                .iter()
                .map(|u| format!("{}@{}", u.name, u.target))
                .collect();
            let output = command("npm")?
                .args(["install", "-g"])
                .args(&cooldown)
                .args(&script_args)
                .args(&specs)
                .output()
                .await?;

            if !output.status.success() {
                let stderr = String::from_utf8_lossy(&output.stderr);
                return Err(anyhow::anyhow!("npm update failed: {}", stderr));
            }
        }

        Ok(())
    }

    /// npm applies the release-age limit to `wanted`. When one package has no release old
    /// enough, npm fails the whole check, so each package is then checked on its own and
    /// that package is held.
    async fn upgrade_candidates(&self) -> Result<Vec<Upgrade>> {
        let cooldown = self.cooldown_args().await;
        let output = command("npm")?
            .args(["outdated", "-g", "--json"])
            .args(&cooldown)
            .output()
            .await?;
        // npm exits 1 when something is outdated, so the JSON decides. npm itself is not a
        // synced package, as in `list_installed`.
        let mut candidates = if no_mature_release(&output.stdout) {
            let mut candidates = Vec::new();
            for package in self.list_installed().await? {
                validate_name(Ecosystem::Npm, &package.name)?;
                let output = command("npm")?
                    .args(["outdated", "-g", "--json"])
                    .args(&cooldown)
                    .arg(&package.name)
                    .output()
                    .await?;
                if no_mature_release(&output.stdout) {
                    candidates.push(Upgrade::held(
                        &package.name,
                        package.version.as_deref(),
                        Hold::NoMatureRelease,
                    ));
                } else {
                    candidates.extend(parse_outdated_json(&output.stdout, false)?);
                }
            }
            candidates
        } else {
            parse_outdated_json(&output.stdout, false)?
        };
        candidates.retain(|u| u.name != "npm");
        Ok(candidates)
    }

    async fn uninstall(&self, package: &str) -> Result<()> {
        validate_name(Ecosystem::Npm, package)?;
        let output = command("npm")?
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn outdated_json_yields_changed_wanted_versions() {
        let stdout = br#"{
            "cowsay": {"current": "1.4.0", "wanted": "1.6.0", "latest": "1.6.0"},
            "@scope/same": {"current": "2.0.0", "wanted": "2.0.0", "latest": "3.0.0"},
            "missing": {"wanted": "1.0.0", "latest": "1.0.0"}
        }"#;
        assert_eq!(
            parse_outdated_json(stdout, false).unwrap(),
            vec![
                Upgrade::new("cowsay", Some("1.4.0"), "1.6.0"),
                Upgrade::new("missing", None, "1.0.0")
            ]
        );
        // pnpm saves an exact pin, so `wanted` stays at `current` and only `latest` moves
        assert_eq!(
            parse_outdated_json(stdout, true).unwrap(),
            vec![
                Upgrade::new("@scope/same", Some("2.0.0"), "3.0.0"),
                Upgrade::new("cowsay", Some("1.4.0"), "1.6.0"),
                Upgrade::new("missing", None, "1.0.0")
            ]
        );
        assert!(parse_outdated_json(b"{}", false).unwrap().is_empty());
    }

    #[test]
    fn a_release_age_target_below_current_never_moves_forward() {
        // npm outdated with --min-release-age reports the newest mature version as `wanted`
        let stdout = br#"{
            "@google/gemini-cli": {"current": "0.63.0", "wanted": "0.62.0", "latest": "0.63.0"}
        }"#;
        let candidates = parse_outdated_json(stdout, false).unwrap();
        assert_eq!(candidates.len(), 1);
        assert!(candidates[0].is_downgrade());
        assert!(!candidates[0].moves_forward());
    }

    #[test]
    fn outdated_json_error_is_an_error() {
        let stdout =
            br#"{"error": {"code": "ENOVERSIONS", "summary": "No versions", "detail": ""}}"#;
        assert!(parse_outdated_json(stdout, false).is_err());
        assert!(no_mature_release(stdout));
        assert!(!no_mature_release(br#"{"error": {"code": "E404"}}"#));
        assert!(!no_mature_release(b"{}"));
    }
}

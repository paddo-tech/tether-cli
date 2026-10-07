use super::command;
use super::policy::{self, PackagePolicy};
use super::{
    validate_name, validate_version, Cooldown, Ecosystem, PackageInfo, PackageManager, Upgrade,
};
use anyhow::Result;
use async_trait::async_trait;

/// Tools from `uv tool list --outdated` lines like `ruff v0.6.0 [latest: 0.7.1]`.
fn parse_outdated(stdout: &str) -> Vec<Upgrade> {
    stdout
        .lines()
        .filter_map(|line| {
            let (head, latest) = line.split_once("[latest: ")?;
            let mut words = head.split_whitespace();
            let name = words.next()?;
            let current = words.next().map(|v| v.trim_start_matches('v'));
            Some(Upgrade::new(
                name,
                current,
                latest.trim_end().trim_end_matches(']'),
            ))
        })
        .collect()
}

pub struct UvManager;

impl UvManager {
    pub fn new() -> Self {
        Self
    }

    fn policy(&self) -> PackagePolicy {
        PackagePolicy::load()
    }

    async fn run_uv(&self, args: &[&str]) -> Result<String> {
        let output = command("uv")?.args(args).output().await?;

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
        validate_name(Ecosystem::Python, &package.name)?;
        let pkg_spec = match &package.version {
            Some(version) => {
                validate_version(Ecosystem::Python, version)?;
                format!("{}=={}", package.name, version)
            }
            None => package.name.clone(),
        };
        let cooldown = self.cooldown().await;
        let mut args = vec!["tool", "install"];
        args.extend(cooldown.args().iter().map(String::as_str));
        args.push(&pkg_spec);
        self.run_uv(&args).await?;
        // uv saves `==version` in the tool receipt, and `uv tool upgrade` never moves past it.
        // Installing the bare name again replaces that requirement. The installed version
        // satisfies it, so uv keeps it, and `--offline` stops uv from fetching another release
        // than the one OSV checked. uv does not save `--offline` in the receipt.
        if package.version.is_some() {
            args.pop();
            args.push("--offline");
            args.push(&package.name);
            self.run_uv(&args).await?;
        }
        Ok(())
    }

    async fn is_available(&self) -> bool {
        which::which("uv").is_ok()
    }

    fn name(&self) -> &str {
        "uv"
    }

    fn ecosystem(&self) -> Ecosystem {
        Ecosystem::Python
    }

    async fn cooldown(&self) -> Cooldown {
        policy::uv_cooldown(
            self.policy().min_release_age_days,
            policy::tool_version("uv").await,
            chrono::Utc::now(),
        )
    }

    /// Upgrades only the tools whose newest allowed release is newer than the installed one,
    /// since `uv tool upgrade --exclude-newer` can resolve an older release.
    async fn update_all(&self) -> Result<()> {
        let names: Vec<String> = super::planned_upgrades(self)
            .await?
            .into_iter()
            .map(|u| u.name)
            .filter(|name| validate_name(Ecosystem::Python, name).is_ok())
            .collect();
        if names.is_empty() {
            return Ok(());
        }
        let output = command("uv")?
            .args(["tool", "upgrade"])
            .args(names)
            .args(self.cooldown().await.args())
            .output()
            .await?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(anyhow::anyhow!("uv tool upgrade failed: {}", stderr));
        }

        Ok(())
    }

    async fn upgrade_candidates(&self) -> Result<Vec<Upgrade>> {
        let mut args = vec!["tool", "list", "--outdated"];
        let cooldown = self.cooldown().await;
        args.extend(cooldown.args().iter().map(String::as_str));
        Ok(parse_outdated(&self.run_uv(&args).await?))
    }

    async fn uninstall(&self, package: &str) -> Result<()> {
        validate_name(Ecosystem::Python, package)?;
        let output = command("uv")?
            .args(["tool", "uninstall", package])
            .output()
            .await?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(anyhow::anyhow!("uv tool uninstall failed: {}", stderr));
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn outdated_lines_yield_latest_versions() {
        let stdout = "cowsay v5.0 [latest: 6.1]\n- cowsay\nruff v0.6.0 [latest: 0.7.1]\n- ruff\n";
        assert_eq!(
            parse_outdated(stdout),
            vec![
                Upgrade::new("cowsay", Some("5.0"), "6.1"),
                Upgrade::new("ruff", Some("0.6.0"), "0.7.1")
            ]
        );
        assert!(parse_outdated("").is_empty());
    }
}

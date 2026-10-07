use super::command;
use super::policy::{self, PackagePolicy};
use super::{
    validate_name, validate_version, Cooldown, Ecosystem, PackageInfo, PackageManager, Upgrade,
};
use anyhow::Result;
use async_trait::async_trait;

/// Parse a package@version string, handling scoped packages like @scope/pkg@version
fn parse_package_version(s: &str) -> (String, Option<String>) {
    if let Some(last_at) = s.rfind('@') {
        // Check it's not the @ in a scoped package name (e.g., @google/pkg)
        if last_at > 0 && !s[..last_at].ends_with('/') {
            return (s[..last_at].to_string(), Some(s[last_at + 1..].to_string()));
        }
    }
    (s.to_string(), None)
}

/// Name and `Latest` version from the `bun outdated` table, which has no JSON form, for
/// each package whose `Latest` differs from `Current`. `bun add -g` installs `Latest`.
/// bun marks a version that the release-age limit held back with ` *`.
fn parse_outdated_table(stdout: &str) -> Vec<Upgrade> {
    stdout
        .lines()
        .filter_map(|line| {
            let cells: Vec<&str> = line.split('|').map(str::trim).collect();
            // A row is "| Package | Current | Update | Latest |"
            let [_, name, current, _, latest, _] = cells.as_slice() else {
                return None;
            };
            let name = name.split_whitespace().next()?;
            // The `|---|---|` separator row splits into cells too
            if name.starts_with('-') {
                return None;
            }
            let latest = latest.trim_end_matches('*').trim();
            (name != "Package" && !latest.is_empty() && latest != *current)
                .then(|| Upgrade::new(name, Some(current), latest))
        })
        .collect()
}

pub struct BunManager;

impl BunManager {
    pub fn new() -> Self {
        Self
    }

    fn policy(&self) -> PackagePolicy {
        PackagePolicy::load()
    }

    /// An old bun runs without cooldown args after one warning.
    async fn cooldown_args(&self) -> Vec<String> {
        let cooldown = self.cooldown().await;
        if cooldown == Cooldown::Unsupported {
            policy::warn_unsupported_once("bun", self.policy().min_release_age_days);
        }
        cooldown.args().to_vec()
    }

    async fn run_bun(&self, args: &[&str]) -> Result<String> {
        let output = command("bun")?.args(args).output().await?;

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

        // Parse tree output from `bun pm ls -g`:
        // /Users/paddo/.bun/install/global node_modules (535)
        // └── @google/gemini-cli@0.18.4
        for line in output.lines() {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }

            // Skip header line (contains "node_modules")
            if line.contains("node_modules") {
                continue;
            }

            // Remove tree prefixes (├── └── │)
            let cleaned = line
                .trim_start_matches("├──")
                .trim_start_matches("└──")
                .trim_start_matches("│")
                .trim();

            if cleaned.is_empty() {
                continue;
            }

            // Parse package@version format
            // Handle scoped packages like @google/gemini-cli@0.18.4
            let (name, version) = parse_package_version(cleaned);

            // Skip invalid entries (e.g. bare "@" from malformed output)
            if name.is_empty() || name == "@" {
                continue;
            }

            packages.push(PackageInfo { name, version });
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

        let mut args = vec!["add".to_string(), "-g".to_string()];
        args.extend(self.cooldown_args().await);
        args.extend(policy::bun_script_args(&self.policy(), &package.name));
        args.push(pkg_spec);
        let args: Vec<&str> = args.iter().map(String::as_str).collect();
        self.run_bun(&args).await?;
        Ok(())
    }

    async fn is_available(&self) -> bool {
        which::which("bun").is_ok()
    }

    fn name(&self) -> &str {
        "bun"
    }

    fn ecosystem(&self) -> Ecosystem {
        Ecosystem::Npm
    }

    async fn cooldown(&self) -> Cooldown {
        policy::bun_cooldown(
            self.policy().min_release_age_days,
            policy::tool_version("bun").await,
        )
    }

    /// `bun update -g` updates only the first package, so each planned target is added
    /// exactly. A plain `bun add -g name` would pick the newest mature release, which can
    /// be older than the installed one.
    async fn upgrade(&self, planned: &[Upgrade]) -> Result<()> {
        let cooldown = self.cooldown_args().await;
        let package_policy = self.policy();

        for upgrade in planned {
            if let Err(e) = validate_name(Ecosystem::Npm, &upgrade.name)
                .and_then(|()| validate_version(Ecosystem::Npm, &upgrade.target))
            {
                crate::cli::Output::warning(&format!("Skipping bun entry: {}", e));
                continue;
            }
            let output = command("bun")?
                .args(["add", "-g"])
                .args(&cooldown)
                .args(policy::bun_script_args(&package_policy, &upgrade.name))
                .arg(format!("{}@{}", upgrade.name, upgrade.target))
                .output()
                .await?;

            if !output.status.success() {
                let stderr = String::from_utf8_lossy(&output.stderr);
                crate::cli::Output::warning(&format!(
                    "Failed to update {}: {}",
                    upgrade.name, stderr
                ));
            }
        }

        Ok(())
    }

    async fn upgrade_candidates(&self) -> Result<Vec<Upgrade>> {
        let output = command("bun")?
            .args(["outdated", "-g"])
            .args(self.cooldown_args().await)
            .output()
            .await?;
        if !output.status.success() {
            anyhow::bail!(
                "bun outdated failed: {}",
                super::command_error_message(&output)
            );
        }
        Ok(parse_outdated_table(&String::from_utf8_lossy(
            &output.stdout,
        )))
    }

    async fn uninstall(&self, package: &str) -> Result<()> {
        validate_name(Ecosystem::Npm, package)?;
        let output = command("bun")?
            .args(["remove", "-g", package])
            .output()
            .await?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(anyhow::anyhow!("bun remove failed: {}", stderr));
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_package_version_simple() {
        let (name, version) = parse_package_version("typescript@5.3.3");
        assert_eq!(name, "typescript");
        assert_eq!(version, Some("5.3.3".to_string()));
    }

    #[test]
    fn test_parse_package_version_scoped() {
        let (name, version) = parse_package_version("@google/gemini-cli@0.18.4");
        assert_eq!(name, "@google/gemini-cli");
        assert_eq!(version, Some("0.18.4".to_string()));
    }

    #[test]
    fn test_parse_package_version_scoped_deep() {
        let (name, version) = parse_package_version("@angular/cli@17.0.0");
        assert_eq!(name, "@angular/cli");
        assert_eq!(version, Some("17.0.0".to_string()));
    }

    #[test]
    fn test_parse_package_version_no_version() {
        let (name, version) = parse_package_version("typescript");
        assert_eq!(name, "typescript");
        assert_eq!(version, None);
    }

    #[test]
    fn test_parse_package_version_scoped_no_version() {
        let (name, version) = parse_package_version("@types/node");
        assert_eq!(name, "@types/node");
        assert_eq!(version, None);
    }

    #[test]
    fn outdated_table_yields_changed_latest_versions() {
        let stdout = "bun outdated v1.4.2 (744846f84)
|----------------------------------------------|
| Package           | Current | Update | Latest  |
|-------------------|---------|--------|---------|
| cowsay            | 1.4.0   | 1.4.0  | 1.6.0   |
| @scope/held       | 2.0.0   | 2.0.0  | 2.1.0 * |
| same              | 1.0.0   | 1.0.0  | 1.0.0 * |
|----------------------------------------------|
Note: The * indicates that version isn't true latest due to minimum release age
";
        assert_eq!(
            parse_outdated_table(stdout),
            vec![
                Upgrade::new("cowsay", Some("1.4.0"), "1.6.0"),
                Upgrade::new("@scope/held", Some("2.0.0"), "2.1.0")
            ]
        );
        assert!(parse_outdated_table("bun outdated v1.4.2 (744846f84)\n").is_empty());
    }
}

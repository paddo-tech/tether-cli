use super::command;
use super::policy::PackagePolicy;
use super::{validate_name, validate_version, Cooldown, Ecosystem, PackageInfo, PackageManager};
use anyhow::Result;
use async_trait::async_trait;

/// Name and latest version from `gem outdated` lines like `rdoc (7.0.4 < 8.1.0)`.
fn parse_outdated(stdout: &str) -> Vec<(String, String)> {
    stdout
        .lines()
        .filter_map(|line| {
            let (name, versions) = line.trim().split_once(" (")?;
            let (_, latest) = versions.strip_suffix(')')?.split_once(" < ")?;
            Some((name.to_string(), latest.to_string()))
        })
        .collect()
}

pub struct GemManager;

impl GemManager {
    pub fn new() -> Self {
        Self
    }

    fn policy(&self) -> PackagePolicy {
        PackagePolicy::load()
    }

    async fn run_gem(&self, args: &[&str]) -> Result<String> {
        let output = command("gem")?.args(args).output().await?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(anyhow::anyhow!("gem command failed: {}", stderr));
        }

        Ok(String::from_utf8(output.stdout)?)
    }

    // --user-install ignores GEM_HOME and puts executables outside $GEM_HOME/bin
    fn user_install_flag() -> Option<&'static str> {
        std::env::var_os("GEM_HOME")
            .is_none()
            .then_some("--user-install")
    }
}

// Default gems ship with each Ruby, and so do bundled gems (rake, minitest) and distro
// gems such as Debian's rubygems-integration. Those live outside the dirs that
// `gem install` writes to, and Homebrew copies them into its default dir, so a gem is
// dropped when its newest version is one found outside those dirs. A newer copy the user
// installed still counts. Dependencies follow their parents, so only top-level gems are
// recorded, matching brew's --installed-on-request; dependencies of shipped gems are
// ignored so a newer user copy (e.g. stringio) still counts. A Ruby whose own gem dir is
// also its install dir (ruby-build, rbenv) shows no difference, so its bundled gems stay.
// Each line is "name version" with the newest installed version
const TOP_LEVEL_GEMS: &str = "dirs = [Gem.dir, Gem.user_dir, Gem.default_dir, *ENV.fetch('GEM_PATH', '').split(File::PATH_SEPARATOR)]
dirs = dirs.reject(&:empty?).map { |d| File.expand_path(d) }
specs = Gem::Specification.reject(&:default_gem?)
shipped = specs.reject { |s| dirs.include?(File.expand_path(s.base_dir)) }.map { |s| [s.name, s.version] }
own = specs.reject { |s| shipped.include?([s.name, s.version]) }
deps = own.flat_map { |s| s.runtime_dependencies.map(&:name) }
latest = specs.group_by(&:name).transform_values { |v| v.map(&:version).max }
latest.reject! { |n, v| shipped.include?([n, v]) }
(latest.keys - deps).each { |n| puts \"#{n} #{latest[n]}\" }";

impl Default for GemManager {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl PackageManager for GemManager {
    async fn list_installed(&self) -> Result<Vec<PackageInfo>> {
        let output = command("ruby")?
            .args(["-e", TOP_LEVEL_GEMS])
            .output()
            .await?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(anyhow::anyhow!("gem listing failed: {}", stderr));
        }

        let mut packages = Vec::new();

        for line in String::from_utf8(output.stdout)?.lines() {
            let mut parts = line.split_whitespace();
            let Some(name) = parts.next() else {
                continue;
            };

            packages.push(PackageInfo {
                name: name.to_string(),
                version: parts.next().map(str::to_string),
            });
        }

        packages.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(packages)
    }

    async fn install(&self, package: &PackageInfo) -> Result<()> {
        validate_name(Ecosystem::Gem, &package.name)?;
        let pkg_spec = if let Some(version) = &package.version {
            validate_version(Ecosystem::Gem, version)?;
            format!("{}:{}", package.name, version)
        } else {
            package.name.clone()
        };

        // Without GEM_HOME, --user-install avoids needing sudo for a system Ruby
        // --conservative skips gems present only as dependencies, which the listing omits
        // --remote stops gem from installing a matching *.gem file instead of the registry gem
        let mut args = vec!["install", pkg_spec.as_str(), "--conservative", "--remote"];
        args.extend(Self::user_install_flag());
        self.run_gem(&args).await?;
        Ok(())
    }

    async fn is_available(&self) -> bool {
        which::which("gem").is_ok()
    }

    fn name(&self) -> &str {
        "gem"
    }

    fn ecosystem(&self) -> Ecosystem {
        Ecosystem::Gem
    }

    // RubyGems has no release-age filter
    async fn cooldown(&self) -> Cooldown {
        if self.policy().min_release_age_days == 0 {
            Cooldown::Off
        } else {
            Cooldown::Unsupported
        }
    }

    async fn update_all(&self) -> Result<()> {
        let packages = self.list_installed().await?;
        if packages.is_empty() {
            return Ok(());
        }

        // Without names gem updates every gem, so held gems need the rest named
        let held = super::inbox::hold_malicious_upgrades(self).await;
        let mut names = Vec::new();
        if !held.is_empty() {
            names = packages
                .into_iter()
                .map(|p| p.name)
                .filter(|name| !held.contains(name))
                .filter(|name| validate_name(Ecosystem::Gem, name).is_ok())
                .collect();
            if names.is_empty() {
                return Ok(());
            }
        }
        let output = command("gem")?
            .arg("update")
            .args(names)
            .arg("--remote")
            .args(Self::user_install_flag())
            .output()
            .await?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(anyhow::anyhow!("gem update failed: {}", stderr));
        }

        Ok(())
    }

    async fn upgrade_candidates(&self) -> Result<Vec<(String, String)>> {
        Ok(parse_outdated(
            &self.run_gem(&["outdated", "--remote"]).await?,
        ))
    }

    async fn uninstall(&self, package: &str) -> Result<()> {
        validate_name(Ecosystem::Gem, package)?;
        let output = command("gem")?
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
        let output = command("gem")?
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn outdated_lines_yield_latest_versions() {
        let stdout = "rdoc (7.0.4 < 8.1.0)\nnet-imap (0.5.1 < 0.5.6)\nnoise\n";
        assert_eq!(
            parse_outdated(stdout),
            vec![
                ("rdoc".to_string(), "8.1.0".to_string()),
                ("net-imap".to_string(), "0.5.6".to_string())
            ]
        );
    }
}

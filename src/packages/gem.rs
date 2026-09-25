use super::{PackageInfo, PackageManager};
use anyhow::Result;
use async_trait::async_trait;
use tokio::process::Command;

pub struct GemManager;

impl GemManager {
    pub fn new() -> Self {
        Self
    }

    async fn run_gem(&self, args: &[&str]) -> Result<String> {
        let output = Command::new("gem").args(args).output().await?;

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

// Default gems ship with each Ruby and dependencies follow their parents, so only
// top-level gems are recorded, matching brew's --installed-on-request. Dependencies of
// default gems are ignored so a user-installed newer copy (e.g. stringio) still counts
const TOP_LEVEL_GEMS: &str = "specs = Gem::Specification.reject(&:default_gem?)
deps = specs.flat_map { |s| s.runtime_dependencies.map(&:name) }
puts specs.map(&:name).uniq - deps";

impl Default for GemManager {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl PackageManager for GemManager {
    async fn list_installed(&self) -> Result<Vec<PackageInfo>> {
        let output = Command::new("ruby")
            .args(["-e", TOP_LEVEL_GEMS])
            .output()
            .await?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(anyhow::anyhow!("gem listing failed: {}", stderr));
        }

        let mut packages = Vec::new();

        for line in String::from_utf8(output.stdout)?.lines() {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }

            packages.push(PackageInfo {
                name: line.to_string(),
                version: None,
            });
        }

        packages.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(packages)
    }

    async fn install(&self, package: &PackageInfo) -> Result<()> {
        let pkg_spec = if let Some(version) = &package.version {
            format!("{}:{}", package.name, version)
        } else {
            package.name.clone()
        };

        // Without GEM_HOME, --user-install avoids needing sudo for a system Ruby
        // --conservative skips gems present only as dependencies, which the listing omits
        let mut args = vec!["install", pkg_spec.as_str(), "--conservative"];
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

    async fn update_all(&self) -> Result<()> {
        let packages = self.list_installed().await?;
        if packages.is_empty() {
            return Ok(());
        }

        let output = Command::new("gem")
            .arg("update")
            .args(Self::user_install_flag())
            .output()
            .await?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(anyhow::anyhow!("gem update failed: {}", stderr));
        }

        Ok(())
    }

    async fn uninstall(&self, package: &str) -> Result<()> {
        let output = Command::new("gem")
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
        let output = Command::new("gem")
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

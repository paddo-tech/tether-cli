use super::{PackageInfo, PackageManager};
use anyhow::Result;
use async_trait::async_trait;
use std::path::PathBuf;
use tokio::process::Command;

/// Structured representation of Brewfile contents
#[derive(Debug, Clone, Default)]
pub struct BrewfilePackages {
    pub taps: Vec<String>,
    pub formulae: Vec<String>,
    pub casks: Vec<String>,
}

/// Normalize a brew formula name by stripping tap prefix.
/// e.g., "oven-sh/bun/bun" -> "bun", "git" -> "git"
pub fn normalize_formula_name(name: &str) -> &str {
    // Format is "tap/repo/formula" - we want just the formula part
    name.rsplit('/').next().unwrap_or(name)
}

impl BrewfilePackages {
    /// Parse a Brewfile string into structured package lists
    pub fn parse(content: &str) -> Self {
        let mut packages = Self::default();

        for line in content.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }

            // Extract the quoted package name
            if let Some(name) = line.split('"').nth(1) {
                if line.starts_with("tap ") {
                    packages.taps.push(name.to_string());
                } else if line.starts_with("brew ") {
                    packages.formulae.push(name.to_string());
                } else if line.starts_with("cask ") {
                    packages.casks.push(name.to_string());
                }
            }
        }

        // Sort for deterministic output
        packages.taps.sort();
        packages.formulae.sort();
        packages.casks.sort();

        packages
    }

    /// Generate a Brewfile string from structured package lists
    pub fn generate(&self) -> String {
        let mut lines = Vec::new();

        for tap in &self.taps {
            lines.push(format!("tap \"{}\"", tap));
        }
        for formula in &self.formulae {
            lines.push(format!("brew \"{}\"", formula));
        }
        for cask in &self.casks {
            lines.push(format!("cask \"{}\"", cask));
        }

        lines.join("\n") + "\n"
    }
}

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

    /// Get a temporary file path for Brewfile operations
    fn temp_brewfile_path() -> Result<PathBuf> {
        let home =
            home::home_dir().ok_or_else(|| anyhow::anyhow!("Could not find home directory"))?;
        Ok(home.join(".tether").join("Brewfile.tmp"))
    }

    /// List installed casks
    pub async fn list_installed_casks(&self) -> Result<Vec<String>> {
        let output = self.run_brew(&["list", "--cask", "-1"]).await?;
        Ok(output
            .lines()
            .map(|l| l.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect())
    }

    /// List installed taps
    pub async fn list_taps(&self) -> Result<Vec<String>> {
        let output = self.run_brew(&["tap"]).await?;
        Ok(output
            .lines()
            .map(|l| l.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect())
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
        // Use --installed-on-request to only get explicitly installed packages,
        // not dependencies. This matches what `brew bundle dump` outputs.
        let output = self
            .run_brew(&["list", "--formula", "--installed-on-request", "-1"])
            .await?;

        let mut packages = Vec::new();
        for line in output.lines() {
            let name = line.trim();
            if !name.is_empty() {
                packages.push(PackageInfo {
                    name: name.to_string(),
                    version: None,
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

    async fn export_manifest(&self) -> Result<String> {
        // Use `brew bundle dump` to generate a Brewfile
        let temp_path = Self::temp_brewfile_path()?;

        // Ensure parent directory exists
        if let Some(parent) = temp_path.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }

        // Remove existing temp file if it exists
        if temp_path.exists() {
            tokio::fs::remove_file(&temp_path).await?;
        }

        // Generate Brewfile
        let output = Command::new("brew")
            .args([
                "bundle",
                "dump",
                "--no-vscode",
                "--file",
                temp_path
                    .to_str()
                    .ok_or_else(|| anyhow::anyhow!("Invalid path for Brewfile: {:?}", temp_path))?,
            ])
            .output()
            .await?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(anyhow::anyhow!("brew bundle dump failed: {}", stderr));
        }

        // Read the generated Brewfile
        let content = tokio::fs::read_to_string(&temp_path).await?;

        // Clean up temp file
        let _ = tokio::fs::remove_file(&temp_path).await;

        Ok(content)
    }

    async fn import_manifest(&self, manifest_content: &str) -> Result<()> {
        // Write manifest to temporary Brewfile
        let temp_path = Self::temp_brewfile_path()?;

        // Ensure parent directory exists
        if let Some(parent) = temp_path.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }

        tokio::fs::write(&temp_path, manifest_content).await?;

        // Use `brew bundle install` to install packages from Brewfile
        // --no-upgrade: don't upgrade existing packages (faster, less disruptive)
        // Stream output to terminal so user can see progress and any errors
        let status = Command::new("brew")
            .args([
                "bundle",
                "install",
                "--no-upgrade",
                "--file",
                temp_path
                    .to_str()
                    .ok_or_else(|| anyhow::anyhow!("Invalid path for Brewfile: {:?}", temp_path))?,
            ])
            .env("HOMEBREW_NO_AUTO_UPDATE", "1")
            .env("NONINTERACTIVE", "1")
            .stdout(std::process::Stdio::inherit())
            .stderr(std::process::Stdio::inherit())
            .status()
            .await?;

        // Clean up temp file
        let _ = tokio::fs::remove_file(&temp_path).await;

        // brew bundle may return non-zero even if most packages installed
        // (e.g., one cask failed). Log but don't fail.
        if !status.success() {
            eprintln!("Warning: brew bundle had issues (exit code: {})", status);
        }

        Ok(())
    }

    async fn remove_unlisted(&self, manifest_content: &str) -> Result<()> {
        // Parse manifest to get desired packages
        let desired: std::collections::HashSet<&str> = manifest_content
            .lines()
            .filter_map(|line| {
                let line = line.trim();
                // Parse Brewfile format: brew "package" or cask "package"
                if line.starts_with("brew \"") || line.starts_with("cask \"") {
                    line.split('"').nth(1)
                } else {
                    None
                }
            })
            .collect();

        if desired.is_empty() {
            return Ok(());
        }

        // Get installed packages
        let installed = self.list_installed().await?;

        // Remove packages not in manifest
        for pkg in installed {
            if !desired.contains(pkg.name.as_str()) {
                let output = Command::new("brew")
                    .args(["uninstall", &pkg.name])
                    .output()
                    .await?;

                if !output.status.success() {
                    let stderr = String::from_utf8_lossy(&output.stderr);
                    eprintln!("Warning: Failed to uninstall {}: {}", pkg.name, stderr);
                }
            }
        }

        Ok(())
    }

    async fn update_all(&self) -> Result<()> {
        // Check if there are any packages to update
        let packages = self.list_installed().await?;
        if packages.is_empty() {
            return Ok(());
        }

        // Update Homebrew itself and upgrade all packages
        Command::new("brew").args(["update"]).output().await?;

        let output = Command::new("brew").args(["upgrade"]).output().await?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(anyhow::anyhow!("brew upgrade failed: {}", stderr));
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Brewfile parsing tests
    #[test]
    fn test_parse_brewfile() {
        let content = r#"
tap "homebrew/core"
tap "homebrew/cask"
brew "git"
brew "ripgrep"
cask "visual-studio-code"
"#;
        let packages = BrewfilePackages::parse(content);
        assert_eq!(packages.taps, vec!["homebrew/cask", "homebrew/core"]);
        assert_eq!(packages.formulae, vec!["git", "ripgrep"]);
        assert_eq!(packages.casks, vec!["visual-studio-code"]);
    }

    #[test]
    fn test_parse_brewfile_skips_comments() {
        let content = r#"
# This is a comment
tap "homebrew/core"
# Another comment
brew "git"
"#;
        let packages = BrewfilePackages::parse(content);
        assert_eq!(packages.taps, vec!["homebrew/core"]);
        assert_eq!(packages.formulae, vec!["git"]);
    }

    #[test]
    fn test_parse_brewfile_empty() {
        let packages = BrewfilePackages::parse("");
        assert!(packages.taps.is_empty());
        assert!(packages.formulae.is_empty());
        assert!(packages.casks.is_empty());
    }

    #[test]
    fn test_parse_brewfile_only_comments() {
        let content = "# comment\n# another\n";
        let packages = BrewfilePackages::parse(content);
        assert!(packages.taps.is_empty());
    }

    // Brewfile generation tests
    #[test]
    fn test_generate_brewfile() {
        let packages = BrewfilePackages {
            taps: vec!["homebrew/cask".to_string()],
            formulae: vec!["git".to_string()],
            casks: vec!["iterm2".to_string()],
        };
        let output = packages.generate();
        assert!(output.contains("tap \"homebrew/cask\""));
        assert!(output.contains("brew \"git\""));
        assert!(output.contains("cask \"iterm2\""));
    }

    #[test]
    fn test_generate_brewfile_empty() {
        let packages = BrewfilePackages::default();
        let output = packages.generate();
        assert_eq!(output, "\n");
    }

    // Roundtrip tests
    #[test]
    fn test_brewfile_roundtrip() {
        let original = BrewfilePackages {
            taps: vec!["tap1".to_string(), "tap2".to_string()],
            formulae: vec!["brew1".to_string(), "brew2".to_string()],
            casks: vec!["cask1".to_string()],
        };
        let generated = original.generate();
        let parsed = BrewfilePackages::parse(&generated);

        assert_eq!(original.taps, parsed.taps);
        assert_eq!(original.formulae, parsed.formulae);
        assert_eq!(original.casks, parsed.casks);
    }

    // normalize_formula_name tests
    #[test]
    fn test_normalize_formula_name_simple() {
        assert_eq!(normalize_formula_name("git"), "git");
        assert_eq!(normalize_formula_name("ripgrep"), "ripgrep");
    }

    #[test]
    fn test_normalize_formula_name_with_tap() {
        assert_eq!(normalize_formula_name("homebrew/core/wget"), "wget");
        assert_eq!(normalize_formula_name("oven-sh/bun/bun"), "bun");
    }

    #[test]
    fn test_normalize_formula_name_empty() {
        assert_eq!(normalize_formula_name(""), "");
    }
}

use super::{validate_name, Cooldown, Ecosystem, PackageInfo, PackageManager, PackagePolicy};
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

    /// Drop entries that fail name validation, with a warning, so they never reach brew.
    pub fn retain_valid(&mut self) {
        let keep = |ecosystem: Ecosystem, name: &String| match validate_name(ecosystem, name) {
            Ok(()) => true,
            Err(e) => {
                eprintln!("Warning: Skipping Brewfile entry: {}", e);
                false
            }
        };
        self.taps.retain(|t| keep(Ecosystem::BrewTap, t));
        self.formulae.retain(|f| keep(Ecosystem::Brew, f));
        self.casks.retain(|c| keep(Ecosystem::Brew, c));
    }

    /// Move untrusted taps, and formulae and casks qualified with one, out of `self`.
    /// Short names stay: without their tap tapped they only resolve to trusted taps.
    pub fn take_untrusted(&mut self, policy: &PackagePolicy) -> BrewfilePackages {
        let trusted = |name: &String| tap_of(name).is_none_or(|tap| policy.tap_trusted(tap));
        let (taps, untrusted_taps) = self.taps.drain(..).partition(|t| policy.tap_trusted(t));
        let (formulae, untrusted_formulae) = self.formulae.drain(..).partition(trusted);
        let (casks, untrusted_casks) = self.casks.drain(..).partition(trusted);
        self.taps = taps;
        self.formulae = formulae;
        self.casks = casks;
        BrewfilePackages {
            taps: untrusted_taps,
            formulae: untrusted_formulae,
            casks: untrusted_casks,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.taps.is_empty() && self.formulae.is_empty() && self.casks.is_empty()
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

/// The `user/repo` tap of a qualified `user/repo/name` formula or cask.
fn tap_of(name: &str) -> Option<&str> {
    name.rsplit_once('/').map(|(tap, _)| tap)
}

/// Untrusted taps and their packages are held back from install. The approval inbox
/// takes them over here once it exists.
pub fn hold_untrusted(untrusted: &BrewfilePackages) {
    for tap in &untrusted.taps {
        eprintln!(
            "Warning: Skipping untrusted tap {}. Add it to packages.brew.trusted_taps to allow it",
            tap
        );
    }
    for name in untrusted.formulae.iter().chain(&untrusted.casks) {
        eprintln!("Warning: Skipping {} from an untrusted tap", name);
    }
}

pub struct BrewManager;

impl BrewManager {
    pub fn new() -> Self {
        Self
    }

    fn policy(&self) -> PackagePolicy {
        PackagePolicy::load()
    }

    /// Validate a formula or cask and refuse ones qualified with an untrusted tap.
    fn check_package(&self, name: &str) -> Result<()> {
        validate_name(Ecosystem::Brew, name)?;
        if let Some(tap) = tap_of(name) {
            if !self.policy().tap_trusted(tap) {
                anyhow::bail!("{} is from untrusted tap {}", name, tap);
            }
        }
        Ok(())
    }

    /// Validate and trust-filter a Brewfile before brew sees it.
    pub fn filter_brewfile(&self, packages: &mut BrewfilePackages) {
        packages.retain_valid();
        let untrusted = packages.take_untrusted(&self.policy());
        if !untrusted.is_empty() {
            hold_untrusted(&untrusted);
        }
    }

    /// Unlink conflicting versioned formulae before installing new versions.
    /// e.g., if installing tether-cli@1.0.0 but tether-cli@1.1.0 is linked, unlink it first.
    async fn unlink_conflicting_versioned_formulae(&self, manifest_content: &str) -> Result<()> {
        let packages = BrewfilePackages::parse(manifest_content);

        for formula in &packages.formulae {
            if let Some(at_pos) = formula.find('@') {
                let base_name = &formula[..at_pos];
                let requested_version = &formula[at_pos + 1..];

                // Check what versions of this formula are installed
                let output = Command::new("brew")
                    .args(["list", "--versions"])
                    .output()
                    .await?;

                if output.status.success() {
                    let stdout = String::from_utf8_lossy(&output.stdout);
                    for line in stdout.lines() {
                        // Format: "formula@version version-number" or "formula version-number"
                        let parts: Vec<&str> = line.split_whitespace().collect();
                        if let Some(installed_name) = parts.first() {
                            // Check if it's a versioned formula of the same base
                            if let Some(installed_at_pos) = installed_name.find('@') {
                                let installed_base = &installed_name[..installed_at_pos];
                                let installed_version = &installed_name[installed_at_pos + 1..];

                                // Same base, different version = conflict
                                if installed_base == base_name
                                    && installed_version != requested_version
                                {
                                    let _ = Command::new("brew")
                                        .args(["unlink", installed_name])
                                        .output()
                                        .await;
                                }
                            }
                        }
                    }
                }
            }
        }

        Ok(())
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
        let home = crate::home_dir()?;
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

    /// Add a tap
    pub async fn tap(&self, tap_name: &str) -> Result<()> {
        validate_name(Ecosystem::BrewTap, tap_name)?;
        if !self.policy().tap_trusted(tap_name) {
            anyhow::bail!("{} is not a trusted tap", tap_name);
        }
        self.run_brew(&["tap", tap_name]).await?;
        Ok(())
    }

    /// Install a single cask.
    /// Returns Ok(true) if installed, Ok(false) if needs password (flagged for manual sync).
    pub async fn install_cask(&self, cask: &str, allow_interactive: bool) -> Result<bool> {
        use std::process::Stdio;

        self.check_package(cask)?;
        let mut cmd = Command::new("brew");
        cmd.args(["install", "--cask", cask])
            .env("NONINTERACTIVE", "1")
            .env("HOMEBREW_NO_AUTO_UPDATE", "1");

        if allow_interactive {
            cmd.stdin(Stdio::inherit())
                .stdout(Stdio::inherit())
                .stderr(Stdio::inherit());
            let status = cmd.status().await?;
            Ok(status.success())
        } else {
            // Daemon mode: stdin=null so sudo fails instead of hanging
            cmd.stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped());

            let output = cmd.output().await?;

            if output.status.success() {
                return Ok(true);
            }

            // Check if failed due to password prompt
            let stderr = String::from_utf8_lossy(&output.stderr);
            if stderr.contains("sudo: no tty present")
                || stderr.contains("Password:")
                || stderr.contains("password is required")
            {
                return Ok(false); // needs password
            }

            // Other error - propagate
            Err(anyhow::anyhow!(
                "Failed to install cask {}: {}",
                cask,
                stderr
            ))
        }
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
        self.check_package(&package.name)?;
        self.run_brew(&["install", &package.name]).await?;
        Ok(())
    }

    async fn is_available(&self) -> bool {
        which::which("brew").is_ok()
    }

    fn name(&self) -> &str {
        "brew"
    }

    fn ecosystem(&self) -> Ecosystem {
        Ecosystem::Brew
    }

    // Homebrew has no release-age filter; the tap allowlist guards it instead
    async fn cooldown(&self) -> Cooldown {
        Cooldown::Off
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
        let mut packages = BrewfilePackages::parse(manifest_content);
        self.filter_brewfile(&mut packages);
        let manifest = packages.generate();
        let manifest_content = manifest.as_str();

        // Unlink any conflicting versioned formulae before installing
        self.unlink_conflicting_versioned_formulae(manifest_content)
            .await?;

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
                if let Err(e) = validate_name(Ecosystem::Brew, &pkg.name) {
                    eprintln!("Warning: Skipping brew entry: {}", e);
                    continue;
                }
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

    async fn uninstall(&self, package: &str) -> Result<()> {
        validate_name(Ecosystem::Brew, package)?;
        let output = Command::new("brew")
            .args(["uninstall", package])
            .output()
            .await?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(anyhow::anyhow!("brew uninstall failed: {}", stderr));
        }

        Ok(())
    }

    async fn get_dependents(&self, package: &str) -> Result<Vec<String>> {
        let output = Command::new("brew")
            .args(["uses", "--installed", package])
            .output()
            .await?;

        if !output.status.success() {
            return Ok(vec![]);
        }

        let stdout = String::from_utf8_lossy(&output.stdout);
        Ok(stdout
            .lines()
            .map(|l| l.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect())
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

    #[test]
    fn test_take_untrusted_splits_by_tap() {
        let policy = PackagePolicy {
            min_release_age_days: 7,
            allow_scripts: Vec::new(),
            trusted_taps: vec!["oven-sh/bun".to_string()],
        };
        let mut packages = BrewfilePackages {
            taps: vec![
                "homebrew/cask".to_string(),
                "oven-sh/bun".to_string(),
                "evil/tap".to_string(),
            ],
            formulae: vec![
                "git".to_string(),
                "oven-sh/bun/bun".to_string(),
                "evil/tap/payload".to_string(),
            ],
            casks: vec!["iterm2".to_string(), "evil/tap/app".to_string()],
        };
        let untrusted = packages.take_untrusted(&policy);
        assert_eq!(packages.taps, vec!["homebrew/cask", "oven-sh/bun"]);
        assert_eq!(packages.formulae, vec!["git", "oven-sh/bun/bun"]);
        assert_eq!(packages.casks, vec!["iterm2"]);
        assert_eq!(untrusted.taps, vec!["evil/tap"]);
        assert_eq!(untrusted.formulae, vec!["evil/tap/payload"]);
        assert_eq!(untrusted.casks, vec!["evil/tap/app"]);
    }

    #[test]
    fn test_retain_valid_drops_bad_entries() {
        let mut packages = BrewfilePackages::parse(
            "tap \"--force\"\ntap \"oven-sh/bun\"\nbrew \"--HEAD\"\nbrew \"git\"\ncask \"../x\"\ncask \"iterm2\"\n",
        );
        packages.retain_valid();
        assert_eq!(packages.taps, vec!["oven-sh/bun"]);
        assert_eq!(packages.formulae, vec!["git"]);
        assert_eq!(packages.casks, vec!["iterm2"]);
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

use super::command;
use super::inbox::{self, InboxItem, Kind, Reason};
use super::policy::first_warning;
use super::{validate_name, Cooldown, Ecosystem, PackageInfo, PackageManager, PackagePolicy};
use anyhow::Result;
use async_trait::async_trait;
use serde::Deserialize;
use std::path::PathBuf;

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
                let message = format!("Skipping Brewfile entry: {}", e);
                if first_warning(&message) {
                    crate::cli::Output::warning(&message);
                }
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
        let allowed = |manager: &str, name: &String| {
            tap_of(name).is_none_or(|tap| policy.brew_allowed(manager, name, tap))
        };
        let (taps, untrusted_taps) = self.taps.drain(..).partition(|t| policy.tap_trusted(t));
        let (formulae, untrusted_formulae) = self
            .formulae
            .drain(..)
            .partition(|f| allowed("brew_formulae", f));
        let (casks, untrusted_casks) = self.casks.drain(..).partition(|c| allowed("brew_casks", c));
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

/// The fields Tether reads from `brew info --json=v2`. Formulae and casks share them.
#[derive(Debug, Deserialize)]
struct BrewInfo {
    #[serde(default)]
    formulae: Vec<BrewInfoEntry>,
    #[serde(default)]
    casks: Vec<BrewInfoEntry>,
}

#[derive(Debug, Deserialize)]
struct BrewInfoEntry {
    #[serde(alias = "full_token")]
    full_name: String,
    tap: Option<String>,
    #[serde(default)]
    outdated: bool,
    #[serde(default)]
    pinned: bool,
}

/// Outdated, unpinned formulae and casks from trusted taps, fully qualified, so an upgrade
/// never pulls a new release from a tap the user has not trusted.
fn trusted_upgrades(info: BrewInfo, policy: &PackagePolicy) -> (Vec<String>, Vec<String>) {
    let pick = |entries: Vec<BrewInfoEntry>| {
        entries
            .into_iter()
            .filter(|e| e.outdated && !e.pinned)
            .filter_map(|e| {
                let tap = e.tap.filter(|tap| policy.tap_trusted(tap))?;
                Some(format!("{}/{}", tap, normalize_formula_name(&e.full_name)))
            })
            .filter(|name| validate_name(Ecosystem::Brew, name).is_ok())
            .collect()
    };
    (pick(info.formulae), pick(info.casks))
}

/// A brew command that loads only the formulae and casks named on it. Without these, brew
/// updates itself first, and checks dependents and cleans up after an upgrade, and each of
/// those loads the Ruby of every installed formula or cask, from any tap.
fn brew_without_installed_scan() -> Result<tokio::process::Command> {
    let mut cmd = command("brew")?;
    cmd.env("HOMEBREW_NO_AUTO_UPDATE", "1")
        .env("HOMEBREW_FORCE_API_AUTO_UPDATE", "1")
        .env("HOMEBREW_NO_INSTALLED_DEPENDENTS_CHECK", "1")
        .env("HOMEBREW_NO_INSTALL_CLEANUP", "1")
        .env("HOMEBREW_NO_ENV_HINTS", "1");
    Ok(cmd)
}

/// The tap an install receipt names, from `source.tap`.
fn receipt_tap(path: &std::path::Path) -> Option<String> {
    let text = std::fs::read_to_string(path).ok()?;
    let json: serde_json::Value = serde_json::from_str(&text).ok()?;
    json.pointer("/source/tap")?.as_str().map(str::to_string)
}

/// Installed formulae and the tap of each, read from the install receipts in the Cellar.
/// A formula whose kegs have no tap, or disagree on it, gets `None`.
fn installed_formulae(cellar: &std::path::Path) -> Vec<(String, Option<String>)> {
    let Ok(racks) = std::fs::read_dir(cellar) else {
        return Vec::new();
    };
    let mut installed: Vec<_> = racks
        .filter_map(|e| e.ok())
        .filter(|rack| rack.path().is_dir())
        .map(|rack| {
            let taps: std::collections::BTreeSet<Option<String>> = std::fs::read_dir(rack.path())
                .into_iter()
                .flatten()
                .filter_map(|e| e.ok())
                .filter(|keg| keg.path().is_dir())
                .map(|keg| receipt_tap(&keg.path().join("INSTALL_RECEIPT.json")))
                .collect();
            let tap = match taps.len() {
                1 => taps.into_iter().next().flatten(),
                _ => None,
            };
            (rack.file_name().to_string_lossy().to_string(), tap)
        })
        .collect();
    installed.sort();
    installed
}

/// Installed casks and the tap of each, read from the install receipts in the Caskroom.
fn installed_casks(caskroom: &std::path::Path) -> Vec<(String, Option<String>)> {
    let Ok(tokens) = std::fs::read_dir(caskroom) else {
        return Vec::new();
    };
    let mut installed: Vec<_> = tokens
        .filter_map(|e| e.ok())
        .filter(|token| token.path().is_dir())
        .map(|token| {
            let receipt = token.path().join(".metadata/INSTALL_RECEIPT.json");
            (
                token.file_name().to_string_lossy().to_string(),
                receipt_tap(&receipt),
            )
        })
        .collect();
    installed.sort();
    installed
}

/// Fully qualified `tap/name` of each installed package from a trusted tap. brew then
/// loads only these, never a package from a tap the user has not trusted.
fn trusted_names(installed: Vec<(String, Option<String>)>, policy: &PackagePolicy) -> Vec<String> {
    installed
        .into_iter()
        .filter_map(|(name, tap)| {
            let tap = tap.filter(|tap| policy.tap_trusted(tap))?;
            let qualified = format!("{}/{}", tap, name);
            validate_name(Ecosystem::Brew, &qualified)
                .is_ok()
                .then_some(qualified)
        })
        .collect()
}

/// Tapped repositories under `taps_dir` as (`user/repo`, path).
fn installed_taps(taps_dir: &std::path::Path) -> Vec<(String, PathBuf)> {
    let mut taps = Vec::new();
    for user in std::fs::read_dir(taps_dir)
        .into_iter()
        .flatten()
        .filter_map(|e| e.ok())
    {
        for repo in std::fs::read_dir(user.path())
            .into_iter()
            .flatten()
            .filter_map(|e| e.ok())
        {
            let repo_name = repo.file_name().to_string_lossy().to_string();
            if let Some(short) = repo_name.strip_prefix("homebrew-") {
                taps.push((
                    format!("{}/{}", user.file_name().to_string_lossy(), short),
                    repo.path(),
                ));
            }
        }
    }
    taps.sort();
    taps
}

/// The `user/repo` tap of a qualified `user/repo/name` formula or cask.
fn tap_of(name: &str) -> Option<&str> {
    name.rsplit_once('/').map(|(tap, _)| tap)
}

/// Installed taps under `taps_dir` that could provide a short formula or cask name. It reads
/// file names and JSON only, never Ruby. It mirrors brew's lookup: a formula file in
/// `Formula/` or `HomebrewFormula/` (sharded or not), else a top-level `*.rb`, a cask file in
/// `Casks/`, an alias, a rename or a tap migration. A broader match only holds a package.
fn taps_providing(taps_dir: &std::path::Path, name: &str, cask: bool) -> Vec<String> {
    let file = format!("{}.rb", name.to_lowercase());
    let has_file = |dir: PathBuf, recursive: bool| {
        walkdir::WalkDir::new(dir)
            .max_depth(if recursive { usize::MAX } else { 1 })
            .into_iter()
            .filter_map(|e| e.ok())
            .any(|e| e.file_type().is_file() && e.file_name() == file.as_str())
    };
    let json_key = |path: PathBuf| {
        std::fs::read_to_string(path)
            .ok()
            .and_then(|text| serde_json::from_str::<serde_json::Value>(&text).ok())
            .is_some_and(|json| json.get(name).is_some())
    };
    let mut taps = Vec::new();
    let Ok(users) = std::fs::read_dir(taps_dir) else {
        return taps;
    };
    for user in users.filter_map(|e| e.ok()) {
        let Ok(repos) = std::fs::read_dir(user.path()) else {
            continue;
        };
        for repo in repos.filter_map(|e| e.ok()) {
            let path = repo.path();
            let repo_name = repo.file_name().to_string_lossy().to_string();
            let Some(short) = repo_name.strip_prefix("homebrew-") else {
                continue;
            };
            let provides = if cask {
                has_file(path.join("Casks"), true) || json_key(path.join("cask_renames.json"))
            } else {
                let file_found = match ["Formula", "HomebrewFormula"]
                    .iter()
                    .map(|d| path.join(d))
                    .find(|d| d.is_dir())
                {
                    Some(dir) => has_file(dir, true),
                    None => has_file(path.clone(), false),
                };
                file_found
                    || path.join("Aliases").join(name).exists()
                    || json_key(path.join("formula_renames.json"))
            };
            let provides = provides || json_key(path.join("tap_migrations.json"));
            if provides {
                taps.push(format!("{}/{}", user.file_name().to_string_lossy(), short));
            }
        }
    }
    taps.sort();
    taps
}

/// Untrusted taps and their packages go to the approval inbox instead of brew.
/// The daemon re-reads the Brewfile every cycle, so only newly held items are reported.
pub fn hold_untrusted(untrusted: &BrewfilePackages) {
    let item = |manager: &str, name: &String, tap: Option<&str>| InboxItem {
        kind: Kind::Package,
        manager: manager.to_string(),
        name: name.clone(),
        version: None,
        tap: tap.map(str::to_string),
        source_machine: None,
        commit: None,
        signer: None,
        reasons: vec![Reason::UntrustedTap],
        advisories: Vec::new(),
        first_seen: chrono::Utc::now(),
    };
    let items = untrusted
        .taps
        .iter()
        .map(|t| item("brew_taps", t, None))
        .chain(
            untrusted
                .formulae
                .iter()
                .map(|f| item("brew_formulae", f, tap_of(f))),
        )
        .chain(
            untrusted
                .casks
                .iter()
                .map(|c| item("brew_casks", c, tap_of(c))),
        )
        .collect();
    match inbox::add(items) {
        Ok(held) => {
            for item in held {
                crate::cli::Output::warning(&format!(
                    "Holding {} from an untrusted tap for approval. Run 'tether packages inbox'",
                    item.name
                ));
            }
        }
        Err(e) => {
            crate::cli::Output::warning(&format!("Skipping untrusted Homebrew entries: {}", e))
        }
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

    /// Validate a formula or cask and refuse ones from an untrusted tap, unless the user
    /// approved this one from it.
    async fn check_package(&self, name: &str, cask: bool) -> Result<()> {
        validate_name(Ecosystem::Brew, name)?;
        let manager = if cask { "brew_casks" } else { "brew_formulae" };
        match self.tap_for(name, cask).await {
            Some(tap) if self.policy().brew_allowed(manager, name, &tap) => Ok(()),
            Some(tap) => anyhow::bail!("{} is from untrusted tap {}", name, tap),
            None => anyhow::bail!("cannot find the tap of {}", name),
        }
    }

    /// The tap a formula or cask installs from. A short name resolves to whichever tapped
    /// repository brew picks, which can be an untrusted one, so it is looked up.
    /// `brew info <short name>` would run the Ruby of a formula or cask from any tap before
    /// Tether checks that tap, so brew is asked only about the qualified core name.
    pub async fn tap_for(&self, name: &str, cask: bool) -> Option<String> {
        if let Some(tap) = tap_of(name) {
            return Some(tap.to_string());
        }
        // brew resolves a short name in the core tap first, from API data or the core tap
        let (kind, core) = if cask {
            ("--cask", "homebrew/cask")
        } else {
            ("--formula", "homebrew/core")
        };
        let qualified = format!("{}/{}", core, name);
        let output = command("brew")
            .ok()?
            .args(["info", "--json=v2", kind, &qualified])
            .output()
            .await
            .ok()?;
        if output.status.success() {
            let info: BrewInfo = serde_json::from_slice(&output.stdout).ok()?;
            return info
                .formulae
                .into_iter()
                .chain(info.casks)
                .next()
                .and_then(|entry| entry.tap);
        }
        let repository = self.run_brew(&["--repository"]).await.ok()?;
        let mut taps = taps_providing(
            &PathBuf::from(repository.trim()).join("Library/Taps"),
            name,
            cask,
        );
        // brew refuses a short name that more than one other tap provides
        if taps.len() == 1 {
            taps.pop()
        } else {
            None
        }
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
                let output = command("brew")?
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
                                    let _ = command("brew")?
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
        let output = command("brew")?.args(args).output().await?;

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

        self.check_package(cask, true).await?;
        let mut cmd = command("brew")?;
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
        self.check_package(&package.name, false).await?;
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
        let output = command("brew")?
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
        let status = command("brew")?
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
            crate::cli::Output::warning(&format!("brew bundle had issues (exit code: {})", status));
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
                    crate::cli::Output::warning(&format!("Skipping brew entry: {}", e));
                    continue;
                }
                let output = command("brew")?
                    .args(["uninstall", &pkg.name])
                    .output()
                    .await?;

                if !output.status.success() {
                    let stderr = String::from_utf8_lossy(&output.stderr);
                    crate::cli::Output::warning(&format!(
                        "Failed to uninstall {}: {}",
                        pkg.name, stderr
                    ));
                }
            }
        }

        Ok(())
    }

    async fn update_all(&self) -> Result<()> {
        let policy = self.policy();
        let repository = PathBuf::from(self.run_brew(&["--repository"]).await?.trim());
        let cellar = PathBuf::from(self.run_brew(&["--cellar"]).await?.trim());
        let caskroom = PathBuf::from(self.run_brew(&["--caskroom"]).await?.trim());

        // `brew update` would load every installed formula and cask after it fetches the taps,
        // so git updates the trusted taps and brew refreshes its API data for the core taps.
        for tap in installed_taps(&repository.join("Library/Taps")) {
            if !policy.tap_trusted(&tap.0) {
                continue;
            }
            let output = command("git")?
                .arg("-C")
                .arg(&tap.1)
                .args(["pull", "--ff-only", "--quiet"])
                .output()
                .await?;
            if !output.status.success() {
                crate::cli::Output::warning(&format!(
                    "Could not update tap {}: {}",
                    tap.0,
                    String::from_utf8_lossy(&output.stderr).trim()
                ));
            }
        }

        let installed = [
            ("--formula", installed_formulae(&cellar)),
            ("--cask", installed_casks(&caskroom)),
        ];
        let mut failures = Vec::new();
        for (kind, installed) in installed {
            let names = trusted_names(installed, &policy);
            if names.is_empty() {
                continue;
            }
            let output = brew_without_installed_scan()?
                .args(["info", "--json=v2", kind])
                .args(&names)
                .output()
                .await?;
            // One failed kind must not stop the other kind's upgrades
            if !output.status.success() {
                let stderr = String::from_utf8_lossy(&output.stderr);
                failures.push(format!("brew info {} failed: {}", kind, stderr.trim()));
                continue;
            }
            let info: BrewInfo = serde_json::from_slice(&output.stdout)?;
            let (formulae, casks) = trusted_upgrades(info, &policy);
            let names: Vec<String> = formulae.into_iter().chain(casks).collect();
            if names.is_empty() {
                continue;
            }
            let output = brew_without_installed_scan()?
                .args(["upgrade", kind])
                .args(&names)
                .output()
                .await?;

            if !output.status.success() {
                let stderr = String::from_utf8_lossy(&output.stderr);
                failures.push(format!("brew upgrade {} failed: {}", kind, stderr.trim()));
            }
        }

        if !failures.is_empty() {
            return Err(anyhow::anyhow!(failures.join("; ")));
        }
        Ok(())
    }

    async fn uninstall(&self, package: &str) -> Result<()> {
        validate_name(Ecosystem::Brew, package)?;
        let output = command("brew")?
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
        let output = command("brew")?
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
            approved_from_taps: vec![(
                "brew_formulae".to_string(),
                "evil/tap/approved".to_string(),
                "evil/tap".to_string(),
            )],
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
                "evil/tap/approved".to_string(),
            ],
            casks: vec!["iterm2".to_string(), "evil/tap/app".to_string()],
        };
        let untrusted = packages.take_untrusted(&policy);
        assert_eq!(packages.taps, vec!["homebrew/cask", "oven-sh/bun"]);
        assert_eq!(
            packages.formulae,
            vec!["git", "oven-sh/bun/bun", "evil/tap/approved"]
        );
        assert_eq!(packages.casks, vec!["iterm2"]);
        // An approved formula trusts neither its tap nor the tap's other packages
        assert_eq!(untrusted.taps, vec!["evil/tap"]);
        assert_eq!(untrusted.formulae, vec!["evil/tap/payload"]);
        assert_eq!(untrusted.casks, vec!["evil/tap/app"]);
    }

    #[test]
    fn test_trusted_upgrades_skip_untrusted_taps() {
        let policy = PackagePolicy {
            min_release_age_days: 7,
            allow_scripts: Vec::new(),
            trusted_taps: vec!["oven-sh/bun".to_string()],
            approved_from_taps: Vec::new(),
        };
        let info: BrewInfo = serde_json::from_str(
            r#"{
              "formulae": [
                {"name": "wget", "full_name": "wget", "tap": "homebrew/core", "outdated": true, "pinned": false},
                {"name": "bun", "full_name": "oven-sh/bun/bun", "tap": "oven-sh/bun", "outdated": true, "pinned": false},
                {"name": "bd", "full_name": "evil/tap/bd", "tap": "evil/tap", "outdated": true, "pinned": false},
                {"name": "node", "full_name": "node", "tap": "homebrew/core", "outdated": false, "pinned": false},
                {"name": "jq", "full_name": "jq", "tap": "homebrew/core", "outdated": true, "pinned": true},
                {"name": "local", "full_name": "local", "tap": null, "outdated": true, "pinned": false}
              ],
              "casks": [
                {"token": "iterm2", "full_token": "iterm2", "tap": "homebrew/cask", "outdated": true},
                {"token": "app", "full_token": "evil/tap/app", "tap": "evil/tap", "outdated": true}
              ]
            }"#,
        )
        .unwrap();
        let (formulae, casks) = trusted_upgrades(info, &policy);
        assert_eq!(formulae, vec!["homebrew/core/wget", "oven-sh/bun/bun"]);
        assert_eq!(casks, vec!["homebrew/cask/iterm2"]);
    }

    #[test]
    fn trusted_names_reads_receipts_and_skips_untrusted_taps() {
        let dir = tempfile::tempdir().unwrap();
        let receipt = |path: &str, tap: Option<&str>| {
            let path = dir.path().join(path);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            let json = serde_json::json!({ "source": { "tap": tap } });
            std::fs::write(path, json.to_string()).unwrap();
        };
        receipt(
            "Cellar/wget/1.0/INSTALL_RECEIPT.json",
            Some("homebrew/core"),
        );
        receipt("Cellar/bun/1.3/INSTALL_RECEIPT.json", Some("oven-sh/bun"));
        receipt("Cellar/bd/0.1/INSTALL_RECEIPT.json", Some("evil/tap"));
        receipt("Cellar/mixed/1/INSTALL_RECEIPT.json", Some("homebrew/core"));
        receipt("Cellar/mixed/2/INSTALL_RECEIPT.json", Some("evil/tap"));
        receipt("Cellar/local/1/INSTALL_RECEIPT.json", None);
        receipt(
            "Caskroom/iterm2/.metadata/INSTALL_RECEIPT.json",
            Some("homebrew/cask"),
        );
        receipt(
            "Caskroom/app/.metadata/INSTALL_RECEIPT.json",
            Some("evil/tap"),
        );
        std::fs::create_dir_all(dir.path().join("Caskroom/bare")).unwrap();
        let policy = PackagePolicy {
            min_release_age_days: 7,
            allow_scripts: Vec::new(),
            trusted_taps: vec!["oven-sh/bun".to_string()],
            approved_from_taps: Vec::new(),
        };

        let formulae = trusted_names(installed_formulae(&dir.path().join("Cellar")), &policy);
        assert_eq!(formulae, vec!["oven-sh/bun/bun", "homebrew/core/wget"]);
        let casks = trusted_names(installed_casks(&dir.path().join("Caskroom")), &policy);
        assert_eq!(casks, vec!["homebrew/cask/iterm2"]);
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

    #[test]
    fn taps_providing_reads_files_without_loading_them() {
        let dir = tempfile::tempdir().unwrap();
        let tap = |path: &str, content: &str| {
            let path = dir.path().join(path);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, content).unwrap();
        };
        tap("oven-sh/homebrew-bun/Formula/bun.rb", "");
        tap("evil/homebrew-tap/Formula/b/bd.rb", "");
        tap("evil/homebrew-tap/Casks/a/app.rb", "");
        tap("flat/homebrew-tools/tool.rb", "");
        tap("flat/homebrew-tools/cmd/brew-x.rb", "");
        tap("alias/homebrew-tap/Formula/real.rb", "");
        tap("alias/homebrew-tap/Aliases/nick", "");
        tap(
            "moved/homebrew-tap/tap_migrations.json",
            r#"{"gone": "other/tap"}"#,
        );
        tap("other/homebrew-bun/HomebrewFormula/bun.rb", "");

        let taps = |name, cask| taps_providing(dir.path(), name, cask);
        assert_eq!(taps("bun", false), vec!["other/bun", "oven-sh/bun"]);
        assert_eq!(taps("bd", false), vec!["evil/tap"]);
        assert_eq!(taps("app", true), vec!["evil/tap"]);
        assert!(taps("app", false).is_empty());
        assert_eq!(taps("tool", false), vec!["flat/tools"]);
        assert!(taps("brew-x", false).is_empty());
        assert_eq!(taps("nick", false), vec!["alias/tap"]);
        assert_eq!(taps("gone", false), vec!["moved/tap"]);
        assert_eq!(taps("gone", true), vec!["moved/tap"]);
        assert!(taps("wget", false).is_empty());
    }
}

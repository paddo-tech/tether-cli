use crate::config::{Config, PackagesConfig};
use chrono::{DateTime, Utc};
use std::collections::HashSet;
use std::sync::Mutex;

/// Supply-chain settings every manager applies to installs and upgrades.
#[derive(Debug, Clone)]
pub struct PackagePolicy {
    pub min_release_age_days: u32,
    pub allow_scripts: Vec<String>,
    pub trusted_taps: Vec<String>,
}

impl Default for PackagePolicy {
    fn default() -> Self {
        Self::from_config(&Config::default().packages)
    }
}

impl PackagePolicy {
    pub fn from_config(packages: &PackagesConfig) -> Self {
        Self {
            min_release_age_days: packages.min_release_age_days,
            allow_scripts: packages.allow_scripts.clone(),
            trusted_taps: packages.brew.trusted_taps.clone(),
        }
    }

    /// Managers are built at many call sites without a config, so they read it here.
    /// An unreadable config falls back to the secure defaults.
    /// Taps approved in the inbox are trusted like configured ones.
    pub fn load() -> Self {
        let mut policy = Config::load()
            .map(|c| Self::from_config(&c.packages))
            .unwrap_or_default();
        if let Ok(inbox) = super::inbox::Inbox::load() {
            policy
                .trusted_taps
                .extend(inbox.approved_taps().map(str::to_string));
        }
        policy
    }

    pub fn scripts_allowed(&self, name: &str) -> bool {
        self.allow_scripts.iter().any(|n| n == name)
    }

    /// `homebrew/*` is Homebrew's own organisation and is always trusted.
    pub fn tap_trusted(&self, tap: &str) -> bool {
        let tap = tap.to_ascii_lowercase();
        tap.starts_with("homebrew/")
            || self
                .trusted_taps
                .iter()
                .any(|t| t.to_ascii_lowercase() == tap)
    }
}

/// How a manager enforces `min_release_age_days`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Cooldown {
    /// No release-age limit applies to this manager.
    Off,
    /// Flags that make the manager skip releases younger than the limit.
    Args(Vec<String>),
    /// The installed manager cannot enforce the limit.
    Unsupported,
}

impl Cooldown {
    pub fn args(&self) -> &[String] {
        match self {
            Cooldown::Args(args) => args,
            _ => &[],
        }
    }
}

/// Parse the first `x.y.z` token of `--version` output. The flag marks a prerelease.
pub fn parse_version(output: &str) -> Option<((u64, u64, u64), bool)> {
    let token = output
        .split_whitespace()
        .map(|t| t.trim_start_matches('v'))
        .find(|t| t.starts_with(|c: char| c.is_ascii_digit()))?;
    let (core, pre) = match token.split_once('-') {
        Some((core, _)) => (core, true),
        None => (token, false),
    };
    let mut parts = core.split('.').map(|p| p.parse::<u64>().ok());
    let major = parts.next()??;
    let minor = parts.next().flatten().unwrap_or(0);
    let patch = parts.next().flatten().unwrap_or(0);
    Some(((major, minor, patch), pre))
}

pub async fn tool_version(program: &str) -> Option<((u64, u64, u64), bool)> {
    let output = super::command(program)
        .ok()?
        .arg("--version")
        .output()
        .await
        .ok()?;
    if !output.status.success() {
        return None;
    }
    parse_version(&String::from_utf8_lossy(&output.stdout))
}

/// npm added `min-release-age` (days) in 11.10.0.
pub fn npm_cooldown(days: u32, version: Option<((u64, u64, u64), bool)>) -> Cooldown {
    if days == 0 {
        return Cooldown::Off;
    }
    match version {
        Some((v, _)) if v >= (11, 10, 0) => {
            Cooldown::Args(vec![format!("--min-release-age={}", days)])
        }
        _ => Cooldown::Unsupported,
    }
}

/// pnpm added `minimumReleaseAge` (minutes) in 10.16.0. The 12.0.0 prereleases before
/// rc.9 silently ignored it on the command line, so every 12.0.0 prerelease is refused.
/// pnpm 11.0.0 made the cutoff non-strict: it installs a too-new version when no mature
/// one matches and writes it to `minimumReleaseAgeExclude`. 12.0.0 to 12.2.x also ignore
/// that an explicit cutoff implies strict (pnpm/pnpm#14409), so strict is always passed.
/// Before 11.0.0 the cutoff is always strict and the strict key does not exist.
pub fn pnpm_cooldown(days: u32, version: Option<((u64, u64, u64), bool)>) -> Cooldown {
    if days == 0 {
        return Cooldown::Off;
    }
    match version {
        Some((v, pre)) if v >= (10, 16, 0) && !(v == (12, 0, 0) && pre) => {
            let mut args = vec![format!(
                "--config.minimum-release-age={}",
                u64::from(days) * 24 * 60
            )];
            if v >= (11, 0, 0) {
                args.push("--config.minimum-release-age-strict=true".to_string());
            }
            Cooldown::Args(args)
        }
        _ => Cooldown::Unsupported,
    }
}

/// pnpm 12.0.0 to 12.3.1 reject `update --ignore-scripts` (pnpm/pnpm#14512).
pub fn pnpm_update_accepts_ignore_scripts(version: Option<((u64, u64, u64), bool)>) -> bool {
    !matches!(version, Some((v, _)) if ((12, 0, 0)..(12, 3, 2)).contains(&v))
}

/// bun added `minimumReleaseAge` (seconds) in 1.3.0.
pub fn bun_cooldown(days: u32, version: Option<((u64, u64, u64), bool)>) -> Cooldown {
    if days == 0 {
        return Cooldown::Off;
    }
    match version {
        Some((v, _)) if v >= (1, 3, 0) => Cooldown::Args(vec![format!(
            "--minimum-release-age={}",
            u64::from(days) * 24 * 60 * 60
        )]),
        _ => Cooldown::Unsupported,
    }
}

/// uv saves `--exclude-newer` in the tool receipt, and later upgrades reuse it unless they
/// pass their own. uv 0.9.17 and later accept a duration, which the receipt keeps relative
/// (`exclude-newer-span`); older uv gets a timestamp, which each upgrade with the flag replaces.
/// uv 0.11.24 and later accept `false`, which clears a saved cutoff when the limit is 0.
/// On older uv a saved cutoff stays until `uv tool install --force <name>` reinstalls the tool.
pub fn uv_cooldown(
    days: u32,
    version: Option<((u64, u64, u64), bool)>,
    now: DateTime<Utc>,
) -> Cooldown {
    let v = version.map(|(v, _)| v);
    if days == 0 {
        return if v >= Some((0, 11, 24)) {
            Cooldown::Args(vec!["--exclude-newer".to_string(), "false".to_string()])
        } else {
            Cooldown::Off
        };
    }
    let value = if v >= Some((0, 9, 17)) {
        format!("{} days", days)
    } else {
        let cutoff = now - chrono::Duration::days(i64::from(days));
        cutoff.to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
    };
    Cooldown::Args(vec!["--exclude-newer".to_string(), value])
}

/// npm and pnpm run dependency scripts unless told not to, so scripts stay off
/// for every package outside `packages.allow_scripts`.
/// npm 12 blocks unreviewed install scripts on its own, so `--allow-scripts` runs only the
/// named package's. Older npm would also run every dependency's scripts, so an allowlisted
/// package keeps scripts off there.
pub fn npm_script_args(policy: &PackagePolicy, name: &str, npm_major: u64) -> Vec<String> {
    if policy.scripts_allowed(name) && npm_major >= 12 {
        vec![format!("--allow-scripts={}", name)]
    } else {
        vec!["--ignore-scripts".to_string()]
    }
}

/// pnpm 10.4 added `add --allow-build`, which approves the named package's builds and
/// records them in pnpm's global `allowBuilds`, so later updates need no flag. Older pnpm
/// cannot approve one package, so an allowlisted package keeps scripts off there.
pub fn pnpm_script_args(
    policy: &PackagePolicy,
    name: &str,
    version: Option<((u64, u64, u64), bool)>,
    adding: bool,
) -> Vec<String> {
    if !policy.scripts_allowed(name) || !pnpm_can_allow_build(version) {
        vec!["--ignore-scripts".to_string()]
    } else if adding {
        vec![format!("--allow-build={}", name)]
    } else {
        Vec::new()
    }
}

pub fn pnpm_can_allow_build(version: Option<((u64, u64, u64), bool)>) -> bool {
    version.is_some_and(|(v, _)| v >= (10, 4, 0))
}

/// Allowlisted packages whose manager version cannot limit scripts to them install with
/// scripts off, and the user is told once.
pub fn warn_scripts_unsupported_once(manager: &str, needs: &str) {
    if first_warning(&format!("allow_scripts {}", manager)) {
        eprintln!(
            "Warning: packages.allow_scripts needs {} {} or later. Scripts stay off for allowlisted {} packages",
            manager, needs, manager
        );
    }
}

/// bun skips dependency scripts unless the package is trusted, and `--ignore-scripts`
/// also stops its built-in trusted list.
pub fn bun_script_args(policy: &PackagePolicy, name: &str) -> Vec<String> {
    if policy.scripts_allowed(name) {
        vec!["--trust".to_string()]
    } else {
        vec!["--ignore-scripts".to_string()]
    }
}

static WARNED: Mutex<Option<HashSet<String>>> = Mutex::new(None);

/// True the first time `key` is seen in this process, so the daemon warns once, not daily.
pub fn first_warning(key: &str) -> bool {
    let mut warned = WARNED.lock().unwrap_or_else(|e| e.into_inner());
    warned
        .get_or_insert_with(HashSet::new)
        .insert(key.to_string())
}

pub fn warn_unsupported_once(manager: &str, days: u32) {
    if first_warning(manager) {
        eprintln!(
            "Warning: this {} version cannot enforce packages.min_release_age_days = {}. Upgrade {} to get the cooldown",
            manager, days, manager
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(s: &str) -> Option<((u64, u64, u64), bool)> {
        parse_version(s)
    }

    fn policy(allow: &[&str]) -> PackagePolicy {
        PackagePolicy {
            min_release_age_days: 7,
            allow_scripts: allow.iter().map(|s| s.to_string()).collect(),
            trusted_taps: vec!["oven-sh/bun".to_string()],
        }
    }

    #[test]
    fn parses_tool_versions() {
        assert_eq!(v("12.2.0\n"), Some(((12, 2, 0), false)));
        assert_eq!(
            v("uv 0.12.21 (Homebrew 2026-09-29 aarch64-apple-darwin)"),
            Some(((0, 12, 21), false))
        );
        assert_eq!(v("12.0.0-rc.6"), Some(((12, 0, 0), true)));
        assert_eq!(v("v1.3"), Some(((1, 3, 0), false)));
        assert_eq!(v("garbage"), None);
    }

    #[test]
    fn npm_cooldown_in_days_from_11_10() {
        assert_eq!(
            npm_cooldown(7, v("11.10.0")),
            Cooldown::Args(vec!["--min-release-age=7".to_string()])
        );
        assert_eq!(npm_cooldown(7, v("11.9.4")), Cooldown::Unsupported);
        assert_eq!(npm_cooldown(7, None), Cooldown::Unsupported);
        assert_eq!(npm_cooldown(0, v("10.0.0")), Cooldown::Off);
    }

    #[test]
    fn pnpm_cooldown_in_minutes_from_10_16() {
        assert_eq!(
            pnpm_cooldown(7, v("10.16.0")),
            Cooldown::Args(vec!["--config.minimum-release-age=10080".to_string()])
        );
        assert_eq!(pnpm_cooldown(7, v("10.15.1")), Cooldown::Unsupported);
        assert_eq!(pnpm_cooldown(7, v("12.0.0-rc.6")), Cooldown::Unsupported);
        assert_eq!(pnpm_cooldown(0, None), Cooldown::Off);
    }

    #[test]
    fn pnpm_cooldown_strict_from_11() {
        let strict = Cooldown::Args(vec![
            "--config.minimum-release-age=10080".to_string(),
            "--config.minimum-release-age-strict=true".to_string(),
        ]);
        assert_eq!(pnpm_cooldown(7, v("11.0.0")), strict);
        assert_eq!(pnpm_cooldown(7, v("12.2.1")), strict);
        assert_eq!(pnpm_cooldown(7, v("12.8.1")), strict);
    }

    #[test]
    fn pnpm_update_ignore_scripts_gap() {
        assert!(pnpm_update_accepts_ignore_scripts(v("11.6.0")));
        assert!(!pnpm_update_accepts_ignore_scripts(v("12.0.0")));
        assert!(!pnpm_update_accepts_ignore_scripts(v("12.3.1")));
        assert!(pnpm_update_accepts_ignore_scripts(v("12.3.2")));
        assert!(pnpm_update_accepts_ignore_scripts(v("12.8.1")));
    }

    #[test]
    fn bun_cooldown_in_seconds_from_1_3() {
        assert_eq!(
            bun_cooldown(7, v("1.4.2")),
            Cooldown::Args(vec!["--minimum-release-age=604800".to_string()])
        );
        assert_eq!(bun_cooldown(7, v("1.2.23")), Cooldown::Unsupported);
        assert_eq!(bun_cooldown(0, None), Cooldown::Off);
    }

    #[test]
    fn uv_cooldown_duration_or_cutoff() {
        let now = DateTime::parse_from_rfc3339("2026-10-02T12:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let args =
            |value: &str| Cooldown::Args(vec!["--exclude-newer".to_string(), value.to_string()]);
        assert_eq!(uv_cooldown(7, v("uv 0.12.21"), now), args("7 days"));
        assert_eq!(uv_cooldown(7, v("uv 0.9.17"), now), args("7 days"));
        assert_eq!(
            uv_cooldown(7, v("uv 0.9.16"), now),
            args("2026-09-25T12:00:00Z")
        );
        assert_eq!(uv_cooldown(7, None, now), args("2026-09-25T12:00:00Z"));
        assert_eq!(uv_cooldown(0, v("uv 0.11.24"), now), args("false"));
        assert_eq!(uv_cooldown(0, v("uv 0.11.23"), now), Cooldown::Off);
    }

    #[test]
    fn scripts_off_unless_allowlisted() {
        let p = policy(&["esbuild"]);
        assert_eq!(
            npm_script_args(&p, "left-pad", 12),
            vec!["--ignore-scripts"]
        );
        assert_eq!(
            npm_script_args(&p, "esbuild", 12),
            vec!["--allow-scripts=esbuild"]
        );
        assert_eq!(npm_script_args(&p, "esbuild", 11), vec!["--ignore-scripts"]);
        let pnpm12 = v("12.8.1");
        assert_eq!(
            pnpm_script_args(&p, "left-pad", pnpm12, true),
            vec!["--ignore-scripts"]
        );
        assert_eq!(
            pnpm_script_args(&p, "esbuild", pnpm12, true),
            vec!["--allow-build=esbuild"]
        );
        assert!(pnpm_script_args(&p, "esbuild", pnpm12, false).is_empty());
        assert_eq!(
            pnpm_script_args(&p, "esbuild", v("10.3.0"), true),
            vec!["--ignore-scripts"]
        );
        assert_eq!(bun_script_args(&p, "left-pad"), vec!["--ignore-scripts"]);
        assert_eq!(bun_script_args(&p, "esbuild"), vec!["--trust"]);
    }

    #[test]
    fn homebrew_taps_always_trusted() {
        let p = policy(&[]);
        assert!(p.tap_trusted("homebrew/core"));
        assert!(p.tap_trusted("Homebrew/cask"));
        assert!(p.tap_trusted("oven-sh/bun"));
        assert!(!p.tap_trusted("evil/tap"));
    }

    #[test]
    fn default_policy_is_secure() {
        let p = PackagePolicy::default();
        assert_eq!(p.min_release_age_days, 7);
        assert!(p.allow_scripts.is_empty());
        assert!(p.trusted_taps.is_empty());
    }

    #[test]
    fn first_warning_fires_once() {
        assert!(first_warning("test-manager-once"));
        assert!(!first_warning("test-manager-once"));
    }
}

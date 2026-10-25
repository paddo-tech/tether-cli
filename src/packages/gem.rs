use super::command;
use super::policy::PackagePolicy;
use super::resolve::resolve_version;
use super::{
    validate_name, validate_version, Cooldown, Ecosystem, PackageInfo, PackageManager, Upgrade,
};
use anyhow::Result;
use async_trait::async_trait;

/// Gems from `gem outdated` lines like `rdoc (7.0.4 < 8.1.0)`.
fn parse_outdated(stdout: &str) -> Vec<Upgrade> {
    stdout
        .lines()
        .filter_map(|line| {
            let (name, versions) = line.trim().split_once(" (")?;
            let (current, latest) = versions.strip_suffix(')')?.split_once(" < ")?;
            Some(Upgrade::new(name, Some(current), latest))
        })
        .collect()
}

/// gem's stderr from its first `ERROR:` line, without Ruby backtrace frames. gem prints load
/// warnings first, often hundreds of lines. A native build failure puts its compiler output
/// and log path after a blank line, and a later gem can fail on its own, so the rest stays.
/// Without an `ERROR:` line, all of it.
fn gem_error(stderr: &str) -> String {
    let Some(start) = stderr.lines().position(|l| l.starts_with("ERROR:")) else {
        return stderr.trim().to_string();
    };
    stderr
        .lines()
        .skip(start)
        .filter(|l| {
            let l = l.trim_start();
            !((l.starts_with('/') || l.starts_with("<internal:")) && l.contains(":in "))
        })
        .collect::<Vec<_>>()
        .join("\n")
        .trim_end()
        .to_string()
}

pub struct GemManager;

impl GemManager {
    pub fn new() -> Self {
        Self
    }

    fn policy(&self) -> PackagePolicy {
        PackagePolicy::load()
    }

    async fn run_ruby(&self, script: &str) -> Result<String> {
        let output = command("ruby")?.args(["-e", script]).output().await?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(anyhow::anyhow!("gem listing failed: {}", stderr));
        }
        Ok(String::from_utf8(output.stdout)?)
    }

    async fn run_gem(&self, args: &[&str]) -> Result<String> {
        let output = command("gem")?.args(args).output().await?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(anyhow::anyhow!(
                "gem command failed: {}",
                gem_error(&stderr)
            ));
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
        let mut packages = Vec::new();

        for line in self.run_ruby(TOP_LEVEL_GEMS).await?.lines() {
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

    /// The listing leaves out dependencies and gems that ship with Ruby, and
    /// `gem install --conservative` skips them. All installed gems, listed ones included
    async fn installed_names(&self) -> Result<std::collections::HashSet<String>> {
        Ok(self
            .run_ruby("puts Gem::Specification.map(&:name).uniq")
            .await?
            .lines()
            .map(str::to_string)
            .collect())
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
        // --no-document skips RubyGems' rdoc hook, which crashes when two rdoc versions load
        let mut args = vec![
            "install",
            pkg_spec.as_str(),
            "--conservative",
            "--remote",
            "--no-document",
        ];
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

    async fn upgrades_hold_release_age(&self) -> bool {
        true
    }

    async fn upgrade(&self, planned: &[Upgrade]) -> Result<Vec<String>> {
        // Each gem installs at its planned target, which may be older than the newest release
        let specs: Vec<String> = planned
            .iter()
            .filter(|u| {
                validate_name(Ecosystem::Gem, &u.name).is_ok()
                    && validate_version(Ecosystem::Gem, &u.target).is_ok()
            })
            .map(|u| format!("{}:{}", u.name, u.target))
            .collect();
        if specs.is_empty() {
            return Ok(Vec::new());
        }
        let output = command("gem")?
            .arg("install")
            .args(&specs)
            // By default gem also upgrades dependencies that already fit, past the plan
            .args(["--remote", "--no-document", "--minimal-deps"])
            .args(Self::user_install_flag())
            .output()
            .await?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(anyhow::anyhow!(
                "gem install failed: {}",
                gem_error(&stderr)
            ));
        }

        Ok(Vec::new())
    }

    /// With a release-age limit, the target is the newest release that RubyGems published
    /// before the limit, because gem itself cannot hold to it.
    async fn upgrade_candidates(&self) -> Result<Vec<Upgrade>> {
        let outdated = parse_outdated(&self.run_gem(&["outdated", "--remote"]).await?);
        let min_age = self.policy().min_release_age_days;
        if min_age == 0 {
            return Ok(outdated);
        }
        let mut candidates = Vec::new();
        // One gem that fails its check is skipped, so the others still upgrade
        for u in outdated {
            match resolve_version("gem", Ecosystem::Gem, &u.name, min_age).await {
                Ok(target) => candidates.push(Upgrade::new(&u.name, u.current.as_deref(), &target)),
                Err(e) => {
                    crate::cli::Output::warning(&format!("gem: could not check {}: {}", u.name, e))
                }
            }
        }
        Ok(candidates)
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
    fn a_gem_error_skips_warnings_and_the_backtrace() {
        let stderr = "warning: already initialized constant RDoc::VERSION\n\
                      Ignoring json-3.0.2 because its extensions are not built.\n\
                      ERROR:  While executing gem ... (ArgumentError)\n    \
                      wrong number of arguments (ArgumentError)\n\n    \
                      caller: rdoc/markup/to_joined_paragraph.rb:13\n        \
                      /opt/rdoc/markup/formatter.rb:46:in 'RDoc::Markup::Formatter#initialize'\n        \
                      <internal:kernel>:9:in 'block'\n";
        assert_eq!(
            gem_error(stderr),
            "ERROR:  While executing gem ... (ArgumentError)\n    wrong number of arguments (ArgumentError)\n\n    caller: rdoc/markup/to_joined_paragraph.rb:13"
        );
        // A native build failure keeps its output after the blank line, and a later error
        let build = "WARN: ERROR: in a warning\nERROR:  Error installing pg:\n\tERROR: Failed to build gem native extension.\n\n    /opt/ruby/bin/ruby extconf.rb\nchecking for pg_config... no\nResults logged to /x/gem_make.out\nERROR:  rake: download failed\n";
        assert_eq!(
            gem_error(build),
            "ERROR:  Error installing pg:\n\tERROR: Failed to build gem native extension.\n\n    /opt/ruby/bin/ruby extconf.rb\nchecking for pg_config... no\nResults logged to /x/gem_make.out\nERROR:  rake: download failed"
        );
        assert_eq!(gem_error("  plain failure \n"), "plain failure");
    }

    #[test]
    fn outdated_lines_yield_latest_versions() {
        let stdout = "rdoc (7.0.4 < 8.1.0)\nnet-imap (0.5.1 < 0.5.6)\nnoise\n";
        assert_eq!(
            parse_outdated(stdout),
            vec![
                Upgrade::new("rdoc", Some("7.0.4"), "8.1.0"),
                Upgrade::new("net-imap", Some("0.5.1"), "0.5.6")
            ]
        );
    }
}

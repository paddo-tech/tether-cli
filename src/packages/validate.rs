use anyhow::{bail, Result};

/// Naming rules differ per registry, so each manager validates against its ecosystem.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ecosystem {
    /// npm, pnpm and bun share the npm registry
    Npm,
    /// Homebrew formulae and casks, optionally qualified as `user/repo/name`
    Brew,
    /// Homebrew taps (`user/repo`)
    BrewTap,
    Python,
    Gem,
}

// npm's own limit; no registry in scope allows longer names
const MAX_NAME_LEN: usize = 214;

/// Reject any manifest entry that a package manager could read as a flag, URL, path,
/// tarball or non-registry spec. Manifest lines arrive from other machines, so only
/// plain registry names may reach a process.
pub fn validate_name(ecosystem: Ecosystem, name: &str) -> Result<()> {
    if let Err(reason) = check_name(ecosystem, name) {
        bail!("invalid package name {:?}: {}", name, reason);
    }
    Ok(())
}

/// Versions are appended to names (`name@1.0.0`), so they must not smuggle in a spec.
pub fn validate_version(version: &str) -> Result<()> {
    let ok = !version.is_empty()
        && version.len() <= MAX_NAME_LEN
        && version
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_alphanumeric())
        && version
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '+' | '_'));
    if !ok {
        bail!("invalid package version {:?}", version);
    }
    Ok(())
}

fn check_name(ecosystem: Ecosystem, name: &str) -> std::result::Result<(), &'static str> {
    if name.is_empty() {
        return Err("empty");
    }
    if name.len() > MAX_NAME_LEN {
        return Err("too long");
    }
    if name.starts_with('-') {
        return Err("starts with '-'");
    }
    if name.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return Err("contains whitespace or control characters");
    }
    if name.contains(':') {
        return Err("URLs and protocol specs are not allowed");
    }

    match ecosystem {
        Ecosystem::Npm => check_npm(name),
        Ecosystem::Brew => {
            let parts: Vec<&str> = name.split('/').collect();
            match parts.as_slice() {
                [formula] => check_segment(formula, is_brew_char),
                [user, repo, formula] => {
                    check_segment(user, is_tap_char)?;
                    check_segment(repo, is_tap_char)?;
                    check_segment(formula, is_brew_char)
                }
                _ => Err("expected 'name' or 'user/repo/name'"),
            }
        }
        Ecosystem::BrewTap => match name.split('/').collect::<Vec<_>>().as_slice() {
            [user, repo] => {
                check_segment(user, is_tap_char)?;
                check_segment(repo, is_tap_char)
            }
            _ => Err("expected 'user/repo'"),
        },
        Ecosystem::Python => {
            check_segment(name, is_python_char)?;
            // PEP 508 names end with a letter or digit
            if !name.ends_with(|c: char| c.is_ascii_alphanumeric()) {
                return Err("must end with a letter or digit");
            }
            Ok(())
        }
        Ecosystem::Gem => check_segment(name, is_gem_char),
    }
}

fn check_npm(name: &str) -> std::result::Result<(), &'static str> {
    // npm reads names ending in a tarball extension as local file specs
    if [".tgz", ".tar", ".tar.gz"]
        .iter()
        .any(|ext| name.ends_with(ext))
    {
        return Err("tarballs are not allowed");
    }
    let bare = match name.strip_prefix('@') {
        Some(scoped) => {
            let (scope, bare) = scoped
                .split_once('/')
                .ok_or("scoped names need '@scope/name'")?;
            check_segment(scope, is_npm_char)?;
            bare
        }
        // An unscoped 'user/repo' is GitHub shorthand to npm
        None if name.contains('/') => return Err("paths and GitHub shorthand are not allowed"),
        None => name,
    };
    if bare.contains('/') {
        return Err("paths are not allowed");
    }
    check_segment(bare, is_npm_char)
}

/// A segment starts with a letter or digit, which also rules out '.', '..' and hidden paths.
fn check_segment(
    segment: &str,
    allowed: fn(char) -> bool,
) -> std::result::Result<(), &'static str> {
    if !segment.starts_with(|c: char| c.is_ascii_alphanumeric()) {
        return Err("must start with a letter or digit");
    }
    if !segment.chars().all(allowed) {
        return Err("contains characters the registry does not allow");
    }
    Ok(())
}

fn is_npm_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, '-' | '.' | '_' | '~')
}

fn is_brew_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, '-' | '.' | '_' | '+' | '@')
}

fn is_tap_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, '-' | '.' | '_')
}

fn is_python_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, '-' | '.' | '_')
}

fn is_gem_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, '-' | '.' | '_')
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ok(eco: Ecosystem, name: &str) -> bool {
        validate_name(eco, name).is_ok()
    }

    #[test]
    fn npm_accepts_plain_and_scoped_names() {
        for name in [
            "typescript",
            "@google/gemini-cli",
            "lodash.merge",
            "@types/node",
            "a~b",
        ] {
            assert!(ok(Ecosystem::Npm, name), "{name}");
        }
    }

    #[test]
    fn npm_rejects_specs_paths_and_tarballs() {
        for name in [
            "-g",
            "--ignore-scripts=false",
            "git+https://github.com/x/y.git",
            "github:user/repo",
            "file:../evil",
            "link:../evil",
            "https://example.com/x.tgz",
            "user/repo",
            "./local",
            "../up",
            "/abs/path",
            "evil.tgz",
            "evil.tar.gz",
            "@scope",
            "@scope/a/b",
            "@./x",
            ".hidden",
            "_private",
            "has space",
            "tab\tname",
            "nl\nname",
            "",
        ] {
            assert!(!ok(Ecosystem::Npm, name), "{name:?}");
        }
    }

    #[test]
    fn brew_accepts_formulae_casks_and_qualified_names() {
        for name in [
            "git",
            "gtk+3",
            "python@3.12",
            "oven-sh/bun/bun",
            "font-fira-code",
        ] {
            assert!(ok(Ecosystem::Brew, name), "{name}");
        }
    }

    #[test]
    fn brew_rejects_bad_names() {
        for name in [
            "--HEAD",
            "user/repo",
            "a/b/c/d",
            "../x",
            "https://x/y.rb",
            "./Formula/x.rb",
            "x y",
        ] {
            assert!(!ok(Ecosystem::Brew, name), "{name:?}");
        }
    }

    #[test]
    fn brew_tap_requires_user_repo() {
        assert!(ok(Ecosystem::BrewTap, "homebrew/core"));
        assert!(ok(Ecosystem::BrewTap, "oven-sh/bun"));
        assert!(!ok(Ecosystem::BrewTap, "bun"));
        assert!(!ok(Ecosystem::BrewTap, "a/b/c"));
        assert!(!ok(Ecosystem::BrewTap, "--force"));
        assert!(!ok(Ecosystem::BrewTap, "../x"));
    }

    #[test]
    fn python_follows_pep_508() {
        for name in ["ruff", "black", "zope.interface", "my_tool-2"] {
            assert!(ok(Ecosystem::Python, name), "{name}");
        }
        for name in [
            "-e",
            "git+https://x/y",
            "./pkg",
            "pkg-",
            "pkg[extra]",
            "pkg==1.0",
        ] {
            assert!(!ok(Ecosystem::Python, name), "{name:?}");
        }
    }

    #[test]
    fn gem_names() {
        assert!(ok(Ecosystem::Gem, "rails"));
        assert!(ok(Ecosystem::Gem, "net-http_persistent.x"));
        for name in ["--source", "../x", "x/y", "http://x", "a b"] {
            assert!(!ok(Ecosystem::Gem, name), "{name:?}");
        }
    }

    #[test]
    fn versions() {
        for v in ["1.2.3", "1.0.0-beta.1+build.5", "latest", "24.10.0"] {
            assert!(validate_version(v).is_ok(), "{v}");
        }
        for v in ["", "-1", "file:../x", "1.0 || 2.0", "^1.0", "../x"] {
            assert!(validate_version(v).is_err(), "{v:?}");
        }
    }
}

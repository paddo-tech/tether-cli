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

const ALL_ECOSYSTEMS: [Ecosystem; 5] = [
    Ecosystem::Npm,
    Ecosystem::Brew,
    Ecosystem::BrewTap,
    Ecosystem::Python,
    Ecosystem::Gem,
];

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
/// Only a concrete release passes: with a dist-tag such as `latest` or a range, the registry
/// picks the release, and OSV cannot check it.
pub fn validate_version(ecosystem: Ecosystem, version: &str) -> Result<()> {
    let ok = version.len() <= MAX_NAME_LEN
        && match ecosystem {
            Ecosystem::Npm => is_semver(version),
            Ecosystem::Python => is_pep440(version),
            Ecosystem::Gem => is_gem_version(version),
            Ecosystem::Brew | Ecosystem::BrewTap => false,
        }
        // npm reads `name@x.tgz` as a local file, so versions get the same suffix check
        && !ALL_ECOSYSTEMS.iter().any(|&eco| {
            local_file_suffixes(eco)
                .iter()
                .any(|ext| version.to_ascii_lowercase().ends_with(ext))
        });
    if !ok {
        bail!("invalid package version {:?}", version);
    }
    Ok(())
}

fn is_digits(s: &str) -> bool {
    !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit())
}

/// Dot-separated identifiers of letters, digits and `-`, as in a semver prerelease or build.
fn is_idents(s: &str) -> bool {
    s.split('.').all(|part| {
        !part.is_empty() && part.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
    })
}

/// `MAJOR.MINOR.PATCH`, with an optional `-prerelease` and `+build`.
fn is_semver(v: &str) -> bool {
    let (v, build) = v.split_once('+').map_or((v, None), |(v, b)| (v, Some(b)));
    let (core, pre) = v.split_once('-').map_or((v, None), |(c, p)| (c, Some(p)));
    let parts: Vec<&str> = core.split('.').collect();
    parts.len() == 3
        && parts.iter().all(|p| is_digits(p))
        && pre.is_none_or(is_idents)
        && build.is_none_or(is_idents)
}

/// A normalized PEP 440 version, as uv prints it:
/// `[N!]N(.N)*[{a|b|rc}N][.postN][.devN][+local]`.
fn is_pep440(v: &str) -> bool {
    let (v, local) = v.split_once('+').map_or((v, None), |(v, l)| (v, Some(l)));
    let local_ok = local.is_none_or(|l| {
        l.split('.').all(|part| {
            !part.is_empty()
                && part
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
        })
    });
    let v = match v.split_once('!') {
        Some((epoch, rest)) if is_digits(epoch) => rest,
        Some(_) => return false,
        None => v,
    };
    let digits = |s: &str| s.bytes().take_while(u8::is_ascii_digit).count();
    if digits(v) == 0 {
        return false;
    }
    let mut rest = &v[digits(v)..];
    while let Some(after) = rest.strip_prefix('.') {
        let n = digits(after);
        if n == 0 {
            break;
        }
        rest = &after[n..];
    }
    for markers in [&["a", "b", "rc"][..], &[".post"], &[".dev"]] {
        if let Some(after) = markers.iter().find_map(|m| rest.strip_prefix(m)) {
            let n = digits(after);
            if n == 0 {
                return false;
            }
            rest = &after[n..];
        }
    }
    local_ok && rest.is_empty()
}

/// RubyGems' version pattern: `N(.X)*` with an optional `-prerelease`, where each X is
/// letters or digits.
fn is_gem_version(v: &str) -> bool {
    let (core, pre) = v.split_once('-').map_or((v, None), |(c, p)| (c, Some(p)));
    let mut parts = core.split('.');
    parts.next().is_some_and(is_digits)
        && parts.all(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_alphanumeric()))
        && pre.is_none_or(is_idents)
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
    let lower = name.to_ascii_lowercase();
    if local_file_suffixes(ecosystem)
        .iter()
        .any(|ext| lower.ends_with(ext))
    {
        return Err("local package files are not allowed");
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

/// Suffixes each tool reads as a local file instead of a registry name, matched without case.
/// npm-package-arg uses /[.](?:tgz|tar\.gz|tar)$/i; gem installs `x.gem`; brew installs
/// `.rb`/`.json` formula files and bottle tarballs; uv installs sdists and wheels.
fn local_file_suffixes(ecosystem: Ecosystem) -> &'static [&'static str] {
    match ecosystem {
        Ecosystem::Npm => &[".tgz", ".tar", ".tar.gz"],
        Ecosystem::Gem => &[".gem"],
        Ecosystem::Brew => &[".rb", ".json", ".tar.gz"],
        Ecosystem::BrewTap => &[],
        Ecosystem::Python => &[".tar.gz", ".whl", ".zip"],
    }
}

fn check_npm(name: &str) -> std::result::Result<(), &'static str> {
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
            "evil.TGZ",
            "evil.tar.gz",
            "evil.Tar.Gz",
            "evil.tar",
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
            "payload.rb",
            "payload.RB",
            "formula.json",
            "git--2.0.arm64_sonoma.bottle.tar.gz",
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
            "pkg-1.0.tar.gz",
            "pkg-1.0-py3-none-any.whl",
            "pkg.WHL",
            "pkg.zip",
        ] {
            assert!(!ok(Ecosystem::Python, name), "{name:?}");
        }
    }

    #[test]
    fn gem_names() {
        assert!(ok(Ecosystem::Gem, "rails"));
        assert!(ok(Ecosystem::Gem, "net-http_persistent.x"));
        for name in [
            "--source", "../x", "x/y", "http://x", "a b", "x.gem", "x.GEM",
        ] {
            assert!(!ok(Ecosystem::Gem, name), "{name:?}");
        }
    }

    #[test]
    fn versions_must_be_concrete_releases() {
        let ok = |eco, v| validate_version(eco, v).is_ok();
        for v in ["1.2.3", "1.0.0-beta.1+build.5", "24.10.0", "0.0.1-rc-1"] {
            assert!(ok(Ecosystem::Npm, v), "{v}");
        }
        for v in [
            "0.6.0",
            "1.0rc1",
            "2.0.post1",
            "1!2.0",
            "1.0.dev3",
            "1.0+local.7",
            "24",
        ] {
            assert!(ok(Ecosystem::Python, v), "{v}");
        }
        for v in ["8.0.1", "2.0.0.pre1", "1.0.a", "1.0-beta.2", "3"] {
            assert!(ok(Ecosystem::Gem, v), "{v}");
        }
        for v in [
            "",
            "latest",
            "next",
            "x",
            "1",
            "1.2",
            "1.2.x",
            "-1",
            "v1.2.3",
            "file:../x",
            "1.0.0 || 2.0.0",
            "^1.0.0",
            "~1.0.0",
            ">=1.0.0",
            "../x",
            "1.0.0-x.tgz",
            "1.0.0-evil.TGZ",
            "1.0.0-a.tar",
        ] {
            assert!(!ok(Ecosystem::Npm, v), "{v:?}");
        }
        for v in [
            "latest",
            "==1.0",
            ">=1.0",
            "1.0.whl",
            "1.0rc",
            "a1",
            "1.0+Local",
        ] {
            assert!(!ok(Ecosystem::Python, v), "{v:?}");
        }
        for v in ["latest", "~> 1.0", ">= 1.0", "x.gem", "1..0", "1.0-x.gem"] {
            assert!(!ok(Ecosystem::Gem, v), "{v:?}");
        }
        assert!(!ok(Ecosystem::Brew, "1.0.0"));
    }
}

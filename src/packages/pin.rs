use super::Ecosystem;
use std::cmp::Ordering;

/// An install spec that pins a version in each tool's own syntax: `name@1.2.3` for the npm
/// registry, `name==1.2.3` for uv, `name:1.2.3` for gem. Homebrew installs only the current
/// release, so it gets the name. Never write these to a manifest: 1.x reads a manifest line
/// as an installed name.
pub fn format_pin(ecosystem: Ecosystem, name: &str, version: Option<&str>) -> String {
    match (version, separator(ecosystem)) {
        (Some(version), Some(sep)) => format!("{}{}{}", name, sep, version),
        _ => name.to_string(),
    }
}

/// Split an install spec into name and pinned version. Manifest lines carry only a name,
/// but pre-release 2.0 builds wrote pinned manifest lines, so those parse too.
pub fn parse_pin(ecosystem: Ecosystem, line: &str) -> (String, Option<String>) {
    let line = line.trim();
    let Some(sep) = separator(ecosystem) else {
        return (line.to_string(), None);
    };
    match line.rsplit_once(sep) {
        // A leading '@' starts an npm scope, not a version
        Some((name, version)) if !name.is_empty() && !version.is_empty() => {
            (name.to_string(), Some(version.to_string()))
        }
        _ => (line.to_string(), None),
    }
}

/// Package names in a manifest, without versions.
pub fn manifest_names(ecosystem: Ecosystem, content: &str) -> Vec<String> {
    content
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(|l| parse_pin(ecosystem, l).0)
        .collect()
}

fn separator(ecosystem: Ecosystem) -> Option<&'static str> {
    match ecosystem {
        Ecosystem::Npm => Some("@"),
        Ecosystem::Python => Some("=="),
        Ecosystem::Gem => Some(":"),
        Ecosystem::Brew | Ecosystem::BrewTap => None,
    }
}

/// Order two versions by their ecosystem's rules: semver for the npm registry, PEP 440 for
/// uv, and `Gem::Version` for gem. Callers pass versions that `validate_version` accepted;
/// these comparators only cover the forms it accepts.
pub fn compare_versions(ecosystem: Ecosystem, a: &str, b: &str) -> Ordering {
    match ecosystem {
        Ecosystem::Python => pep440_key(a).cmp(&pep440_key(b)),
        Ecosystem::Gem => compare_gem(a, b),
        Ecosystem::Npm | Ecosystem::Brew | Ecosystem::BrewTap => compare_semver(a, b),
    }
}

/// A semver prerelease identifier. Numeric identifiers sort below alphanumeric ones.
#[derive(PartialEq, Eq, PartialOrd, Ord)]
enum SemverIdent<'a> {
    Num(u64),
    Alpha(&'a str),
}

/// Build metadata does not count, and a release sorts above its prereleases.
fn compare_semver(a: &str, b: &str) -> Ordering {
    fn split(v: &str) -> (Vec<u64>, Option<Vec<SemverIdent<'_>>>) {
        let v = v.split_once('+').map_or(v, |(v, _)| v);
        let (core, pre) = v.split_once('-').map_or((v, None), |(c, p)| (c, Some(p)));
        let core = core.split('.').map(|p| p.parse().unwrap_or(0)).collect();
        let pre = pre.map(|p| {
            p.split('.')
                .map(|id| match id.parse() {
                    Ok(n) if id.bytes().all(|b| b.is_ascii_digit()) => SemverIdent::Num(n),
                    _ => SemverIdent::Alpha(id),
                })
                .collect()
        });
        (core, pre)
    }
    let ((a_core, a_pre), (b_core, b_pre)) = (split(a), split(b));
    a_core.cmp(&b_core).then_with(|| match (a_pre, b_pre) {
        (None, None) => Ordering::Equal,
        (None, Some(_)) => Ordering::Greater,
        (Some(_), None) => Ordering::Less,
        (Some(x), Some(y)) => x.cmp(&y),
    })
}

/// A PEP 440 local version segment. Numeric segments sort above alphanumeric ones.
#[derive(PartialEq, Eq, PartialOrd, Ord)]
enum LocalSegment {
    Alpha(String),
    Num(u64),
}

/// Sort key for a normalized PEP 440 version:
/// epoch, release without trailing zeros, pre, post, dev, local.
/// `1.0.dev1 < 1.0a1 < 1.0rc1 < 1.0 < 1.0.post1 < 1.0+local`.
type Pep440Key = (
    u64,
    Vec<u64>,
    (u8, u64),
    Option<u64>,
    (u8, u64),
    Option<Vec<LocalSegment>>,
);

fn pep440_key(v: &str) -> Pep440Key {
    fn number(s: &str) -> (u64, &str) {
        let n = s.bytes().take_while(u8::is_ascii_digit).count();
        (s[..n].parse().unwrap_or(0), &s[n..])
    }
    let (v, local) = v.split_once('+').map_or((v, None), |(v, l)| (v, Some(l)));
    let (epoch, mut rest) = match v.split_once('!') {
        Some((epoch, rest)) => (epoch.parse().unwrap_or(0), rest),
        None => (0, v),
    };
    let mut release = Vec::new();
    loop {
        let (n, after) = number(rest);
        release.push(n);
        match after.strip_prefix('.') {
            Some(next) if next.starts_with(|c: char| c.is_ascii_digit()) => rest = next,
            _ => {
                rest = after;
                break;
            }
        }
    }
    while release.len() > 1 && release.last() == Some(&0) {
        release.pop();
    }
    let mut marker = |tag: &str| {
        rest.strip_prefix(tag).map(|after| {
            let (n, after) = number(after);
            rest = after;
            n
        })
    };
    let pre = [("a", 1), ("b", 2), ("rc", 3)]
        .into_iter()
        .find_map(|(tag, rank)| marker(tag).map(|n| (rank, n)));
    let post = marker(".post");
    let dev = marker(".dev");
    let pre = match (pre, post, dev) {
        (Some(pre), _, _) => pre,
        // A dev release of the final release sorts below its prereleases
        (None, None, Some(_)) => (0, 0),
        (None, _, _) => (4, 0),
    };
    let dev = dev.map_or((1, 0), |n| (0, n));
    let local = local.map(|l| {
        l.split('.')
            .map(|s| match s.parse() {
                Ok(n) => LocalSegment::Num(n),
                Err(_) => LocalSegment::Alpha(s.to_ascii_lowercase()),
            })
            .collect()
    });
    (epoch, release, pre, post, dev, local)
}

/// A `Gem::Version` segment. Ruby compares a string segment below any number.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord)]
enum GemSegment {
    Alpha(String),
    Num(u64),
}

/// `Gem::Version#<=>`: a missing segment counts as 0.
fn compare_gem(a: &str, b: &str) -> Ordering {
    let (a, b) = (gem_segments(a), gem_segments(b));
    let zero = GemSegment::Num(0);
    (0..a.len().max(b.len()))
        .map(|i| a.get(i).unwrap_or(&zero).cmp(b.get(i).unwrap_or(&zero)))
        .find(|order| order.is_ne())
        .unwrap_or(Ordering::Equal)
}

/// `Gem::Version#canonical_segments`: digit and letter runs, with `-` read as `.pre.`, and
/// trailing zeros dropped from the numeric part and from the part from the first letters on.
fn gem_segments(v: &str) -> Vec<GemSegment> {
    let v = v.replace('-', ".pre.");
    let mut segments = Vec::new();
    let mut rest = v.as_str();
    while let Some(start) = rest.find(|c: char| c.is_ascii_alphanumeric()) {
        rest = &rest[start..];
        let digit = rest.starts_with(|c: char| c.is_ascii_digit());
        let n = rest
            .find(|c: char| !c.is_ascii_alphanumeric() || c.is_ascii_digit() != digit)
            .unwrap_or(rest.len());
        let (run, after) = rest.split_at(n);
        segments.push(if digit {
            GemSegment::Num(run.parse().unwrap_or(0))
        } else {
            GemSegment::Alpha(run.to_string())
        });
        rest = after;
    }
    let first_alpha = segments
        .iter()
        .position(|s| matches!(s, GemSegment::Alpha(_)))
        .unwrap_or(segments.len());
    let mut alpha = segments.split_off(first_alpha);
    for part in [&mut segments, &mut alpha] {
        while part.last() == Some(&GemSegment::Num(0)) {
            part.pop();
        }
    }
    segments.append(&mut alpha);
    segments
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pins_use_each_tool_syntax() {
        assert_eq!(
            format_pin(Ecosystem::Npm, "@types/node", Some("24.1.0")),
            "@types/node@24.1.0"
        );
        assert_eq!(
            format_pin(Ecosystem::Python, "ruff", Some("0.6.0")),
            "ruff==0.6.0"
        );
        assert_eq!(
            format_pin(Ecosystem::Gem, "rails", Some("8.0.1")),
            "rails:8.0.1"
        );
        assert_eq!(format_pin(Ecosystem::Brew, "wget", Some("1.25")), "wget");
        assert_eq!(format_pin(Ecosystem::Npm, "typescript", None), "typescript");
    }

    #[test]
    fn parses_pinned_and_legacy_lines() {
        let pin = |e, l| parse_pin(e, l);
        assert_eq!(
            pin(Ecosystem::Npm, "@google/gemini-cli@0.18.4"),
            ("@google/gemini-cli".to_string(), Some("0.18.4".to_string()))
        );
        assert_eq!(
            pin(Ecosystem::Npm, "@types/node"),
            ("@types/node".to_string(), None)
        );
        assert_eq!(
            pin(Ecosystem::Npm, " typescript "),
            ("typescript".to_string(), None)
        );
        assert_eq!(
            pin(Ecosystem::Python, "ruff==0.6.0"),
            ("ruff".to_string(), Some("0.6.0".to_string()))
        );
        assert_eq!(
            pin(Ecosystem::Gem, "rails:8.0.1"),
            ("rails".to_string(), Some("8.0.1".to_string()))
        );
        assert_eq!(pin(Ecosystem::Gem, "rails"), ("rails".to_string(), None));
        assert_eq!(
            manifest_names(Ecosystem::Npm, "a@1\n\n@s/b@2\nc\n"),
            vec!["a", "@s/b", "c"]
        );
    }

    /// Each list is in ascending order, and its neighbors compare as `Less`.
    fn assert_ascending(ecosystem: Ecosystem, versions: &[&str]) {
        for pair in versions.windows(2) {
            assert_eq!(
                compare_versions(ecosystem, pair[0], pair[1]),
                Ordering::Less,
                "{} < {}",
                pair[0],
                pair[1]
            );
            assert_eq!(
                compare_versions(ecosystem, pair[1], pair[0]),
                Ordering::Greater
            );
        }
    }

    #[test]
    fn compares_semver() {
        assert_ascending(
            Ecosystem::Npm,
            &[
                "1.0.0-2",
                "1.0.0-10",
                "1.0.0-alpha",
                "1.0.0-alpha.1",
                "1.0.0-alpha.beta",
                "1.0.0-beta",
                "1.0.0-beta.2",
                "1.0.0-beta.11",
                "1.0.0-rc.1",
                "1.0.0",
                "1.9.0",
                "1.10.0",
                "2.0.0",
            ],
        );
        let eq = |a, b| compare_versions(Ecosystem::Npm, a, b);
        assert_eq!(eq("2.0.0+build.5", "2.0.0"), Ordering::Equal);
        assert_eq!(eq("1.0.0-x-y", "1.0.0-x-y"), Ordering::Equal);
    }

    #[test]
    fn compares_pep440() {
        assert_ascending(
            Ecosystem::Python,
            &[
                "1.0.dev1",
                "1.0a1.dev1",
                "1.0a1",
                "1.0a2",
                "1.0b1",
                "1.0rc1",
                "1.0",
                "1.0+local.1",
                "1.0.post1.dev1",
                "1.0.post9",
                "1.0.post10",
                "1.0.1",
                "1.10",
                "1!0.1",
            ],
        );
        let cmp = |a, b| compare_versions(Ecosystem::Python, a, b);
        assert_eq!(cmp("1.0", "1.0.0"), Ordering::Equal);
        assert_eq!(cmp("1.0+abc.2", "1.0+abc.10"), Ordering::Less);
        assert_eq!(cmp("1.0+abc", "1.0+5"), Ordering::Less);
    }

    #[test]
    fn compares_gem_versions() {
        assert_ascending(
            Ecosystem::Gem,
            &[
                "1.0.a", "1.0.b1", "1.0-rc1", "1.0.pre", "1.0.rc2", "1.0", "1.0.1", "1.9", "1.10",
            ],
        );
        let cmp = |a, b| compare_versions(Ecosystem::Gem, a, b);
        assert_eq!(cmp("1.0", "1.0.0"), Ordering::Equal);
        assert_eq!(cmp("1.0-rc1", "1.0.pre.rc1"), Ordering::Equal);
    }
}

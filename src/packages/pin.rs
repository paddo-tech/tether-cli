use super::Ecosystem;
use std::cmp::Ordering;

/// Manifest lines pin a version in each tool's own spec syntax: `name@1.2.3` for the npm
/// registry, `name==1.2.3` for uv, `name:1.2.3` for gem. Homebrew installs only the current
/// release, so the Brewfile stays unpinned.
pub fn format_pin(ecosystem: Ecosystem, name: &str, version: Option<&str>) -> String {
    match (version, separator(ecosystem)) {
        (Some(version), Some(sep)) => format!("{}{}{}", name, sep, version),
        _ => name.to_string(),
    }
}

/// Split a manifest line into name and pinned version. Lines from before pinning carry
/// only a name and install the newest release that passes the release-age limit.
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

/// Compare dotted versions part by part, numerically where both parts are numbers.
/// Build metadata is ignored, and a `-` prerelease sorts below its release (semver).
pub fn compare_versions(a: &str, b: &str) -> Ordering {
    let split = |v: &str| {
        let v = v.split_once('+').map_or(v, |(v, _)| v);
        match v.split_once('-') {
            Some((core, pre)) => (core.to_string(), Some(pre.to_string())),
            None => (v.to_string(), None),
        }
    };
    let ((a_core, a_pre), (b_core, b_pre)) = (split(a), split(b));
    compare_parts(&a_core, &b_core).then_with(|| match (a_pre, b_pre) {
        (None, None) => Ordering::Equal,
        (None, Some(_)) => Ordering::Greater,
        (Some(_), None) => Ordering::Less,
        (Some(x), Some(y)) => compare_parts(&x, &y),
    })
}

fn compare_parts(a: &str, b: &str) -> Ordering {
    let (a, b): (Vec<&str>, Vec<&str>) =
        (a.split(['.', '-']).collect(), b.split(['.', '-']).collect());
    for (x, y) in a.iter().zip(&b) {
        let order = match (x.parse::<u64>(), y.parse::<u64>()) {
            (Ok(x), Ok(y)) => x.cmp(&y),
            _ => x.cmp(y),
        };
        if order != Ordering::Equal {
            return order;
        }
    }
    a.len().cmp(&b.len())
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

    #[test]
    fn compares_versions_numerically() {
        assert_eq!(compare_versions("1.10.0", "1.9.0"), Ordering::Greater);
        assert_eq!(compare_versions("1.2", "1.2.0"), Ordering::Less);
        assert_eq!(compare_versions("2.0.0", "2.0.0"), Ordering::Equal);
        assert_eq!(
            compare_versions("1.0.0-beta", "1.0.0-alpha"),
            Ordering::Greater
        );
        assert_eq!(compare_versions("2.0.0-rc.1", "2.0.0"), Ordering::Less);
        assert_eq!(compare_versions("2.0.0+build.5", "2.0.0"), Ordering::Equal);
    }
}

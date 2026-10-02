use super::pin::compare_versions;
use super::{validate_version, Ecosystem};
use anyhow::{anyhow, bail, Result};
use chrono::{DateTime, Duration, Utc};
use serde_json::Value;

/// The release an unpinned install of `name` picks now: the newest stable release that is
/// older than `min_age_days`, from the public registry. Tether checks this version against
/// OSV and installs it pinned, so the version it checked is the version that installs.
pub async fn resolve_version(
    ecosystem: Ecosystem,
    name: &str,
    min_age_days: u32,
) -> Result<String> {
    let cutoff = (min_age_days > 0).then(|| Utc::now() - Duration::days(i64::from(min_age_days)));
    let picked = match ecosystem {
        Ecosystem::Npm => {
            let url = format!(
                "https://registry.npmjs.org/{}",
                name.replacen('/', "%2f", 1)
            );
            pick_npm(&get(&url).await?, cutoff)
        }
        Ecosystem::Python => pick_pypi(
            &get(&format!("https://pypi.org/pypi/{}/json", name)).await?,
            cutoff,
        ),
        // gem cannot hold to a release age, so the newest release is what it installs
        Ecosystem::Gem => get(&format!(
            "https://rubygems.org/api/v1/versions/{}/latest.json",
            name
        ))
        .await?
        .get("version")
        .and_then(Value::as_str)
        .map(str::to_string),
        Ecosystem::Brew | Ecosystem::BrewTap => None,
    };
    picked
        .filter(|v| validate_version(ecosystem, v).is_ok())
        .ok_or_else(|| anyhow!("no release of {} passes the release-age limit", name))
}

async fn get(url: &str) -> Result<Value> {
    let output = tokio::process::Command::new("curl")
        .args([
            "--silent",
            "--show-error",
            "--fail",
            "--location",
            "--connect-timeout",
            "3",
            "--max-time",
            "20",
            "--header",
            "Accept: application/json",
            url,
        ])
        .output()
        .await?;
    if !output.status.success() {
        bail!("{}", String::from_utf8_lossy(&output.stderr).trim());
    }
    Ok(serde_json::from_slice(&output.stdout)?)
}

fn released_before(time: Option<&str>, cutoff: Option<DateTime<Utc>>) -> bool {
    match cutoff {
        None => true,
        Some(cutoff) => time
            .and_then(|t| DateTime::parse_from_rfc3339(t).ok())
            .is_some_and(|t| t <= cutoff),
    }
}

/// From an npm packument: the `latest` tag when it is old enough, else the newest stable
/// release below it that is, like npm, pnpm and bun with a release-age limit.
fn pick_npm(packument: &Value, cutoff: Option<DateTime<Utc>>) -> Option<String> {
    let latest = packument.pointer("/dist-tags/latest")?.as_str()?;
    let times = packument.get("time");
    let time = |v: &str| times.and_then(|t| t.get(v)).and_then(Value::as_str);
    if released_before(time(latest), cutoff) {
        return Some(latest.to_string());
    }
    packument
        .get("versions")?
        .as_object()?
        .keys()
        .filter(|v| validate_version(Ecosystem::Npm, v).is_ok() && !v.contains('-'))
        .filter(|v| compare_versions(Ecosystem::Npm, v, latest).is_le())
        .filter(|v| released_before(time(v), cutoff))
        .max_by(|a, b| compare_versions(Ecosystem::Npm, a, b))
        .cloned()
}

/// From PyPI's JSON: the newest stable, unyanked release with a file uploaded before the
/// cutoff, as uv picks with `--exclude-newer`. Release keys are normalized first, as uv reads
/// them, so `1.0-post1` counts as `1.0.post1`.
fn pick_pypi(project: &Value, cutoff: Option<DateTime<Utc>>) -> Option<String> {
    project
        .get("releases")?
        .as_object()?
        .iter()
        .filter_map(|(v, files)| Some((normalize_pep440(v)?, files)))
        .filter(|(v, _)| validate_version(Ecosystem::Python, v).is_ok() && !is_pep440_prerelease(v))
        .filter(|(_, files)| {
            files.as_array().is_some_and(|files| {
                files.iter().any(|f| {
                    !f.get("yanked").and_then(Value::as_bool).unwrap_or(false)
                        && released_before(
                            f.get("upload_time_iso_8601").and_then(Value::as_str),
                            cutoff,
                        )
                })
            })
        })
        .map(|(v, _)| v)
        .max_by(|a, b| compare_versions(Ecosystem::Python, a, b))
}

/// A PEP 440 version in its normalized form: `1.0-post1`, `1.0-1` and `1.0.r1` become
/// `1.0.post1`, `1.0-RC1` becomes `1.0rc1`. `None` when it is not a PEP 440 version.
fn normalize_pep440(raw: &str) -> Option<String> {
    fn digits(s: &str) -> usize {
        s.bytes().take_while(u8::is_ascii_digit).count()
    }
    fn sep(s: &str) -> &str {
        s.strip_prefix(['-', '_', '.']).unwrap_or(s)
    }
    /// The number after a tag, with an optional separator; 0 when it has none.
    fn number(s: &str) -> Option<(u64, &str)> {
        let t = sep(s);
        match digits(t) {
            0 => Some((0, s)),
            n => Some((t[..n].parse().ok()?, &t[n..])),
        }
    }
    /// The number after one of `tags`, with optional separators before and after.
    fn tagged<'a>(s: &'a str, tags: &[&str]) -> Option<(u64, &'a str)> {
        let t = sep(s);
        let after = tags.iter().find_map(|tag| t.strip_prefix(tag))?;
        number(after)
    }

    let lower = raw.trim().to_ascii_lowercase();
    let v = lower.strip_prefix('v').unwrap_or(&lower);
    let (v, local) = v.split_once('+').map_or((v, None), |(v, l)| (v, Some(l)));
    let mut out = String::new();
    let mut rest = v;
    if let Some((epoch, after)) = v.split_once('!') {
        if digits(epoch) != epoch.len() || epoch.is_empty() {
            return None;
        }
        let epoch: u64 = epoch.parse().ok()?;
        if epoch != 0 {
            out.push_str(&format!("{}!", epoch));
        }
        rest = after;
    }
    let mut release = Vec::new();
    loop {
        let n = digits(rest);
        if n == 0 {
            return None;
        }
        release.push(rest[..n].parse::<u64>().ok()?.to_string());
        rest = &rest[n..];
        match rest.strip_prefix('.') {
            Some(after) if digits(after) > 0 => rest = after,
            _ => break,
        }
    }
    out.push_str(&release.join("."));
    let pre = [
        (&["alpha", "a"][..], "a"),
        (&["beta", "b"][..], "b"),
        (&["preview", "pre", "rc", "c"][..], "rc"),
    ];
    if let Some((tag, n, after)) = pre
        .iter()
        .find_map(|(tags, tag)| tagged(rest, tags).map(|(n, after)| (*tag, n, after)))
    {
        out.push_str(&format!("{}{}", tag, n));
        rest = after;
    }
    let implicit_post = rest
        .strip_prefix('-')
        .filter(|after| digits(after) > 0)
        .map(|after| {
            (
                after[..digits(after)].parse::<u64>(),
                &after[digits(after)..],
            )
        });
    if let Some((n, after)) = implicit_post {
        out.push_str(&format!(".post{}", n.ok()?));
        rest = after;
    } else if let Some((n, after)) = tagged(rest, &["post", "rev", "r"]) {
        out.push_str(&format!(".post{}", n));
        rest = after;
    }
    if let Some((n, after)) = tagged(rest, &["dev"]) {
        out.push_str(&format!(".dev{}", n));
        rest = after;
    }
    if !rest.is_empty() {
        return None;
    }
    if let Some(local) = local {
        let parts: Vec<&str> = local.split(['-', '_', '.']).collect();
        if parts
            .iter()
            .any(|p| p.is_empty() || !p.bytes().all(|b| b.is_ascii_alphanumeric()))
        {
            return None;
        }
        out.push('+');
        out.push_str(&parts.join("."));
    }
    Some(out)
}

/// Alpha, beta, release candidate and dev releases. A post release is stable.
fn is_pep440_prerelease(version: &str) -> bool {
    let public = version.split('+').next().unwrap_or(version).to_lowercase();
    public
        .replace("post", "")
        .chars()
        .any(|c| c.is_ascii_alphabetic())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(s: &str) -> Option<DateTime<Utc>> {
        Some(DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc))
    }

    #[test]
    fn npm_picks_latest_or_the_newest_old_enough_release_below_it() {
        let packument = serde_json::json!({
            "dist-tags": { "latest": "6.0.1", "next": "7.0.0-beta.1" },
            "versions": { "5.6.0": {}, "5.6.2": {}, "6.0.0": {}, "6.0.1": {},
                          "6.1.0-rc.1": {}, "7.0.0-beta.1": {} },
            "time": {
                "5.6.0": "2025-08-17T07:27:47Z",
                "5.6.2": "2025-09-08T14:47:54Z",
                "6.0.0": "2026-07-26T14:51:07Z",
                "6.0.1": "2026-09-27T03:40:44Z",
                "6.1.0-rc.1": "2026-01-01T00:00:00Z",
                "7.0.0-beta.1": "2026-01-01T00:00:00Z"
            }
        });
        assert_eq!(pick_npm(&packument, None).as_deref(), Some("6.0.1"));
        assert_eq!(
            pick_npm(&packument, at("2026-09-25T00:00:00Z")).as_deref(),
            Some("6.0.0")
        );
        assert_eq!(
            pick_npm(&packument, at("2025-09-01T00:00:00Z")).as_deref(),
            Some("5.6.0")
        );
        assert_eq!(pick_npm(&packument, at("2020-01-01T00:00:00Z")), None);
    }

    #[test]
    fn pypi_picks_the_newest_stable_unyanked_release_before_the_cutoff() {
        let file = |time: &str, yanked: bool| serde_json::json!({ "upload_time_iso_8601": time, "yanked": yanked });
        let project = serde_json::json!({
            "releases": {
                "0.9.9": [file("2025-02-28T10:15:44Z", false)],
                "0.10.0": [file("2025-06-01T00:00:00Z", true)],
                "0.10.1": [file("2025-06-02T00:00:00Z", false)],
                "0.10.1.post1": [file("2025-06-03T00:00:00Z", false)],
                "0.11.0rc1": [file("2025-07-01T00:00:00Z", false)],
                "0.11.0": [file("2026-09-30T00:00:00Z", false)],
                "0.12.0": []
            }
        });
        assert_eq!(pick_pypi(&project, None).as_deref(), Some("0.11.0"));
        assert_eq!(
            pick_pypi(&project, at("2026-09-25T00:00:00Z")).as_deref(),
            Some("0.10.1.post1")
        );
        assert_eq!(
            pick_pypi(&project, at("2025-06-01T12:00:00Z")).as_deref(),
            Some("0.9.9")
        );

        // uv reads a hyphenated post release as the normalized one, so Tether must too
        let project = serde_json::json!({
            "releases": {
                "1.0": [file("2025-01-01T00:00:00Z", false)],
                "1.0-post1": [file("2025-01-02T00:00:00Z", false)],
                "1.1-RC1": [file("2025-01-03T00:00:00Z", false)]
            }
        });
        assert_eq!(pick_pypi(&project, None).as_deref(), Some("1.0.post1"));
    }

    #[test]
    fn pep440_versions_normalize_as_uv_reads_them() {
        let cases = [
            ("1.0", "1.0"),
            ("v1.0", "1.0"),
            ("1.0-post1", "1.0.post1"),
            ("1.0_post_1", "1.0.post1"),
            ("1.0post", "1.0.post0"),
            ("1.0-1", "1.0.post1"),
            ("1.0.r2", "1.0.post2"),
            ("1.0rev3", "1.0.post3"),
            ("1.0-RC1", "1.0rc1"),
            ("1.0.alpha", "1.0a0"),
            ("1.0-preview.2", "1.0rc2"),
            ("1.0c1", "1.0rc1"),
            ("1.0.DEV", "1.0.dev0"),
            ("01.002", "1.2"),
            ("0!1.0", "1.0"),
            ("2!1.0b1.post2.dev3+Ubuntu-1", "2!1.0b1.post2.dev3+ubuntu.1"),
        ];
        for (raw, normal) in cases {
            assert_eq!(normalize_pep440(raw).as_deref(), Some(normal), "{raw}");
        }
        for bad in ["", "latest", "1.0.x", "1.0+", "1.0-", "x!1.0", ">=1.0"] {
            assert_eq!(normalize_pep440(bad), None, "{bad}");
        }
    }
}

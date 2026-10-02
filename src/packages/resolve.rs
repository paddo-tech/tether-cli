use super::pin::compare_versions;
use super::{validate_version, Ecosystem};
use anyhow::{anyhow, bail, Result};
use chrono::{DateTime, Duration, Utc};
use serde_json::Value;

/// The release an unpinned install of `name` with `manager_key` picks now: the newest
/// stable release that is older than `min_age_days`, from the registry that manager installs
/// from. Tether checks this version against OSV and installs it pinned, so the version it
/// checked is the version that installs. A registry that needs a login fails here, and the
/// caller takes the cautious path for an unknown release.
pub async fn resolve_version(
    manager_key: &str,
    ecosystem: Ecosystem,
    name: &str,
    min_age_days: u32,
) -> Result<String> {
    let cutoff = (min_age_days > 0).then(|| Utc::now() - Duration::days(i64::from(min_age_days)));
    let picked = match ecosystem {
        Ecosystem::Npm => {
            let registry = npm_registry(manager_key, name).await?;
            pick_npm(&get(&packument_url(&registry, name)).await?, cutoff)
        }
        Ecosystem::Python => pick_pypi(
            &get(&format!("https://pypi.org/pypi/{}/json", name)).await?,
            cutoff,
            &uv_python().await?,
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

const NPM_REGISTRY: &str = "https://registry.npmjs.org/";

/// The registry `manager_key` installs `name` from: its `@scope:registry`, else its
/// `registry`. npm and pnpm report their own config. bun has no config command, so a bun
/// registry setting anywhere Tether can see makes the release unknown.
async fn npm_registry(manager_key: &str, name: &str) -> Result<String> {
    if manager_key == "bun" {
        if bun_registry_configured() {
            bail!("bun uses a registry set in its config, so Tether cannot find the release");
        }
        return Ok(NPM_REGISTRY.to_string());
    }
    let mut keys = Vec::new();
    if let Some((scope, _)) = name.split_once('/').filter(|_| name.starts_with('@')) {
        keys.push(format!("{}:registry", scope));
    }
    keys.push("registry".to_string());
    for key in keys {
        let output = super::command(manager_key)?
            .args(["config", "get", &key])
            .output()
            .await?;
        if !output.status.success() {
            bail!("{} config get {} failed", manager_key, key);
        }
        if let Some(registry) = config_value(&String::from_utf8_lossy(&output.stdout)) {
            return Ok(registry);
        }
    }
    Ok(NPM_REGISTRY.to_string())
}

/// A value from `npm config get` or `pnpm config get`. Both print `undefined` for an unset key.
fn config_value(stdout: &str) -> Option<String> {
    let value = stdout.trim();
    (!value.is_empty() && value != "undefined" && value != "null").then(|| value.to_string())
}

fn packument_url(registry: &str, name: &str) -> String {
    format!(
        "{}/{}",
        registry.trim_end_matches('/'),
        name.replacen('/', "%2f", 1)
    )
}

/// Whether bun may install from a registry other than npm's: an environment variable, a
/// global `bunfig.toml` with `install.registry` or `install.scopes`, or a registry in
/// `~/.npmrc`, which bun reads too. Global installs run in an empty directory, so project
/// config does not apply.
fn bun_registry_configured() -> bool {
    if [
        "BUN_CONFIG_REGISTRY",
        "NPM_CONFIG_REGISTRY",
        "npm_config_registry",
    ]
    .iter()
    .any(|v| std::env::var_os(v).is_some())
    {
        return true;
    }
    let Ok(home) = crate::home_dir() else {
        return true;
    };
    let mut bunfigs = vec![home.join(".bunfig.toml")];
    if let Some(xdg) = std::env::var_os("XDG_CONFIG_HOME") {
        bunfigs.push(std::path::PathBuf::from(xdg).join(".bunfig.toml"));
    }
    bunfigs
        .iter()
        .filter_map(|p| std::fs::read_to_string(p).ok())
        .any(|text| bunfig_sets_registry(&text))
        || std::fs::read_to_string(home.join(".npmrc")).is_ok_and(|text| npmrc_sets_registry(&text))
}

fn bunfig_sets_registry(text: &str) -> bool {
    let Ok(config) = toml::from_str::<toml::Table>(text) else {
        return true;
    };
    config
        .get("install")
        .and_then(|i| i.as_table())
        .is_some_and(|i| i.contains_key("registry") || i.contains_key("scopes"))
}

/// A `registry` or `@scope:registry` key. Auth lines such as `//host/:_authToken` do not
/// change where packages come from.
fn npmrc_sets_registry(text: &str) -> bool {
    text.lines().any(|line| {
        let key = line.split_once('=').map_or("", |(k, _)| k).trim();
        key == "registry" || (key.starts_with('@') && key.ends_with(":registry"))
    })
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

/// The version of the Python that `uv tool install` would use, as `uv python find` reports it.
async fn uv_python() -> Result<String> {
    let output = super::command("uv")?
        .args(["python", "find", "--show-version"])
        .output()
        .await?;
    if !output.status.success() {
        bail!("uv python find failed");
    }
    let version = String::from_utf8_lossy(&output.stdout).trim().to_string();
    normalize_pep440(&version).ok_or_else(|| anyhow!("uv reported Python {:?}", version))
}

/// From PyPI's JSON: the newest stable, unyanked release with a file uploaded before the
/// cutoff whose `requires_python` admits `python`, as uv picks with `--exclude-newer`.
/// Release keys are normalized first, as uv reads them, so `1.0-post1` counts as
/// `1.0.post1`. Wheel platform tags are not checked: a release with only wheels for other
/// platforms is still picked, and its install fails rather than installing another release.
fn pick_pypi(project: &Value, cutoff: Option<DateTime<Utc>>, python: &str) -> Option<String> {
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
                        && f.get("requires_python")
                            .and_then(Value::as_str)
                            .is_none_or(|spec| python_admits(spec, python))
                })
            })
        })
        .map(|(v, _)| v)
        .max_by(|a, b| compare_versions(Ecosystem::Python, a, b))
}

/// Whether a `requires_python` specifier such as `>=3.8,!=3.9.*` admits `python`. An
/// unreadable specifier admits nothing, so its release is skipped.
fn python_admits(spec: &str, python: &str) -> bool {
    let cmp = |v: &str| compare_versions(Ecosystem::Python, python, v);
    // `3.9.*` matches 3.9 and every 3.9.x
    let prefix = |v: &str| {
        let release = |s: &str| -> Vec<u64> {
            s.split('.')
                .map_while(|p| p.parse().ok())
                .collect::<Vec<_>>()
        };
        let want = release(v);
        let have = release(python.split(['a', 'b', 'r', '+']).next().unwrap_or(python));
        (0..want.len()).all(|i| have.get(i).copied().unwrap_or(0) == want[i])
    };
    spec.split(',')
        .map(str::trim)
        .filter(|clause| !clause.is_empty())
        .all(|clause| {
            let ops = ["===", "~=", "==", "!=", ">=", "<=", ">", "<"];
            let Some(op) = ops.iter().find(|op| clause.starts_with(**op)) else {
                return false;
            };
            let raw = clause[op.len()..].trim();
            if let Some(base) = raw.strip_suffix(".*") {
                return match *op {
                    "==" => prefix(base),
                    "!=" => !prefix(base),
                    _ => false,
                };
            }
            let Some(v) = normalize_pep440(raw) else {
                return false;
            };
            match *op {
                "===" | "==" => cmp(&v).is_eq(),
                "!=" => cmp(&v).is_ne(),
                ">=" => cmp(&v).is_ge(),
                "<=" => cmp(&v).is_le(),
                ">" => cmp(&v).is_gt(),
                "<" => cmp(&v).is_lt(),
                // `~=3.8` is `>=3.8, ==3.*`
                _ => {
                    let parts: Vec<&str> = v.split('.').collect();
                    parts.len() >= 2
                        && cmp(&v).is_ge()
                        && prefix(&parts[..parts.len() - 1].join("."))
                }
            }
        })
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
    fn registry_config_decides_where_the_packument_comes_from() {
        assert_eq!(config_value("undefined\n"), None);
        assert_eq!(config_value("\n"), None);
        assert_eq!(
            config_value("https://npm.example.com/\n").as_deref(),
            Some("https://npm.example.com/")
        );
        assert_eq!(
            packument_url(NPM_REGISTRY, "@types/node"),
            "https://registry.npmjs.org/@types%2fnode"
        );
        assert_eq!(
            packument_url("https://npm.example.com/repo", "zx"),
            "https://npm.example.com/repo/zx"
        );

        assert!(!npmrc_sets_registry("//registry.npmjs.org/:_authToken=x\n"));
        assert!(npmrc_sets_registry("registry = https://npm.example.com/\n"));
        assert!(npmrc_sets_registry(
            "@acme:registry=https://npm.example.com/\n"
        ));
        assert!(!bunfig_sets_registry("[install]\nexact = true\n"));
        assert!(bunfig_sets_registry(
            "[install]\nregistry = \"https://npm.example.com/\"\n"
        ));
        assert!(bunfig_sets_registry(
            "[install.scopes]\nacme = \"https://npm.example.com/\"\n"
        ));
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
        assert_eq!(
            pick_pypi(&project, None, "3.12.1").as_deref(),
            Some("0.11.0")
        );
        assert_eq!(
            pick_pypi(&project, at("2026-09-25T00:00:00Z"), "3.12.1").as_deref(),
            Some("0.10.1.post1")
        );
        assert_eq!(
            pick_pypi(&project, at("2025-06-01T12:00:00Z"), "3.12.1").as_deref(),
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
        assert_eq!(
            pick_pypi(&project, None, "3.12.1").as_deref(),
            Some("1.0.post1")
        );
    }

    #[test]
    fn pypi_skips_releases_whose_requires_python_excludes_uv_python() {
        let file = |requires: &str| serde_json::json!({ "upload_time_iso_8601": "2025-01-01T00:00:00Z", "requires_python": requires });
        let project = serde_json::json!({
            "releases": {
                "1.0": [file(">=3.8")],
                "2.0": [file(">=3.13")],
                "3.0": [file(">=3.15,<4")]
            }
        });
        assert_eq!(pick_pypi(&project, None, "3.12.4").as_deref(), Some("1.0"));
        assert_eq!(pick_pypi(&project, None, "3.14.8").as_deref(), Some("2.0"));
        assert_eq!(pick_pypi(&project, None, "3.7.0"), None);

        assert!(python_admits(">=3.8, !=3.9.*, <4", "3.12.4"));
        assert!(!python_admits(">=3.8,!=3.9.*", "3.9.18"));
        assert!(python_admits("==3.12.*", "3.12.4"));
        assert!(python_admits("~=3.10", "3.14.8"));
        assert!(!python_admits("~=3.10.2", "3.11.0"));
        assert!(python_admits(">3.6", "3.12.4"));
        assert!(python_admits("", "3.12.4"));
        assert!(!python_admits("bogus", "3.12.4"));
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

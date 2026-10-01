use super::Ecosystem;
use anyhow::{bail, Result};
use serde::Deserialize;
use std::process::Stdio;
use tokio::io::AsyncWriteExt;

const QUERYBATCH_URL: &str = "https://api.osv.dev/v1/querybatch";

// OSV accepts at most 1000 queries per batch
const MAX_BATCH: usize = 1000;

/// OSV's name for each registry. Homebrew has no OSV ecosystem.
pub fn ecosystem_name(ecosystem: Ecosystem) -> Option<&'static str> {
    match ecosystem {
        Ecosystem::Npm => Some("npm"),
        Ecosystem::Python => Some("PyPI"),
        Ecosystem::Gem => Some("RubyGems"),
        Ecosystem::Brew | Ecosystem::BrewTap => None,
    }
}

/// OSV marks malicious-package reports with the `MAL-` prefix.
pub fn is_malicious(id: &str) -> bool {
    id.starts_with("MAL-")
}

#[derive(Deserialize)]
struct BatchResponse {
    results: Vec<BatchResult>,
}

#[derive(Deserialize)]
struct BatchResult {
    #[serde(default)]
    vulns: Vec<Vuln>,
}

#[derive(Deserialize)]
struct Vuln {
    id: String,
}

fn request_body(osv_ecosystem: &str, packages: &[(String, Option<String>)]) -> String {
    let queries: Vec<serde_json::Value> = packages
        .iter()
        .map(|(name, version)| {
            let mut query = serde_json::json!({
                "package": { "name": name, "ecosystem": osv_ecosystem }
            });
            if let Some(version) = version {
                query["version"] = serde_json::json!(version);
            }
            query
        })
        .collect();
    serde_json::json!({ "queries": queries }).to_string()
}

/// Advisory ids per package, in request order. Without a version OSV returns advisories
/// for every release, so only `MAL-` ids count for an unpinned package.
/// Only the first page is read; OSV pages only past 1000 advisories for one package.
fn parse_response(body: &str, packages: &[(String, Option<String>)]) -> Result<Vec<Vec<String>>> {
    let response: BatchResponse = serde_json::from_str(body)?;
    if response.results.len() != packages.len() {
        bail!(
            "OSV returned {} results for {} queries",
            response.results.len(),
            packages.len()
        );
    }
    Ok(response
        .results
        .into_iter()
        .zip(packages)
        .map(|(result, (_, version))| {
            result
                .vulns
                .into_iter()
                .map(|v| v.id)
                .filter(|id| version.is_some() || is_malicious(id))
                .collect()
        })
        .collect())
}

/// curl ships with macOS and common Linux systems, so Tether needs no HTTP client crate.
async fn post(body: String) -> Result<String> {
    let mut child = tokio::process::Command::new("curl")
        .args([
            "--silent",
            "--show-error",
            "--fail",
            "--connect-timeout",
            "3",
            "--max-time",
            "10",
            "--header",
            "Content-Type: application/json",
            "--data-binary",
            "@-",
            QUERYBATCH_URL,
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    let mut stdin = child.stdin.take().expect("stdin is piped");
    stdin.write_all(body.as_bytes()).await?;
    drop(stdin);
    let output = child.wait_with_output().await?;
    if !output.status.success() {
        bail!("{}", String::from_utf8_lossy(&output.stderr).trim());
    }
    Ok(String::from_utf8(output.stdout)?)
}

/// Advisory ids for each package, in input order. A network or API failure must not
/// stop installs, so it is logged and only the packages in the failed batch get no
/// advisories. Results from other batches are kept.
pub async fn advisories(
    ecosystem: Ecosystem,
    packages: &[(String, Option<String>)],
) -> Vec<Vec<String>> {
    let Some(osv_ecosystem) = ecosystem_name(ecosystem) else {
        return vec![Vec::new(); packages.len()];
    };
    let mut found = Vec::with_capacity(packages.len());
    for chunk in packages.chunks(MAX_BATCH) {
        let result = match post(request_body(osv_ecosystem, chunk)).await {
            Ok(body) => parse_response(&body, chunk),
            Err(e) => Err(e),
        };
        match result {
            Ok(ids) => found.extend(ids),
            Err(e) => {
                eprintln!(
                    "Warning: OSV check skipped for {} packages: {}",
                    chunk.len(),
                    e
                );
                found.extend(vec![Vec::new(); chunk.len()]);
            }
        }
    }
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pkgs(list: &[(&str, Option<&str>)]) -> Vec<(String, Option<String>)> {
        list.iter()
            .map(|(n, v)| (n.to_string(), v.map(str::to_string)))
            .collect()
    }

    #[test]
    fn request_matches_querybatch_format() {
        let body = request_body("npm", &pkgs(&[("nx", Some("21.5.0")), ("left-pad", None)]));
        let json: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(
            json,
            serde_json::json!({"queries": [
                {"package": {"name": "nx", "ecosystem": "npm"}, "version": "21.5.0"},
                {"package": {"name": "left-pad", "ecosystem": "npm"}}
            ]})
        );
    }

    #[test]
    fn parses_ids_and_keeps_only_mal_for_unpinned() {
        let body = r#"{"results":[{},
            {"vulns":[{"id":"GHSA-g2r8-wvmj-jf5w","modified":"2026-07-31T17:00:20Z"},
                      {"id":"MAL-2025-41443","modified":"2026-07-28T05:21:53Z"}]},
            {"vulns":[{"id":"GHSA-x","modified":"2026-07-31T17:00:20Z"},
                      {"id":"MAL-2026-1","modified":"2026-07-28T05:21:53Z"}]}]}"#;
        let packages = pkgs(&[
            ("left-pad", Some("1.3.0")),
            ("nx", Some("21.5.0")),
            ("x", None),
        ]);
        let ids = parse_response(body, &packages).unwrap();
        assert!(ids[0].is_empty());
        assert_eq!(ids[1], vec!["GHSA-g2r8-wvmj-jf5w", "MAL-2025-41443"]);
        assert_eq!(ids[2], vec!["MAL-2026-1"]);
        assert!(ids[1].iter().any(|id| is_malicious(id)));
    }

    #[test]
    fn rejects_mismatched_result_count() {
        assert!(parse_response(r#"{"results":[]}"#, &pkgs(&[("a", None)])).is_err());
    }

    #[test]
    fn homebrew_has_no_osv_ecosystem() {
        assert_eq!(ecosystem_name(Ecosystem::Brew), None);
        assert_eq!(ecosystem_name(Ecosystem::Python), Some("PyPI"));
        assert_eq!(ecosystem_name(Ecosystem::Gem), Some("RubyGems"));
    }
}

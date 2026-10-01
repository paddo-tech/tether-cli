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
    next_page_token: Option<String>,
}

#[derive(Deserialize)]
struct Vuln {
    id: String,
}

/// One querybatch entry. `page_token` asks for a later page of an earlier result.
struct Query<'a> {
    name: &'a str,
    version: Option<&'a str>,
    page_token: Option<String>,
}

fn request_body(osv_ecosystem: &str, queries: &[Query]) -> String {
    let queries: Vec<serde_json::Value> = queries
        .iter()
        .map(|q| {
            let mut query = serde_json::json!({
                "package": { "name": q.name, "ecosystem": osv_ecosystem }
            });
            if let Some(version) = q.version {
                query["version"] = serde_json::json!(version);
            }
            if let Some(token) = &q.page_token {
                query["page_token"] = serde_json::json!(token);
            }
            query
        })
        .collect();
    serde_json::json!({ "queries": queries }).to_string()
}

/// Advisory ids and the next page token per query, in request order. Without a version
/// OSV returns advisories for every release, so only `MAL-` ids count for an unpinned package.
fn parse_response(body: &str, queries: &[Query]) -> Result<Vec<(Vec<String>, Option<String>)>> {
    let response: BatchResponse = serde_json::from_str(body)?;
    if response.results.len() != queries.len() {
        bail!(
            "OSV returned {} results for {} queries",
            response.results.len(),
            queries.len()
        );
    }
    Ok(response
        .results
        .into_iter()
        .zip(queries)
        .map(|(result, query)| {
            let ids = result
                .vulns
                .into_iter()
                .map(|v| v.id)
                .filter(|id| query.version.is_some() || is_malicious(id))
                .collect();
            (ids, result.next_page_token)
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

/// Advisory ids for each package, in input order. A result with a page token is asked
/// again with that token until OSV has no more pages. A network or API failure must not
/// stop installs, so it is logged, and the packages in the failed request keep only the
/// ids already received.
pub async fn advisories(
    ecosystem: Ecosystem,
    packages: &[(String, Option<String>)],
) -> Vec<Vec<String>> {
    let mut found = vec![Vec::new(); packages.len()];
    let Some(osv_ecosystem) = ecosystem_name(ecosystem) else {
        return found;
    };
    let mut pending: Vec<(usize, Option<String>)> =
        (0..packages.len()).map(|i| (i, None)).collect();
    while !pending.is_empty() {
        let size = pending.len().min(MAX_BATCH);
        let batch: Vec<(usize, Option<String>)> = pending.drain(..size).collect();
        let queries: Vec<Query> = batch
            .iter()
            .map(|(i, token)| Query {
                name: &packages[*i].0,
                version: packages[*i].1.as_deref(),
                page_token: token.clone(),
            })
            .collect();
        let result = match post(request_body(osv_ecosystem, &queries)).await {
            Ok(body) => parse_response(&body, &queries),
            Err(e) => Err(e),
        };
        match result {
            Ok(pages) => {
                for ((i, _), (ids, next)) in batch.into_iter().zip(pages) {
                    found[i].extend(ids);
                    if next.is_some() {
                        pending.push((i, next));
                    }
                }
            }
            Err(e) => eprintln!(
                "Warning: OSV check incomplete for {} packages: {}",
                queries.len(),
                e
            ),
        }
    }
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    fn queries<'a>(list: &[(&'a str, Option<&'a str>, Option<&str>)]) -> Vec<Query<'a>> {
        list.iter()
            .map(|(name, version, token)| Query {
                name,
                version: *version,
                page_token: token.map(str::to_string),
            })
            .collect()
    }

    #[test]
    fn request_matches_querybatch_format() {
        let body = request_body(
            "npm",
            &queries(&[
                ("nx", Some("21.5.0"), None),
                ("left-pad", None, None),
                ("big", Some("1.0.0"), Some("tok")),
            ]),
        );
        let json: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(
            json,
            serde_json::json!({"queries": [
                {"package": {"name": "nx", "ecosystem": "npm"}, "version": "21.5.0"},
                {"package": {"name": "left-pad", "ecosystem": "npm"}},
                {"package": {"name": "big", "ecosystem": "npm"}, "version": "1.0.0", "page_token": "tok"}
            ]})
        );
    }

    #[test]
    fn parses_ids_page_tokens_and_keeps_only_mal_for_unpinned() {
        let body = r#"{"results":[{},
            {"vulns":[{"id":"GHSA-g2r8-wvmj-jf5w","modified":"2026-07-31T17:00:20Z"},
                      {"id":"MAL-2025-41443","modified":"2026-07-28T05:21:53Z"}],
             "next_page_token":"page2"},
            {"vulns":[{"id":"GHSA-x","modified":"2026-07-31T17:00:20Z"},
                      {"id":"MAL-2026-1","modified":"2026-07-28T05:21:53Z"}]}]}"#;
        let packages = queries(&[
            ("left-pad", Some("1.3.0"), None),
            ("nx", Some("21.5.0"), None),
            ("x", None, None),
        ]);
        let results = parse_response(body, &packages).unwrap();
        assert_eq!(results[0], (Vec::new(), None));
        assert_eq!(
            results[1],
            (
                vec![
                    "GHSA-g2r8-wvmj-jf5w".to_string(),
                    "MAL-2025-41443".to_string()
                ],
                Some("page2".to_string())
            )
        );
        assert_eq!(results[2], (vec!["MAL-2026-1".to_string()], None));
    }

    #[test]
    fn rejects_mismatched_result_count() {
        assert!(parse_response(r#"{"results":[]}"#, &queries(&[("a", None, None)])).is_err());
    }

    #[test]
    fn homebrew_has_no_osv_ecosystem() {
        assert_eq!(ecosystem_name(Ecosystem::Brew), None);
        assert_eq!(ecosystem_name(Ecosystem::Python), Some("PyPI"));
        assert_eq!(ecosystem_name(Ecosystem::Gem), Some("RubyGems"));
    }
}

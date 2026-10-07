//! `tether upgrade` with real npm and uv from the public registries. The release-age limit is
//! set so that the newest allowed release is older than the installed one: upgrade must keep
//! the installed version. A package with no release old enough is skipped, and the rest of
//! its manager upgrades. A uv tool the user pinned stays and is reported as pinned. With the
//! default limit, an old version upgrades.

use crate::harness::{enabled, Lab, Machine};
use chrono::{DateTime, Utc};

const NPM: (&str, &str, &str) = ("is-number", "6.0.0", "7.0.0");
const UV: (&str, &str, &str) = ("pycowsay", "0.0.0.1", "0.0.0.2");
/// Preinstalled in the node image. Its first release is from 2020, years after NPM's.
const YOUNG: &str = "corepack";

/// A limit in days whose cutoff falls halfway between two release dates.
fn days_between(older: DateTime<Utc>, newer: DateTime<Utc>) -> i64 {
    let mid = older + (newer - older) / 2;
    (Utc::now() - mid).num_days()
}

fn date(s: &str) -> DateTime<Utc> {
    s.parse().unwrap_or_else(|e| panic!("bad date {s}: {e}"))
}

async fn npm_times(m: &Machine, name: &str) -> serde_json::Value {
    let out = m.ok(&format!("npm view {name} time --json")).await;
    serde_json::from_str(&out.stdout).unwrap()
}

async fn npm_dates(m: &Machine) -> (DateTime<Utc>, DateTime<Utc>) {
    let (name, older, newer) = NPM;
    let t = npm_times(m, name).await;
    (
        date(t[older].as_str().unwrap()),
        date(t[newer].as_str().unwrap()),
    )
}

async fn first_release(m: &Machine, name: &str) -> DateTime<Utc> {
    let t = npm_times(m, name).await;
    t.as_object()
        .unwrap()
        .iter()
        .filter(|(k, _)| *k != "created" && *k != "modified")
        .map(|(_, v)| date(v.as_str().unwrap()))
        .min()
        .unwrap()
}

async fn pypi_dates(m: &Machine) -> (DateTime<Utc>, DateTime<Utc>) {
    let (name, older, newer) = UV;
    let out = m
        .ok(&format!("curl -sf https://pypi.org/pypi/{name}/json"))
        .await;
    let d: serde_json::Value = serde_json::from_str(&out.stdout).unwrap();
    let at = |v: &str| {
        date(
            d["releases"][v][0]["upload_time_iso_8601"]
                .as_str()
                .unwrap(),
        )
    };
    (at(older), at(newer))
}

async fn npm_version_of(m: &Machine, name: &str) -> String {
    let out = m.ok(&format!("npm ls -g {name} --json")).await;
    let v: serde_json::Value = serde_json::from_str(&out.stdout).unwrap();
    v["dependencies"][name]["version"]
        .as_str()
        .unwrap_or("")
        .to_string()
}

async fn npm_version(m: &Machine) -> String {
    npm_version_of(m, NPM.0).await
}

async fn uv_version(m: &Machine) -> String {
    let out = m.ok("uv tool list").await;
    out.stdout
        .lines()
        .find_map(|l| l.strip_prefix(&format!("{} v", UV.0)))
        .unwrap_or("")
        .to_string()
}

async fn set_limit(m: &Machine, days: i64) {
    m.tether_ok(&format!("config set packages.min_release_age_days {days}"))
        .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn upgrade_never_downgrades() {
    if !enabled("upgrade_never_downgrades") {
        return;
    }
    let lab = Lab::new("upgrade_never_downgrades").await;
    let m = lab.real_machine("a").await;
    assert_eq!(m.init(&lab).await.code, 0, "init");
    let (npm_old, npm_new) = npm_dates(&m).await;
    let (uv_old, uv_new) = pypi_dates(&m).await;
    let young_first = first_release(&m, YOUNG).await;
    let young_version = npm_version_of(&m, YOUNG).await;
    assert!(!young_version.is_empty(), "{YOUNG} is preinstalled");

    // npm fails its whole outdated check when one package has no release older than the
    // limit. That package is skipped with a note, and the rest of npm upgrades
    m.ok(&format!("npm install -g {}@{}", NPM.0, NPM.1)).await;
    set_limit(&m, days_between(npm_new, young_first)).await;
    let out = m.tether_ok("upgrade -y").await.text();
    assert_eq!(npm_version(&m).await, NPM.2, "npm did not upgrade:\n{out}");
    assert!(
        out.contains(&format!(
            "{YOUNG} skipped: no release is older than the release-age limit"
        )),
        "{out}"
    );
    assert_eq!(npm_version_of(&m, YOUNG).await, young_version);

    // Installed releases that are newer than the limit allows stay
    set_limit(&m, days_between(npm_old, npm_new)).await;
    let out = m.tether_ok("upgrade -y").await;
    assert_eq!(
        npm_version(&m).await,
        NPM.2,
        "npm downgraded:\n{}",
        out.text()
    );

    // uv keeps a `==` pin in the tool receipt, and `uv tool upgrade` never moves past it.
    // Installing the bare name offline drops the pin and keeps the version, as Tether does
    m.ok(&format!(
        "uv tool install {0}=={1} && uv tool install --offline {0}",
        UV.0, UV.2
    ))
    .await;
    set_limit(&m, days_between(uv_old, uv_new)).await;
    let out = m.tether_ok("upgrade -y").await;
    assert_eq!(uv_version(&m).await, UV.2, "uv downgraded:\n{}", out.text());
    assert_eq!(npm_version(&m).await, NPM.2);

    // A pin the user set stays, and upgrade reports it as pinned, never as upgraded. The
    // limit lets the releases of both packages through
    m.ok(&format!("npm install -g {}@{}", NPM.0, NPM.1)).await;
    m.ok(&format!("uv tool install --force {}=={}", UV.0, UV.1))
        .await;
    set_limit(&m, 7).await;
    let out = m.tether_ok("upgrade -y").await.text();
    assert_eq!(uv_version(&m).await, UV.1, "the user's pin moved:\n{out}");
    assert!(
        out.contains(&format!("{} pinned at {}, not upgraded", UV.0, UV.1)),
        "{out}"
    );
    assert!(!out.contains(&format!("{} {} →", UV.0, UV.1)), "{out}");
    let receipt = m
        .ok(&format!("cat \"$(uv tool dir)/{}/uv-receipt.toml\"", UV.0))
        .await
        .stdout;
    assert!(receipt.contains(&format!("=={}", UV.1)), "{receipt}");
    let npm = npm_version(&m).await;
    assert!(newer_than(&npm, NPM.1), "npm stayed at {npm}:\n{out}");

    // With the default limit, which is not in config.toml, old releases upgrade. Upstream
    // can publish new releases, so the checks ask only for a version above the old one
    m.ok("sed -i '/^min_release_age_days/d' /root/.tether/config.toml")
        .await;
    let limit = m
        .tether_ok("config get packages.min_release_age_days")
        .await
        .stdout;
    assert_eq!(limit.trim(), "7", "the limit is not the default");
    m.ok(&format!("npm install -g {}@{}", NPM.0, NPM.1)).await;
    m.ok(&format!("uv tool install --offline {}", UV.0)).await;
    let out = m.tether_ok("upgrade -y").await.text();
    let npm = npm_version(&m).await;
    assert!(newer_than(&npm, NPM.1), "npm stayed at {npm}:\n{out}");
    let uv = uv_version(&m).await;
    assert!(newer_than(&uv, UV.1), "uv stayed at {uv}:\n{out}");
}

/// Whether dotted version `a` is above `b`, part by part. An empty version is not.
fn newer_than(a: &str, b: &str) -> bool {
    let parts = |v: &str| -> Vec<u64> { v.split('.').map(|p| p.parse().unwrap_or(0)).collect() };
    !a.is_empty() && parts(a) > parts(b)
}

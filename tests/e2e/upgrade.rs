//! `tether upgrade` with real npm and uv from the public registries. The release-age limit is
//! set so that the newest allowed release is older than the installed one: upgrade must keep
//! the installed version. With the default limit, an old version upgrades.

use crate::harness::{enabled, Lab, Machine};
use chrono::{DateTime, Utc};

const NPM: (&str, &str, &str) = ("is-number", "6.0.0", "7.0.0");
const UV: (&str, &str, &str) = ("pycowsay", "0.0.0.1", "0.0.0.2");

/// A limit in days whose cutoff falls halfway between two release dates.
fn days_between(older: DateTime<Utc>, newer: DateTime<Utc>) -> i64 {
    let mid = older + (newer - older) / 2;
    (Utc::now() - mid).num_days()
}

fn date(s: &str) -> DateTime<Utc> {
    s.parse().unwrap_or_else(|e| panic!("bad date {s}: {e}"))
}

async fn npm_dates(m: &Machine) -> (DateTime<Utc>, DateTime<Utc>) {
    let (name, older, newer) = NPM;
    let out = m.ok(&format!("npm view {name} time --json")).await;
    let t: serde_json::Value = serde_json::from_str(&out.stdout).unwrap();
    (
        date(t[older].as_str().unwrap()),
        date(t[newer].as_str().unwrap()),
    )
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

async fn npm_version(m: &Machine) -> String {
    let out = m.ok(&format!("npm ls -g {} --json", NPM.0)).await;
    let v: serde_json::Value = serde_json::from_str(&out.stdout).unwrap();
    v["dependencies"][NPM.0]["version"]
        .as_str()
        .unwrap_or("")
        .to_string()
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

    // npm outdated fails for a package with no release older than the limit, and corepack
    // has none before 2019, so it would fail the whole npm upgrade
    m.ok("npm uninstall -g corepack").await;

    // Installed releases that are newer than the limit allows stay
    m.ok(&format!("npm install -g {}@{}", NPM.0, NPM.2)).await;
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

    // With the default limit, old releases upgrade
    m.ok(&format!("npm install -g {}@{}", NPM.0, NPM.1)).await;
    m.ok(&format!(
        "uv tool install --force {0}=={1} && uv tool install --offline {0}",
        UV.0, UV.1
    ))
    .await;
    set_limit(&m, 7).await;
    let out = m.tether_ok("upgrade -y").await;
    let npm = npm_version(&m).await;
    let uv = uv_version(&m).await;
    assert!(
        npm.split('.').next().unwrap().parse::<u32>().unwrap() >= 7,
        "npm stayed at {npm}:\n{}",
        out.text()
    );
    assert!(
        uv != UV.1 && !uv.is_empty(),
        "uv stayed at {uv}:\n{}",
        out.text()
    );
}

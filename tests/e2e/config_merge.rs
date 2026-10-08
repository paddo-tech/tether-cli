//! config.toml merges between HEAD machines: a machine that only rewrote the format of its
//! config, or changed another setting, never exports over another machine's profile change.
//! A 1.x machine's export does not strip 2.0 settings, and its stale copy of an earlier
//! export changes nothing. 1.x loads every exported copy, also when the local file leaves
//! out fields 1.x requires. A setting both machines changed settles, set lists keep one
//! order, a cleared list stays cleared, a dry run writes no config, and a write makes
//! config.toml 0600.

use crate::harness::{enabled, Lab, Machine, HEAD, OLD};

const CONFIG: &str = "/root/.tether/config.toml";

async fn config(m: &Machine) -> toml::Table {
    toml::from_str(&m.read(CONFIG).await).expect("config.toml")
}

async fn assigned(m: &Machine, id: &str) -> Option<String> {
    config(m).await["machine_profiles"]
        .get(id)
        .and_then(|v| v.as_str())
        .map(str::to_string)
}

async fn has_profile(m: &Machine, name: &str) -> bool {
    config(m).await["profiles"].get(name).is_some()
}

/// packages.allow_scripts as written, in file order; missing is empty.
async fn allow_scripts(m: &Machine) -> Vec<String> {
    config(m).await["packages"]
        .get("allow_scripts")
        .and_then(|v| v.as_array())
        .map(|a| a.iter().map(|v| v.as_str().unwrap().to_string()).collect())
        .unwrap_or_default()
}

async fn sha(m: &Machine, path: &str) -> String {
    m.ok(&format!("sha256sum {path}")).await.stdout
}

/// Syncs a then b until a round commits nothing; panics after `rounds`.
async fn settle(lab: &Lab, a: &Machine, b: &Machine, rounds: usize, what: &str) {
    for _ in 0..rounds {
        let head = lab.head().await;
        a.tether_ok("sync").await;
        b.tether_ok("sync").await;
        if lab.changed(&head, &lab.head().await).await.is_empty() {
            return;
        }
    }
    panic!("{what}: the pair still commits after {rounds} rounds");
}

/// Syncs the machines in turn until a round commits no config.toml; panics after 4 rounds.
/// 1.x rewrites its own record on every sync, so only config.toml counts.
async fn settle3(lab: &Lab, machines: [&Machine; 3], what: &str) {
    for _ in 0..4 {
        let head = lab.head().await;
        for m in machines {
            m.tether_ok("sync").await;
        }
        let changed = lab.changed(&head, &lab.head().await).await;
        if !changed.iter().any(|p| p.contains("config.toml")) {
            return;
        }
    }
    panic!("{what}: config.toml still changes after 4 rounds");
}

#[tokio::test(flavor = "multi_thread")]
async fn config_changes_merge() {
    if !enabled("config_changes_merge") {
        return;
    }
    let lab = Lab::new("config_changes_merge").await;
    let a = lab.machine("a", &[HEAD]).await;
    let b = lab.machine("b", &[HEAD]).await;
    a.seed("npm", "dev-only", "1.0.0").await;
    assert_eq!(a.init(&lab).await.code, 0, "init a");
    assert_eq!(b.init(&lab).await.code, 0, "init b");
    let b_id = b.machine_id().await;

    b.tether_ok("machines profile create linux-server --from dev --managers npm -y")
        .await;
    b.tether_ok("machines profile set linux-server").await;
    b.tether_ok("sync").await;
    let fp = a.fingerprint().await;
    b.tether_ok(&format!("machines trust a --fingerprint {fp}"))
        .await;

    // a rewrites its config in another format, as an upgrade does, and has no merge base yet
    a.ok(&format!(
        "sed -i '1i # rewritten by an upgrade' {CONFIG} && sed -i 's/ = /=/' {CONFIG} && \
         rm -f /root/.tether/config.base.toml"
    ))
    .await;
    a.tether_ok("sync").await;
    assert_eq!(assigned(&a, &b_id).await.as_deref(), Some("linux-server"));
    assert!(has_profile(&a, "linux-server").await);

    for _ in 0..2 {
        let out = b.tether_ok("sync").await.text();
        assert!(!out.contains("changed this machine's profile"), "{out}");
    }
    assert_eq!(assigned(&b, &b_id).await.as_deref(), Some("linux-server"));
    assert!(has_profile(&b, "linux-server").await);
    let installs = b.installs().await;
    assert!(
        !installs.iter().any(|i| i.name == "dev-only"),
        "b installed a package of profile dev: {installs:?}"
    );

    // Two machines change different settings before either syncs: both changes stay
    a.tether_ok("config set dashboard.theme mocha").await;
    b.tether_ok("config set packages.brew.sync_casks false")
        .await;
    a.tether_ok("sync").await;
    b.tether_ok("sync").await;
    a.tether_ok("sync").await;
    for m in [&a, &b] {
        let cfg = config(m).await;
        assert_eq!(
            cfg["dashboard"]["theme"].as_str(),
            Some("mocha"),
            "{}",
            m.name
        );
        assert_eq!(
            cfg["packages"]["brew"]["sync_casks"].as_bool(),
            Some(false),
            "{}",
            m.name
        );
        assert_eq!(
            assigned(m, &b_id).await.as_deref(),
            Some("linux-server"),
            "{}",
            m.name
        );
    }
    // A record lists the config hash of the sync before, so it settles one round later
    a.tether_ok("sync").await;
    b.tether_ok("sync").await;
    let head = lab.head().await;
    a.tether_ok("sync").await;
    b.tether_ok("sync").await;
    let changed = lab.changed(&head, &lab.head().await).await;
    let diff = lab
        .remote(&format!("git diff {head} main -- machines/*.json"))
        .await;
    assert!(
        changed.is_empty(),
        "a settled pair pushed {changed:?}:\n{diff}"
    );

    // A new machine takes the remote config, and its defaults do not replace it
    let c = lab.machine("c", &[HEAD]).await;
    assert_eq!(c.init(&lab).await.code, 0, "init c");
    let c_id = c.machine_id().await;
    a.tether_ok("sync").await;
    for m in [&a, &c] {
        let cfg = config(m).await;
        assert_eq!(
            cfg["dashboard"]["theme"].as_str(),
            Some("mocha"),
            "{}",
            m.name
        );
        assert_eq!(
            cfg["packages"]["brew"]["sync_casks"].as_bool(),
            Some(false),
            "{}",
            m.name
        );
        assert_eq!(assigned(m, &b_id).await.as_deref(), Some("linux-server"));
        assert!(has_profile(m, "linux-server").await);
    }
    assert_eq!(assigned(&c, &c_id).await.as_deref(), Some("dev"));

    // A 1.x machine saves a config without the keys it does not know and exports it. HEAD
    // machines keep those settings and take the change 1.x made
    a.tether_ok(r#"config set packages.allow_scripts '["esbuild"]'"#)
        .await;
    a.tether_ok("sync").await;
    let old = lab.machine("old", &[OLD[2]]).await;
    assert_eq!(old.init(&lab).await.code, 0, "init old");
    old.tether_ok("config set packages.brew.sync_casks true")
        .await;
    let stripped = old.read(CONFIG).await;
    assert!(!stripped.contains("dashboard"), "{stripped}");
    assert!(!stripped.contains("allow_scripts"), "{stripped}");
    old.tether_ok("sync").await;
    let head = lab.head().await;
    a.tether_ok("sync").await;
    // a exports the merged config, so the repo holds the 2.0 settings again
    assert!(
        lab.changed(&head, &lab.head().await)
            .await
            .iter()
            .any(|p| p.contains("config.toml")),
        "a did not restore the stripped config"
    );
    b.tether_ok("sync").await;
    for m in [&a, &b] {
        let cfg = config(m).await;
        assert_eq!(
            cfg["dashboard"]["theme"].as_str(),
            Some("mocha"),
            "{}",
            m.name
        );
        assert_eq!(
            cfg["packages"]["brew"]["sync_casks"].as_bool(),
            Some(true),
            "{}",
            m.name
        );
        assert_eq!(allow_scripts(m).await, vec!["esbuild"], "{}", m.name);
        assert_eq!(assigned(m, &b_id).await.as_deref(), Some("linux-server"));
        assert!(has_profile(m, "linux-server").await, "{}", m.name);
    }

    // 1.13.1 applies the restored copy, and later pushes it again verbatim: a stale copy
    // with an older config_generation. The config.toml of a and b leaves out fields 1.x
    // requires
    old.tether_ok("sync").await;
    assert!(old.read(CONFIG).await.contains("config_generation"));
    for m in [&a, &b] {
        m.ok(&format!("sed -i '/sync_versions/d' {CONFIG}")).await;
    }
    a.tether_ok("config set packages.min_release_age_days 14")
        .await;
    a.tether_ok("sync").await;
    b.tether_ok("sync").await;
    let head = lab.head().await;
    old.tether_ok("sync").await;
    assert!(
        lab.changed(&head, &lab.head().await)
            .await
            .iter()
            .any(|p| p.contains("config.toml")),
        "1.13.1 did not export its stale copy"
    );
    let head = lab.head().await;
    b.tether_ok("sync").await;
    assert!(
        lab.changed(&head, &lab.head().await)
            .await
            .iter()
            .any(|p| p.contains("config.toml")),
        "b did not restore the config over the stale copy"
    );
    a.tether_ok("sync").await;
    let days = |cfg: toml::Table| cfg["packages"]["min_release_age_days"].as_integer();
    for m in [&a, &b] {
        assert_eq!(days(config(m).await), Some(14), "{}", m.name);
    }
    // 1.13.1 takes the restored copy, which has every field it requires, and loads it
    old.tether_ok("sync").await;
    let text = old.read(CONFIG).await;
    assert!(text.contains("min_release_age_days = 14"), "{text}");
    assert_eq!(text.matches("sync_versions = false").count(), 5, "{text}");
    old.tether_ok("config get packages.npm.sync_versions").await;
    settle3(&lab, [&a, &b, &old], "stale 1.x copy").await;

    // 1.13.1 saves its config again: no marker, no generation, no 2.0 keys. HEAD merges its
    // change, keeps the 2.0 settings, and marks the copy again
    old.tether_ok("config set packages.brew.sync_taps false")
        .await;
    assert!(!old.read(CONFIG).await.contains("config_writer"));
    old.tether_ok("sync").await;
    a.tether_ok("sync").await;
    b.tether_ok("sync").await;
    for m in [&a, &b] {
        let cfg = config(m).await;
        assert_eq!(
            cfg["packages"]["brew"]["sync_taps"].as_bool(),
            Some(false),
            "{}",
            m.name
        );
        assert_eq!(days(cfg.clone()), Some(14), "{}", m.name);
        assert_eq!(
            cfg["dashboard"]["theme"].as_str(),
            Some("mocha"),
            "{}",
            m.name
        );
        assert_eq!(allow_scripts(m).await, vec!["esbuild"], "{}", m.name);
    }
    old.tether_ok("sync").await;
    assert!(old.read(CONFIG).await.contains("config_writer"));
    old.tether_ok("config get packages.brew.sync_taps").await;
    settle3(&lab, [&a, &b, &old], "1.x save").await;
    drop(old);
    drop(c);

    // Both machines change one setting to different values: the pair settles on one value
    // within two rounds, and then stops committing
    a.tether_ok("config set packages.min_release_age_days 3")
        .await;
    b.tether_ok("config set packages.min_release_age_days 14")
        .await;
    for _ in 0..2 {
        a.tether_ok("sync").await;
        b.tether_ok("sync").await;
    }
    let days = |cfg: toml::Table| cfg["packages"]["min_release_age_days"].as_integer();
    assert_eq!(days(config(&a).await), days(config(&b).await));
    settle(&lab, &a, &b, 3, "same-setting conflict").await;

    // Both machines add to a set list: both write the union in one order, so no machine
    // rewrites the other's order
    a.tether_ok(r#"config set packages.allow_scripts '["zeta", "esbuild"]'"#)
        .await;
    b.tether_ok(r#"config set packages.allow_scripts '["esbuild", "alpha"]'"#)
        .await;
    a.tether_ok("sync").await;
    b.tether_ok("sync").await;
    a.tether_ok("sync").await;
    for m in [&a, &b] {
        assert_eq!(
            allow_scripts(m).await,
            vec!["alpha", "esbuild", "zeta"],
            "{}",
            m.name
        );
    }
    settle(&lab, &a, &b, 3, "set list order").await;

    // A cleared list stays cleared
    a.tether_ok("config set packages.allow_scripts '[]'").await;
    a.tether_ok("sync").await;
    b.tether_ok("sync").await;
    a.tether_ok("sync").await;
    for m in [&a, &b] {
        assert!(allow_scripts(m).await.is_empty(), "{}", m.name);
    }

    // A dry run neither merges nor writes the base
    a.tether_ok("config set dashboard.theme latte").await;
    a.tether_ok("sync").await;
    let base = "/root/.tether/config.base.toml";
    let before = (sha(&b, CONFIG).await, sha(&b, base).await);
    b.tether_ok("sync --dry-run").await;
    assert_eq!(before, (sha(&b, CONFIG).await, sha(&b, base).await));
    // Without config.toml, a dry run runs with the defaults and writes none
    b.ok(&format!("mv {CONFIG} /root/config.away")).await;
    b.tether_ok("sync --dry-run").await;
    b.ok(&format!(
        "! test -e {CONFIG} && mv /root/config.away {CONFIG}"
    ))
    .await;
    // A write makes a config.toml that was 0644 private again
    b.ok(&format!("chmod 644 {CONFIG}")).await;
    b.tether_ok("sync").await;
    assert_eq!(
        config(&b).await["dashboard"]["theme"].as_str(),
        Some("latte")
    );
    let modes = b.ok(&format!("stat -c %a {CONFIG} {base}")).await.stdout;
    assert_eq!(modes.split_whitespace().collect::<Vec<_>>(), ["600", "600"]);
}

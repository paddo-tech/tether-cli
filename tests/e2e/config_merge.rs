//! config.toml merges between HEAD machines: a machine that only rewrote the format of its
//! config, or changed another setting, never exports over another machine's profile change.

use crate::harness::{enabled, Lab, Machine, HEAD};

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
}

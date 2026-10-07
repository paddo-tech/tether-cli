//! The approval inbox on a HEAD machine: packages from an untrusted machine wait, versions
//! from trusted machines wait while auto-install is off, and approve, reject and
//! approve --all --from decide them.

use crate::harness::{enabled, Lab, Machine, HEAD};

/// The inbox item with `id`, if any.
async fn item(m: &Machine, id: &str) -> Option<serde_json::Value> {
    m.inbox().await.into_iter().find(|i| i["id"] == id)
}

#[tokio::test(flavor = "multi_thread")]
async fn inbox() {
    if !enabled("inbox") {
        return;
    }
    let lab = Lab::new("inbox").await;
    let a = lab.machine("a", &[HEAD]).await;
    let b = lab.machine("b", &[HEAD]).await;
    let c = lab.machine("c", &[HEAD]).await;
    a.seed("npm", "unsigned-a", "1.0.0").await;
    for m in [&a, &b, &c] {
        assert_eq!(m.init(&lab).await.code, 0, "init {}", m.name);
    }

    // A package from a machine b does not trust waits and does not install
    b.tether_ok("sync").await;
    let held = item(&b, "npm:unsigned-a").await.expect("held");
    assert_eq!(held["reasons"], serde_json::json!(["unsigned"]));
    assert_eq!(held["bulk_approvable"], true);
    b.tether_ok("sync").await;
    assert_eq!(b.installed("npm", "unsigned-a").await, None);

    // b trusts a and c but installs nothing on its own, so each version waits
    for peer in [&a, &c] {
        let fp = peer.fingerprint().await;
        b.tether_ok(&format!("machines trust {} --fingerprint {fp}", peer.name))
            .await;
    }
    b.tether_ok("config set packages.auto_install_from_trusted false")
        .await;
    a.seed("npm", "exact", "1.1.0").await;
    a.seed("npm", "rejected", "1.0.0").await;
    a.seed("npm", "bulk-one", "1.0.0").await;
    a.seed("pnpm", "bulk-two", "2.0.0").await;
    c.seed("npm", "from-c", "1.0.0").await;
    for m in [&a, &c, &b] {
        m.tether_ok("sync").await;
    }
    assert_eq!(b.installed("npm", "exact").await, None);
    let exact = item(&b, "npm:exact").await.expect("exact waits");
    assert_eq!(exact["version"], "1.1.0");
    assert_eq!(exact["expect"], "1.1.0");

    // approve needs the reviewed version without a terminal, and refuses another one
    let out = b.tether("packages approve npm:exact").await;
    assert_eq!(out.code, 1, "{}", out.text());
    assert!(out.text().contains("--expect 1.1.0"), "{}", out.text());
    let out = b.tether("packages approve npm:exact --expect 1.0.0").await;
    assert_eq!(out.code, 1, "{}", out.text());
    b.tether_ok("packages approve npm:exact --expect 1.1.0")
        .await;
    assert_eq!(b.installed("npm", "exact").await.as_deref(), Some("1.1.0"));
    let specs: Vec<String> = b
        .installs()
        .await
        .into_iter()
        .filter(|i| i.name == "exact")
        .map(|i| i.spec)
        .collect();
    assert_eq!(specs, ["exact@1.1.0"]);

    // A rejected version stays out; a new version waits again
    b.tether_ok("packages reject npm:rejected --expect 1.0.0")
        .await;
    b.tether_ok("sync").await;
    assert!(item(&b, "npm:rejected").await.is_none());
    a.ok("sed -i 's/^rejected .*/rejected 1.2.0/' /state/pkgs/npm")
        .await;
    a.tether_ok("sync").await;
    b.tether_ok("sync").await;
    let again = item(&b, "npm:rejected").await.expect("new version waits");
    assert_eq!(again["version"], "1.2.0");
    assert_eq!(b.installed("npm", "rejected").await, None);

    // approve --all --from a installs a's waiting packages and leaves c's package and the
    // rejected version alone
    b.tether_ok("packages reject npm:rejected --expect 1.2.0")
        .await;
    let out = b.tether("packages approve --all --from a").await;
    assert_eq!(
        out.code,
        1,
        "--all needs -y without a terminal:\n{}",
        out.text()
    );
    b.tether_ok("packages approve --all --from a -y").await;
    assert_eq!(
        b.installed("npm", "bulk-one").await.as_deref(),
        Some("1.0.0")
    );
    assert_eq!(
        b.installed("pnpm", "bulk-two").await.as_deref(),
        Some("2.0.0")
    );
    assert_eq!(
        b.installed("npm", "unsigned-a").await.as_deref(),
        Some("1.0.0")
    );
    assert_eq!(b.installed("npm", "from-c").await, None);
    assert!(item(&b, "npm:from-c").await.is_some());
    assert_eq!(b.installed("npm", "rejected").await, None);
}

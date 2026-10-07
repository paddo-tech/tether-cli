//! The CLI contract on a machine with a sync repo: each --json command prints one JSON value
//! on stdout, and each error prints one `Error:` line on stderr and exits 1.

use crate::harness::{enabled, Lab, HEAD};

#[tokio::test(flavor = "multi_thread")]
async fn cli_contract() {
    if !enabled("cli_contract") {
        return;
    }
    let lab = Lab::new("cli_contract").await;
    let a = lab.machine("a", &[HEAD]).await;
    let b = lab.machine("b", &[HEAD]).await;
    a.seed("npm", "typescript", "5.0.0").await;
    b.seed("npm", "held", "1.0.0").await;
    assert_eq!(a.init(&lab).await.code, 0);
    assert_eq!(b.init(&lab).await.code, 0);
    a.tether_ok("sync").await;

    for (args, check) in [
        ("status --json", "/machine"),
        ("machines list --json", "/1/record"),
        ("packages list --json", "/packages/0/id"),
        ("packages inbox --json", "/0/id"),
    ] {
        let out = a.tether_ok(args).await;
        let v: serde_json::Value = serde_json::from_str(&out.stdout).unwrap_or_else(|e| {
            panic!("`tether {args}` stdout is not JSON ({e}):\n{}", out.stdout)
        });
        assert!(
            v.pointer(check).is_some(),
            "`tether {args}` has no {check}:\n{v:#}"
        );
    }
    let list: serde_json::Value =
        serde_json::from_str(&a.tether_ok("packages list --json").await.stdout).unwrap();
    assert!(list["packages"]
        .as_array()
        .unwrap()
        .iter()
        .any(|p| p["id"] == "npm:typescript" && p["version"] == "5.0.0"));
    let inbox = a.inbox().await;
    assert!(inbox.iter().any(|i| i["id"] == "npm:held"), "{inbox:?}");

    for args in [
        "machines show nope",
        "machines trust nope --fingerprint SHA256:x",
        "packages approve npm:nope --expect 1.0.0",
        "packages reject npm:held --expect 9.9.9",
        "packages share npm:typescript --to nope",
        "config set sync.nope 1",
        "upgrade",
    ] {
        let out = a.tether(args).await;
        assert_eq!(out.code, 1, "`tether {args}` exits 1:\n{}", out.text());
        assert!(
            out.stderr.contains("Error: "),
            "`tether {args}` prints Error: on stderr:\n{}",
            out.text()
        );
    }
}

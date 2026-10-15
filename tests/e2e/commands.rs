//! Signed fleet commands between HEAD machines: a command runs once on its target, a
//! replayed or expired command does not run, a command from a key the target does not trust
//! waits, and the signer refuses a target whose Tether cannot run commands.

use crate::harness::{enabled, Lab, Machine, HEAD, OLD, PRE_COMMANDS};

/// Has `m` trust each of `peers` by fingerprint.
async fn trust(m: &Machine, peers: &[&Machine]) {
    for peer in peers {
        let fp = peer.fingerprint().await;
        m.tether_ok(&format!("machines trust {} --fingerprint {fp}", peer.name))
            .await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn signed_commands() {
    if !enabled("signed_commands") {
        return;
    }
    let lab = Lab::new("signed_commands").await;
    let a = lab.machine("a", &[HEAD]).await;
    let b = lab.machine("b", &[HEAD]).await;
    let c = lab.machine("c", &[HEAD]).await;
    let older = lab.machine("older", &[PRE_COMMANDS]).await;
    let v1 = lab.machine("v1", &[OLD[2]]).await;
    for name in ["alpha", "beta", "gamma"] {
        b.seed("npm", name, "1.0.0").await;
    }
    for m in [&a, &b, &c, &older, &v1] {
        assert_eq!(m.init(&lab).await.code, 0, "init {}", m.name);
    }
    for m in [&a, &b, &c, &older] {
        m.tether_ok("sync").await;
    }
    let a_id = a.machine_id().await;
    let b_id = b.machine_id().await;
    trust(&a, &[&b, &c, &older]).await;
    trust(&b, &[&a]).await;
    trust(&c, &[&b]).await;

    // a uninstalls alpha on b; b runs it on its next sync and stops installing it
    let out = a
        .tether_ok("packages uninstall npm:alpha --on b")
        .await
        .text();
    assert!(out.contains("Sent 'uninstall npm:alpha' to"), "{out}");
    let sent = lab.head().await;
    let out = b.tether_ok("sync").await.text();
    assert!(
        out.contains("Fleet command applied: uninstall npm:alpha from"),
        "{out}"
    );
    assert_eq!(b.installed("npm", "alpha").await, None);
    let record: serde_json::Value = serde_json::from_str(
        &lab.remote(&format!("git show main:machines/{b_id}.json"))
            .await,
    )
    .unwrap();
    assert_eq!(
        record["removed_packages"]["npm"],
        serde_json::json!(["alpha"])
    );
    let show = a.tether_ok("machines show b").await.text();
    assert!(show.contains("pending: uninstall npm:alpha"), "{show}");
    a.tether_ok("sync").await;
    let show = a.tether_ok("machines show b").await.text();
    assert!(show.contains("applied: uninstall npm:alpha"), "{show}");
    assert!(!show.contains("pending:"), "{show}");
    assert_eq!(
        lab.remote("git ls-tree -r --name-only main commands/")
            .await,
        "",
        "a deletes its command once b reported it"
    );

    // The command runs once: b installs alpha again by hand, and a replay of the file changes
    // nothing
    b.seed("npm", "alpha", "1.0.0").await;
    b.tether_ok("sync").await;
    lab.push_edit(&format!("git checkout {sent} -- commands/"))
        .await;
    let out = b.tether_ok("sync").await.text();
    assert!(!out.contains("Fleet command"), "{out}");
    assert_eq!(b.installed("npm", "alpha").await.as_deref(), Some("1.0.0"));

    // An expired command that a's key really signed does not run
    let body = format!(
        "target = \"{b_id}\"\nsigner = \"{a_id}\"\nseq = 1000\n\
         expires_at = \"2020-01-01T00:00:00Z\"\naction = \"uninstall\"\nmanager = \"npm\"\n\
         name = \"beta\"\n"
    );
    let signed = a
        .ok(&format!(
            "cd /tmp && printf '%s' '{body}' > cmd.toml && \
             printf 'tether-command-v1\\n{b_id}\\n{a_id}\\n1000\\n1577836800\\n%s\\n' \
             \"$(sha256sum cmd.toml | cut -d' ' -f1)\" > msg && rm -f msg.sig && \
             ssh-keygen -q -Y sign -f /root/.tether/signing_key -n tether-command msg && \
             echo \"$(base64 -w0 cmd.toml) $(base64 -w0 msg.sig)\""
        ))
        .await
        .stdout;
    let (toml_b64, sig_b64) = signed.trim().split_once(' ').unwrap();
    lab.push_edit(&format!(
        "mkdir -p commands/{b_id} && echo {toml_b64} | base64 -d > commands/{b_id}/{a_id}-1000.toml \
         && echo {sig_b64} | base64 -d > commands/{b_id}/{a_id}-1000.toml.sig"
    ))
    .await;
    let out = b.tether_ok("sync").await.text();
    assert!(
        out.contains("Fleet command expired: uninstall npm:beta"),
        "{out}"
    );
    assert_eq!(b.installed("npm", "beta").await.as_deref(), Some("1.0.0"));

    // b does not trust c, so c's command waits and does not run
    c.tether_ok("sync").await;
    c.tether_ok("packages uninstall npm:gamma --on b").await;
    let out = b.tether_ok("sync").await.text();
    assert!(out.contains("this machine does not trust"), "{out}");
    assert_eq!(b.installed("npm", "gamma").await.as_deref(), Some("1.0.0"));
    c.tether_ok("sync").await;
    let show = c.tether_ok("machines show b").await.text();
    assert!(show.contains("pending: uninstall npm:gamma"), "{show}");

    // The signer refuses a Tether that cannot run commands, and a 1.x machine it cannot trust
    let out = a.tether("packages uninstall npm:alpha --on older").await;
    assert_eq!(out.code, 1, "{}", out.text());
    assert!(
        out.text().contains("which cannot run fleet commands"),
        "{}",
        out.text()
    );
    // An untrusted record never lends its hostname, so only its id names the 1.x machine
    let v1_id = v1.machine_id().await;
    let out = a
        .tether(&format!("packages uninstall npm:alpha --on {v1_id}"))
        .await;
    assert_eq!(out.code, 1, "{}", out.text());
    assert!(out.text().contains("is not trusted"), "{}", out.text());
    let out = a
        .tether_ok("machines profile set dev --on all")
        .await
        .text();
    assert!(
        out.contains("Tether 2.0.1 cannot run fleet commands"),
        "{out}"
    );
}

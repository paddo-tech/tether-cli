//! Machine trust between two HEAD machines: trust by fingerprint, then a tampered record, a
//! replayed record and a changed key, each pushed by someone without the machine's key.

use crate::harness::{enabled, Lab, HEAD};

/// Edits machine `id`'s record in a checkout of the remote without signing it again: adds
/// an npm package to the record and the manifest.
fn tamper(id: &str, package: &str) -> String {
    format!(
        "python3 -c \"import json; p='machines/{id}.json'; d=json.load(open(p)); \
         d['packages']['npm'].append('{package}'); d['package_versions']['npm']['{package}']='1.0.0'; \
         open(p, 'w').write(json.dumps(d, indent=2))\" && echo {package} >> manifests/npm.txt"
    )
}

#[tokio::test(flavor = "multi_thread")]
async fn trust() {
    if !enabled("trust") {
        return;
    }
    let lab = Lab::new("trust").await;
    let a = lab.machine("a", &[HEAD]).await;
    let b = lab.machine("b", &[HEAD]).await;
    a.seed("npm", "alpha", "1.0.0").await;
    assert_eq!(a.init(&lab).await.code, 0, "init a");
    assert_eq!(b.init(&lab).await.code, 0, "init b");
    let a_id = a.machine_id().await;
    let a_key = a.fingerprint().await;
    let first_record = lab.head().await;

    // b holds a's package and key until it trusts that key
    b.tether_ok("sync").await;
    assert_eq!(b.installed("npm", "alpha").await, None);
    let inbox = b.inbox().await;
    let key = inbox
        .iter()
        .find(|i| i["id"] == format!("machine:{a_id}"))
        .expect("a's key waits in b's inbox");
    assert_eq!(key["fingerprint"], a_key.as_str());
    let show = b.tether_ok(&format!("machines show {a_id}")).await.text();
    assert!(
        show.contains(&a_key),
        "machines show names the key:\n{show}"
    );
    assert!(show.contains("not trusted"), "{show}");
    let wrong = b
        .tether("machines trust a --fingerprint SHA256:AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA")
        .await;
    assert_eq!(wrong.code, 1, "trust with a wrong fingerprint fails");

    b.tether_ok(&format!("machines trust a --fingerprint {a_key}"))
        .await;
    b.tether_ok("sync").await;
    assert_eq!(b.installed("npm", "alpha").await.as_deref(), Some("1.0.0"));
    let show = b.tether_ok("machines show a").await.text();
    assert!(show.contains("Trust           trusted"), "{show}");

    // A record edited in the repo fails its signature: b warns and installs nothing from it
    lab.push_edit(&tamper(&a_id, "evil")).await;
    let out = b.tether_ok("sync").await.text();
    assert!(
        out.contains(&format!("Record for {a_id} fails its signature")),
        "tampered record warns:\n{out}"
    );
    assert_eq!(b.installed("npm", "evil").await, None);
    let list = b.tether_ok("machines list").await.text();
    assert!(list.contains("signature failed (ignored)"), "{list}");
    // The tampered record still claims a 2.x build, so a is not a 1.x machine
    assert!(!list.contains("on 1.x"), "{list}");
    let status = b.tether_ok("status").await.text();
    assert!(!status.contains("on 1.x"), "{status}");

    // a signs a new record over the tampered one, and b trusts it again
    a.seed("npm", "beta", "1.0.0").await;
    a.tether_ok("sync").await;
    b.tether_ok("sync").await;
    assert_eq!(b.installed("npm", "beta").await.as_deref(), Some("1.0.0"));
    assert_eq!(b.installed("npm", "evil").await, None);

    // An older record that a really signed, pushed back, is a replay: b refuses it
    lab.push_edit(&format!(
        "git checkout {first_record} -- machines/{a_id}.json machines/{a_id}.json.sig && \
         echo replayed >> manifests/npm.txt"
    ))
    .await;
    let out = b.tether_ok("sync").await.text();
    assert!(
        out.contains(&format!("Ignoring machines/{a_id}.json"))
            && out.contains("replaying an old record"),
        "replayed record warns:\n{out}"
    );
    assert_eq!(b.installed("npm", "replayed").await, None);
    let show = b.tether_ok("machines show a").await.text();
    assert!(show.contains("replayed"), "{show}");

    // a gets a new key: b shows the change and installs nothing from a until it trusts it
    a.ok("rm -f /root/.tether/signing_key /root/.tether/signing_key.pub")
        .await;
    a.seed("npm", "gamma", "1.0.0").await;
    a.tether_ok("sync").await;
    let new_key = a.fingerprint().await;
    assert_ne!(new_key, a_key);
    let out = b.tether_ok("sync").await.text();
    assert!(
        out.contains(&format!("SIGNING KEY CHANGED for machine {a_id}")),
        "key change warns:\n{out}"
    );
    assert_eq!(b.installed("npm", "gamma").await, None);
    let inbox = b.inbox().await;
    let key = inbox
        .iter()
        .find(|i| i["id"] == format!("machine:{a_id}"))
        .expect("the new key waits in b's inbox");
    assert_eq!(key["fingerprint"], new_key.as_str());
    assert!(key["reasons"].to_string().contains("key_changed"), "{key}");
    b.tether_ok("sync").await;
    assert_eq!(b.installed("npm", "gamma").await, None, "still blocked");

    b.tether_ok(&format!(
        "packages approve machine:{a_id} --expect {new_key}"
    ))
    .await;
    b.tether_ok("sync").await;
    assert_eq!(b.installed("npm", "gamma").await.as_deref(), Some("1.0.0"));
}

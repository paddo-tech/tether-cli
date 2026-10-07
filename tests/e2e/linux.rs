//! Linux behaviour: casks, the systemd user service and desktop notifications.

use crate::harness::{enabled, Lab, Machine, HEAD};
use std::time::Duration;

#[tokio::test(flavor = "multi_thread")]
async fn casks_never_import_on_linux() {
    if !enabled("casks_never_import_on_linux") {
        return;
    }
    let lab = Lab::new("casks_never_import_on_linux").await;
    let a = lab.machine("a", &[HEAD]).await;
    let b = lab.machine("b", &[HEAD]).await;
    a.seed("brew_formulae", "jq", "1.0.0").await;
    a.seed("brew_casks", "zoom", "1.0.0").await;
    assert_eq!(a.init(&lab).await.code, 0);
    assert_eq!(b.init(&lab).await.code, 0);
    let fp = a.fingerprint().await;
    b.tether_ok(&format!("machines trust a --fingerprint {fp}"))
        .await;
    // A cask from a Mac in the Brewfile, as a Mac machine writes it
    lab.push_edit("echo 'cask \"iterm2\"' >> manifests/Brewfile")
        .await;
    b.tether_ok("sync").await;
    b.tether_ok("sync").await;

    assert_eq!(
        b.installed("brew_formulae", "jq").await.as_deref(),
        Some("1.0.0")
    );
    let casks: Vec<_> = b
        .installs()
        .await
        .into_iter()
        .filter(|i| i.key == "brew_casks")
        .collect();
    assert!(casks.is_empty(), "b installed casks: {casks:?}");
    let cask_calls: Vec<_> = b
        .calls()
        .await
        .into_iter()
        .filter(|c| c["argv"].to_string().contains("--cask") && c["tool"] == "brew")
        .filter(|c| c["argv"][0] == "install")
        .collect();
    assert!(cask_calls.is_empty(), "{cask_calls:?}");
    let inbox = b.inbox().await;
    assert!(
        !inbox.iter().any(|i| i["manager"] == "brew_casks"),
        "casks wait in the inbox: {inbox:?}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn systemd_install_needs_a_user_session() {
    if !enabled("systemd_install_needs_a_user_session") {
        return;
    }
    let lab = Lab::new("systemd_install_needs_a_user_session").await;
    let a = lab.machine("a", &[HEAD]).await;
    assert_eq!(a.init(&lab).await.code, 0);
    // systemctl --user without a user bus, as in a container or an ssh session
    a.ok("rm /shims/systemctl && printf '#!/bin/sh\\necho \"Failed to connect to bus: No medium found\" >&2\\nexit 1\\n' > /shims/systemctl && chmod +x /shims/systemctl")
        .await;
    let out = a.tether("daemon install").await;
    assert_eq!(out.code, 1, "{}", out.text());
    assert!(
        out.stderr.starts_with("Error: ") && out.text().contains("No systemd user session found"),
        "{}",
        out.text()
    );
    let unit = a
        .sh("test -e /root/.config/systemd/user/tether.service")
        .await;
    assert_ne!(unit.code, 0, "the unit file was written");
}

/// The inbox notifications notify-send showed, oldest first.
async fn inbox_notifications(m: &Machine) -> Vec<String> {
    m.calls()
        .await
        .into_iter()
        .filter(|c| c["tool"] == "notify-send")
        .map(|c| c["argv"].to_string())
        .filter(|a| a.contains("for approval"))
        .collect()
}

/// Makes the running daemon sync now, and waits until that sync ends.
async fn daemon_sync(m: &Machine, n: usize) {
    m.ok("kill -HUP $(cat /root/.tether/daemon.pid)").await;
    wait_for_log(m, "Received SIGHUP", n).await;
    // The sync holds the sync lock from just after that line until it ends
    tokio::time::sleep(Duration::from_millis(500)).await;
    m.ok("flock /root/.tether/sync.lock true").await;
}

async fn wait_for_log(m: &Machine, line: &str, count: usize) {
    for _ in 0..300 {
        let log = m.read("/root/.tether/daemon.log").await;
        if log.matches(line).count() >= count {
            return;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!(
        "{}: daemon.log never had {count} x {line:?}:\n{}",
        m.name,
        m.read("/root/.tether/daemon.log").await
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn notify_send_once_per_inbox_batch() {
    if !enabled("notify_send_once_per_inbox_batch") {
        return;
    }
    let lab = Lab::new("notify_send_once_per_inbox_batch").await;
    let a = lab.machine("a", &[HEAD]).await;
    let b = lab.machine("b", &[HEAD]).await;
    assert_eq!(a.init(&lab).await.code, 0);
    assert_eq!(b.init(&lab).await.code, 0);
    b.tether_ok("daemon start").await;
    wait_for_log(&b, "Daemon starting", 1).await;

    a.seed("npm", "first", "1.0.0").await;
    a.tether_ok("sync").await;
    daemon_sync(&b, 1).await;
    let shown = inbox_notifications(&b).await;
    assert_eq!(shown.len(), 1, "{shown:?}");
    assert!(shown[0].contains("first waits for approval"), "{shown:?}");

    // The same held package is not a new batch
    daemon_sync(&b, 2).await;
    assert_eq!(inbox_notifications(&b).await.len(), 1);

    a.seed("npm", "second", "1.0.0").await;
    a.seed("npm", "third", "1.0.0").await;
    a.tether_ok("sync").await;
    daemon_sync(&b, 3).await;
    let shown = inbox_notifications(&b).await;
    assert_eq!(shown.len(), 2, "{shown:?}");
    assert!(
        shown[1].contains("2 packages wait for approval"),
        "{shown:?}"
    );
    b.tether_ok("daemon stop").await;
}

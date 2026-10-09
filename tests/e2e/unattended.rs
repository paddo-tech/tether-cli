//! `tether sync` without a terminal, as from cron or ssh: it runs as the daemon does.

use crate::harness::{enabled, Lab, HEAD};

#[tokio::test(flavor = "multi_thread")]
async fn sync_without_a_terminal_skips_conflicts() {
    if !enabled("sync_without_a_terminal_skips_conflicts") {
        return;
    }
    let lab = Lab::new("sync_without_a_terminal_skips_conflicts").await;
    let a = lab.machine("a", &[HEAD]).await;
    let b = lab.machine("b", &[HEAD]).await;
    assert_eq!(a.init(&lab).await.code, 0, "init a");
    assert_eq!(b.init(&lab).await.code, 0, "init b");
    b.tether_ok("sync").await;

    a.ok("echo 'export FROM=a' >> /root/.bashrc").await;
    a.tether_ok("sync").await;
    b.ok("echo 'export FROM=b' >> /root/.bashrc").await;
    let head = lab.head().await;
    let out = b.tether("sync").await;
    assert_eq!(out.code, 0, "sync without a terminal:\n{}", out.text());
    let text = out.text();
    assert!(text.contains(".bashrc (conflict - skipped)"), "{text}");
    assert!(!text.contains("needs a terminal"), "{text}");
    // The conflict waits for 'tether resolve', and b keeps its own file
    let conflicts = b.read("/root/.tether/conflicts.json").await;
    assert!(conflicts.contains(".bashrc"), "{conflicts}");
    assert!(b.read("/root/.bashrc").await.contains("FROM=b"));
    let notify = b.calls().await;
    assert!(
        notify
            .iter()
            .any(|c| c["tool"] == "notify-send"
                && c["argv"].to_string().contains("Conflict in .bashrc")),
        "b notifies about the conflict: {notify:?}"
    );
    // The skipped conflict keeps the remote file
    let pushed = lab.changed(&head, &lab.head().await).await;
    assert!(
        !pushed.iter().any(|p| p.contains("bashrc")),
        "b pushed its file over the conflict: {pushed:?}"
    );

    // -y answers prompts, but a conflict still waits: its merge tool needs a terminal
    let out = b.tether("sync -y").await;
    assert_eq!(out.code, 0, "sync -y without a terminal:\n{}", out.text());
    let text = out.text();
    assert!(text.contains(".bashrc (conflict - skipped)"), "{text}");
    let pushed = lab.changed(&head, &lab.head().await).await;
    assert!(!pushed.iter().any(|p| p.contains("bashrc")), "{pushed:?}");
    a.tether_ok("sync").await;
    let bashrc = a.read("/root/.bashrc").await;
    assert!(
        bashrc.contains("FROM=a") && !bashrc.contains("FROM=b"),
        "{bashrc}"
    );
}

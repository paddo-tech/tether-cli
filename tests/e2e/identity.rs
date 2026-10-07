//! Machines without a git identity, as on a new Linux machine: two HEAD machines push at the
//! same time, so one push is rejected and that machine rebases onto the other's commit.

use crate::harness::{enabled, Lab, Machine, HEAD};

/// Every push the server receives, rejected or not, appends a line to this file
const PUSHES: &str = "/tmp/pushes";

async fn pushes(lab: &Lab) -> usize {
    lab.remote(&format!("cat {PUSHES} 2>/dev/null"))
        .await
        .lines()
        .count()
}

async fn assert_nothing_discarded(m: &Machine) {
    let branches = m
        .ok("git -C /root/.tether/sync branch --list 'tether-discarded-*'")
        .await
        .stdout;
    assert_eq!(branches.trim(), "", "{} discarded commits", m.name);
}

/// The record `m` pushed lists the package `name`.
async fn remote_lists(lab: &Lab, m: &Machine, name: &str) -> bool {
    let id = m.machine_id().await;
    lab.remote(&format!("git show main:machines/{id}.json"))
        .await
        .contains(&format!("\"{name}\""))
}

#[tokio::test(flavor = "multi_thread")]
async fn rejected_push_without_git_identity() {
    if !enabled("rejected_push_without_git_identity") {
        return;
    }
    let lab = Lab::new("rejected_push_without_git_identity").await;
    let a = lab.machine("a", &[HEAD]).await;
    let b = lab.machine("b", &[HEAD]).await;
    for m in [&a, &b] {
        let ident = m.sh("cd /tmp && git var GIT_COMMITTER_IDENT").await;
        assert_ne!(ident.code, 0, "{} has a git identity", m.name);
    }
    assert_eq!(a.init(&lab).await.code, 0, "init a");
    assert_eq!(b.init(&lab).await.code, 0, "init b");

    // A slow pre-receive hook holds both pushes, so the later one finds main moved
    lab.remote(&format!(
        "printf '#!/bin/sh\\necho push >> {PUSHES}\\nsleep 5\\n' > hooks/pre-receive && \
         chmod +x hooks/pre-receive"
    ))
    .await;
    // Different managers, so the machines change different files: the manifests merge
    // line by line, and two lines added at one place are a real conflict
    a.seed("npm", "from-a", "1.0.0").await;
    b.seed("uv", "from-b", "1.0.0").await;
    let (out_a, out_b) = tokio::join!(a.tether("sync"), b.tether("sync"));
    for (m, out) in [(&a, &out_a), (&b, &out_b)] {
        assert_eq!(out.code, 0, "{} sync failed:\n{}", m.name, out.text());
        assert!(
            !out.text().contains("conflicted"),
            "{}:\n{}",
            m.name,
            out.text()
        );
    }
    let count = pushes(&lab).await;
    lab.note(&format!("pushes in the concurrent round: {count}"));
    assert!(count >= 3, "one push was rejected and retried, got {count}");
    lab.remote("rm -f hooks/pre-receive").await;

    for m in [&a, &b] {
        assert_nothing_discarded(m).await;
        assert!(remote_lists(&lab, m, &format!("from-{}", m.name)).await);
    }
    // Both machines are synced: a sync on each pushes nothing new
    a.tether_ok("sync").await;
    b.tether_ok("sync").await;
    let head = lab.head().await;
    a.tether_ok("sync").await;
    b.tether_ok("sync").await;
    assert_eq!(lab.head().await, head, "a settled fleet pushes nothing");
    for m in [&a, &b] {
        assert_nothing_discarded(m).await;
        let local = m.ok("git -C /root/.tether/sync rev-parse HEAD").await;
        assert_eq!(local.stdout.trim(), head, "{} is behind", m.name);
    }
}

//! Runs the tether binary in a scratch HOME with an empty PATH, so no package manager, git
//! or editor can run, and stdin is not a terminal.

use assert_cmd::Command;
use predicates::prelude::*;
use std::path::Path;
use tempfile::TempDir;

/// A HOME with a default config, as after `tether init` without a repo.
fn home() -> TempDir {
    let dir = tempfile::tempdir().unwrap();
    let tether = dir.path().join(".tether");
    std::fs::create_dir_all(&tether).unwrap();
    let config = toml::to_string(&tether::Config::default()).unwrap();
    std::fs::write(tether.join("config.toml"), config).unwrap();
    dir
}

fn tether(home: &Path) -> Command {
    let empty = home.join("empty-path");
    std::fs::create_dir_all(&empty).unwrap();
    let mut cmd = Command::cargo_bin("tether").unwrap();
    cmd.env_clear()
        .env("HOME", home)
        .env("PATH", empty)
        .write_stdin("");
    cmd
}

/// An error prints one `Error:` line on stderr and exits 1.
fn fails(home: &Path, args: &[&str], message: &str) {
    tether(home)
        .args(args)
        .assert()
        .code(1)
        .stderr(predicate::str::starts_with("Error: "))
        .stderr(predicate::str::contains(message));
}

#[test]
fn config_errors_exit_1() {
    let h = home();
    fails(h.path(), &["config", "get", "nope.nope"], "not found");
    fails(
        h.path(),
        &["config", "set", "sync.nope", "5"],
        "Unknown config key",
    );
    fails(
        h.path(),
        &["config", "features", "enable", "nope"],
        "Unknown feature",
    );
    fails(
        h.path(),
        &["config", "features", "disable", "nope"],
        "Unknown feature",
    );
    fails(h.path(), &["config", "edit"], "needs a terminal");
    tether(h.path())
        .args(["config", "get", "sync.interval"])
        .assert()
        .success()
        .stdout("5m\n");
}

#[test]
fn machines_errors_exit_1() {
    let h = home();
    fails(
        h.path(),
        &["machines", "rename", "zzz", "yyy"],
        "can rename itself",
    );
    fails(
        h.path(),
        &["machines", "remove", "../x"],
        "Invalid machine id",
    );
    fails(
        h.path(),
        &["machines", "profile", "set", "nope"],
        "not found",
    );
    fails(
        h.path(),
        &["machines", "profile", "edit", "nope"],
        "not found",
    );
}

#[test]
fn identity_and_unlock_errors_exit_1() {
    let h = home();
    fails(h.path(), &["identity", "unlock"], "No identity found");
    fails(h.path(), &["unlock"], "No encrypted key found");
}

#[test]
fn team_errors_exit_1() {
    let h = home();
    fails(h.path(), &["team", "remove"], "No teams configured");
    fails(h.path(), &["team", "switch", "nope"], "No teams configured");
    fails(h.path(), &["team", "enable"], "not configured");
}

#[test]
fn collab_ignore_and_resolve_errors_exit_1() {
    let h = home();
    fails(
        h.path(),
        &["collab", "list"],
        "collab_secrets feature is disabled",
    );
    fails(
        h.path(),
        &["ignore", "remove", "nope"],
        "No ignore patterns",
    );
    fails(h.path(), &["resolve", "nope"], "No conflict found");
}

#[test]
fn status_without_init_exits_1() {
    let h = tempfile::tempdir().unwrap();
    fails(h.path(), &["status"], "not initialized");
}

#[test]
fn prompts_without_a_terminal_name_y_and_y_answers_them() {
    let h = home();
    let profiles = |h: &Path| {
        let text = std::fs::read_to_string(h.join(".tether/config.toml")).unwrap();
        toml::from_str::<tether::Config>(&text).unwrap().profiles
    };
    tether(h.path())
        .args(["machines", "profile", "create", "server"])
        .assert()
        .code(1)
        .stderr(predicate::str::contains("needs a terminal"))
        .stderr(predicate::str::contains("-y"));
    assert!(!profiles(h.path()).contains_key("server"));

    tether(h.path())
        .args(["-y", "machines", "profile", "create", "server"])
        .args(["--managers", "npm,uv"])
        .assert()
        .success();
    assert_eq!(profiles(h.path())["server"].packages, ["npm", "uv"]);

    tether(h.path())
        .args([
            "machines", "profile", "create", "server2", "--from", "server",
        ])
        .assert()
        .success();
    assert_eq!(profiles(h.path())["server2"].packages, ["npm", "uv"]);
    fails(
        h.path(),
        &["machines", "profile", "create", "x", "--from", "nope"],
        "not found",
    );
}

#[test]
fn bare_packages_lists_and_uninstall_needs_a_package() {
    let h = home();
    for args in [
        &["packages"][..],
        &["packages", "list"],
        &["packages", "--list"],
    ] {
        tether(h.path())
            .args(args)
            .assert()
            .success()
            .stdout(predicate::str::contains("No packages found"));
    }
    fails(h.path(), &["packages", "uninstall"], "Name the package");
    // `remove` stays an alias of `uninstall`
    fails(h.path(), &["packages", "remove", "nope"], "manager:name");
    fails(
        h.path(),
        &["packages", "uninstall", "nope:x"],
        "manager:name",
    );
}

#[test]
fn approve_all_lists_what_it_covers_and_needs_y_without_a_terminal() {
    use tether::packages::inbox::{Inbox, InboxItem, Kind, Reason};
    let h = home();
    let item = |manager: &str, name: &str, kind: Kind| InboxItem {
        kind,
        manager: manager.to_string(),
        name: name.to_string(),
        version: Some("1.0.0".to_string()),
        tap: None,
        source_machine: Some("other".to_string()),
        commit: None,
        signer: None,
        reasons: vec![Reason::Unsigned],
        advisories: Vec::new(),
        first_seen: chrono::Utc::now(),
    };
    let inbox = Inbox {
        items: vec![
            item("npm", "left-pad", Kind::Package),
            item(
                "machine",
                "other",
                Kind::TrustMachine {
                    public_key: String::new(),
                    fingerprint: "SHA256:x".to_string(),
                },
            ),
        ],
        ..Inbox::default()
    };
    std::fs::write(
        h.path().join(".tether/inbox.json"),
        serde_json::to_string(&inbox).unwrap(),
    )
    .unwrap();
    let items = json(h.path(), &["packages", "inbox", "--json"]);
    assert_eq!(items[0]["id"], "npm:left-pad");
    assert_eq!(items[0]["expect"], "1.0.0");
    assert_eq!(items[0]["bulk_approvable"], true);
    assert_eq!(items[0]["reasons"], serde_json::json!(["unsigned"]));
    assert_eq!(items[1]["kind"], "machine_key");
    assert_eq!(items[1]["expect"], "SHA256:x");
    assert_eq!(items[1]["bulk_approvable"], false);
    tether(h.path())
        .args(["packages", "approve", "--all"])
        .assert()
        .code(1)
        .stdout(predicate::str::contains("npm:left-pad 1.0.0"))
        .stdout(predicate::str::contains(
            "1 item(s) need their own decision",
        ))
        .stderr(predicate::str::contains("Pass -y"));
    // An unknown machine is an error, not an empty approval
    fails(
        h.path(),
        &["packages", "approve", "--all", "--from", "nobody"],
        "No machine nobody",
    );
    fails(
        h.path(),
        &["machines", "untrust", "nobody"],
        "No machine nobody",
    );
    // Approving one item needs the reviewed version without a terminal
    fails(
        h.path(),
        &["packages", "approve", "npm:left-pad"],
        "--expect 1.0.0",
    );
    fails(
        h.path(),
        &["packages", "approve", "npm:left-pad", "--expect", "2.0.0"],
        "is now 1.0.0",
    );
    // -y does not approve a single item
    fails(
        h.path(),
        &["-y", "packages", "approve", "npm:left-pad"],
        "--expect 1.0.0",
    );
    tether(h.path())
        .args(["packages", "approve"])
        .assert()
        .code(2);
}

#[test]
fn items_without_a_binding_or_with_a_failed_signature_need_a_review() {
    use tether::packages::inbox::{Inbox, InboxItem, Kind, Reason};
    let h = home();
    let item = |name: &str, version: Option<&str>, reason: Reason| InboxItem {
        kind: Kind::Package,
        manager: "npm".to_string(),
        name: name.to_string(),
        version: version.map(str::to_string),
        tap: None,
        source_machine: Some("other".to_string()),
        commit: None,
        signer: None,
        reasons: vec![reason],
        advisories: Vec::new(),
        first_seen: chrono::Utc::now(),
    };
    let inbox = Inbox {
        items: vec![
            item("unpinned", None, Reason::Unsigned),
            item("forged", Some("1.0.0"), Reason::SignatureFailed),
        ],
        ..Inbox::default()
    };
    std::fs::write(
        h.path().join(".tether/inbox.json"),
        serde_json::to_string(&inbox).unwrap(),
    )
    .unwrap();
    // No registry can resolve the release here, so nothing binds the unpinned item
    for args in [
        &["packages", "approve", "npm:unpinned"][..],
        &["-y", "packages", "approve", "npm:unpinned"],
        &["packages", "reject", "npm:unpinned"],
    ] {
        fails(h.path(), args, "needs a review in a terminal");
    }
    fails(
        h.path(),
        &["packages", "approve", "npm:unpinned", "--expect", "1.0.0"],
        "has no version, tap or key to name",
    );
    // approve --all covers neither
    tether(h.path())
        .args(["-y", "packages", "approve", "--all"])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "2 item(s) need their own decision",
        ))
        .stdout(predicate::str::contains("No packages to approve"));
    // A failed signature needs an explicit flag without a terminal, even with --expect
    fails(
        h.path(),
        &["packages", "approve", "npm:forged", "--expect", "1.0.0"],
        "--allow-signature-failed",
    );
    tether(h.path())
        .args(["packages", "approve", "npm:forged", "--expect", "1.0.0"])
        .arg("--allow-signature-failed")
        .assert()
        .code(1)
        .stderr(predicate::str::contains("--allow-signature-failed").not());
}

#[test]
fn daemon_status_and_logs() {
    let h = home();
    tether(h.path())
        .args(["daemon", "status"])
        .assert()
        .success()
        .stdout(predicate::str::contains("not running"))
        .stdout(predicate::str::contains("daemon.log"));
    std::fs::write(h.path().join(".tether/daemon.log"), "one\ntwo\nthree\n").unwrap();
    tether(h.path())
        .args(["daemon", "logs", "-n", "2"])
        .assert()
        .success()
        .stdout("two\nthree\n");
}

#[test]
fn status_counts_the_inbox_and_conflicts() {
    let h = home();
    tether(h.path())
        .args(["status"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Inbox"))
        .stdout(predicate::str::contains("Conflicts"));
}

#[test]
fn rename_with_two_names_is_deprecated_and_checks_this_machine() {
    let h = home();
    tether(h.path())
        .args(["machines", "rename", "zzz", "yyy"])
        .assert()
        .code(1)
        .stdout(predicate::str::contains("deprecated"))
        .stderr(predicate::str::contains("can rename itself"));
    fails(h.path(), &["machines", "show", "nope"], "No machine nope");
}

#[test]
fn ignore_secrets_and_files_with_the_old_forms_as_aliases() {
    let h = home();
    tether(h.path())
        .args(["ignore", "secrets", "add", "*.pem"])
        .assert()
        .success();
    tether(h.path())
        .args(["ignore", "list"])
        .assert()
        .success()
        .stdout(predicate::str::contains("*.pem"));
    tether(h.path())
        .args(["ignore", "remove", "*.pem"])
        .assert()
        .success();
    fails(
        h.path(),
        &["ignore", "secrets", "remove", "*.pem"],
        "not found",
    );
    let help = tether(h.path())
        .args(["ignore", "--help"])
        .output()
        .unwrap();
    let help = String::from_utf8(help.stdout).unwrap();
    assert!(help.contains("secrets") && help.contains("files"));
    assert!(!help.contains("sync-list"));
}

fn json(home: &Path, args: &[&str]) -> serde_json::Value {
    let out = tether(home).args(args).output().unwrap();
    assert!(
        out.status.success(),
        "{:?}: {}",
        args,
        String::from_utf8_lossy(&out.stderr)
    );
    serde_json::from_slice(&out.stdout).unwrap_or_else(|e| {
        panic!(
            "{:?} printed no JSON ({}): {}",
            args,
            e,
            String::from_utf8_lossy(&out.stdout)
        )
    })
}

#[test]
fn json_output_has_the_documented_fields() {
    let h = home();
    let status = json(h.path(), &["status", "--json"]);
    for field in [
        "machine",
        "profile",
        "version",
        "daemon",
        "inbox",
        "conflicts",
    ] {
        assert!(status.get(field).is_some(), "status has no {}", field);
    }
    assert_eq!(status["inbox"], 0);
    let list = json(h.path(), &["packages", "list", "--json"]);
    assert_eq!(list["packages"], serde_json::json!([]));
    assert!(list.get("profile").is_some());
    assert_eq!(
        json(h.path(), &["packages", "inbox", "--json"]),
        serde_json::json!([])
    );
    let machines = json(h.path(), &["machines", "list", "--json"]);
    assert!(machines.is_array());
}

#[test]
fn piped_output_has_no_colour() {
    let h = home();
    for args in [
        &["config", "features"][..],
        &["machines", "profile", "list"],
        &["config", "get", "nope"],
    ] {
        let out = tether(h.path()).args(args).output().unwrap();
        let text = [out.stdout, out.stderr].concat();
        assert!(!text.is_empty(), "{:?} printed nothing", args);
        assert!(
            !text.contains(&0x1b),
            "{:?} printed ANSI codes: {}",
            args,
            String::from_utf8_lossy(&text)
        );
    }
}

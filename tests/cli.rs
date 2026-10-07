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

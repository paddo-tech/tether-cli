//! Mixed-version fleets: 1.x releases and HEAD sync through one repo, one machine at a time,
//! for several rounds. Package managers are shims, so the test can count every install.
//!
//! `fleet` checks:
//!   a  every init and sync exits 0 and prints no config or parse error
//!   b  after the fleet settles, a round adds no commits to the remote other than a 1.x
//!      machine rewriting its own record, which 1.x does on every sync
//!   c  no 1.x machine installs a package it already has, or one it removed
//!   d  the manifests never return to an earlier state, and do not change once settled
//!   e  machines that start on HEAD never install a package that only 1.x machines list
//!   f  (a) to (d) still hold in the rounds after one 1.13.1 machine upgrades to HEAD
//!   g  the HEAD machine in profile "server" installs no package that only dev machines
//!      list, except SHARED while it is shared with the server profile
//!   h  after `packages share SHARED --to server` the server machine installs it
//!   i  after the server machine runs `packages remove SHARED`, it does not install it
//!      again, and no dev machine uninstalls it
//!   j  dev HEAD machines install each other's packages, and never the server's own
//!   k  the server keeps its profile assignment to the end. The server sets its profile
//!      before the other machines join: in a mixed fleet, a config.toml change made later
//!      flaps (see `config_flap`)
//!
//! `config_flap`: after the fleet settles, one HEAD machine changes config.toml once.
//! It checks (a), and
//!   l  after the round of the change, a HEAD machine commits config.toml only to restore
//!      the change after a 1.x machine exported its stale copy earlier in that round
//!   m  every HEAD machine ends with the change, and the last rounds commit no config.toml
//! Then a HEAD machine whose config.toml leaves out fields 1.x requires changes a setting,
//! and the 1.x machine saves its config again, without the marker. (a) still holds, and
//!   n  HEAD machines keep the 2.0 setting and take the change of the 1.x save
//!   o  the last rounds after the 1.x save commit no config.toml
//! It also logs how often 1.x machines commit config.toml per round, without failing on
//! it. Its 1.x machine runs TETHER_E2E_FLAP_REF (default v1.13.1), which can be any commit.

use crate::harness::{enabled, flap_ref, Lab, Machine, HEAD, INIT_SCRIPT, OLD};
use regex::Regex;
use serde_json::Value;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::fmt::Write as _;

const SETTLE: usize = 2;
const STEADY: usize = 2;
/// HEAD machines in profile "server"; every other machine stays in "dev"
const SERVERS: [&str; 1] = ["h3"];
/// A package only dev machines list, which h1 shares with the server profile and the
/// server then removes again
const SHARED: (&str, &str, &str) = ("h1", "npm", "head-npm-h1");
/// A package m1 removes after the fleet settles
const REMOVE: (&str, &str, &str) = ("m1", "npm", "cowsay");
/// Packages each machine has before `tether init`, as (machine, key, name, version).
/// Names that start with "old-" exist only on machines that start on 1.x, names that start
/// with "head-" only on machines that start on HEAD.
const SEEDS: &[(&str, &str, &str, &str)] = &[
    ("m1", "brew_formulae", "jq", "1.0.0"),
    ("m1", "npm", "typescript", "1.0.0"),
    ("m1", "npm", "cowsay", "1.0.0"),
    ("m1", "npm", "old-npm-m1", "1.0.0"),
    ("m1", "uv", "old-uv-m1", "1.0.0"),
    ("m2", "brew_formulae", "jq", "1.0.0"),
    ("m2", "npm", "typescript", "1.0.0"),
    ("m2", "gem", "old-gem-m2", "1.0.0"),
    ("m2", "bun", "old-bun-m2", "1.0.0"),
    ("m3", "brew_formulae", "jq", "1.0.0"),
    ("m3", "npm", "typescript", "1.0.0"),
    ("m3", "pnpm", "old-pnpm-m3", "1.0.0"),
    ("m3", "uv", "ruff", "1.0.0"),
    ("m4", "brew_formulae", "ripgrep", "1.0.0"),
    ("m4", "npm", "typescript", "1.0.0"),
    ("m4", "npm", "old-npm-m4", "1.0.0"),
    ("h1", "brew_formulae", "jq", "1.0.0"),
    ("h1", "npm", "typescript", "1.0.0"),
    ("h1", "npm", "head-npm-h1", "1.0.0"),
    ("h1", "uv", "ruff", "1.0.0"),
    ("h2", "brew_formulae", "jq", "1.0.0"),
    ("h2", "npm", "typescript", "1.0.0"),
    ("h2", "uv", "head-uv-h2", "1.0.0"),
    ("h2", "bun", "head-bun-h2", "1.0.0"),
    ("h3", "npm", "typescript", "1.0.0"),
    ("h3", "npm", "head-npm-h3", "1.0.0"),
];
const CONFIG: &str = "configs/tether/config.toml.enc";

/// Text a sync prints when it cannot read its config, state or a machine record
fn parse_error() -> Regex {
    Regex::new(
        r"(?i)parse error|failed to parse|TOML parse|invalid type|unknown variant|missing field|expected .* at line|deserializ|Error: ",
    )
    .unwrap()
}

struct Sync {
    round: String,
    phase: String,
    machine: String,
    version: String,
    rc: i64,
    tree_before: String,
    tree_after: String,
    changed: Vec<String>,
    /// Changed files other than this machine's own record
    foreign: Vec<String>,
    parse_errors: Vec<String>,
    text: String,
}

struct Round {
    round: String,
    phase: String,
}

struct Fleet {
    lab: Lab,
    machines: Vec<Machine>,
    start_version: HashMap<String, String>,
    ids: HashMap<String, String>,
    syncs: Vec<Sync>,
    rounds: Vec<Round>,
}

fn is_old(version: &str) -> bool {
    version != HEAD
}

impl Fleet {
    async fn start(test: &str, fleet: &[(&str, &str)], extra: &[&str]) -> Fleet {
        let lab = Lab::new(test).await;
        let mut machines = Vec::new();
        for (name, version) in fleet {
            let mut versions = vec![*version];
            versions.extend(extra.iter().filter(|v| *v != version));
            let m = lab.machine(name, &versions).await;
            for (_, key, pkg, v) in SEEDS.iter().filter(|s| s.0 == *name) {
                m.seed(key, pkg, v).await;
            }
            machines.push(m);
        }
        Fleet {
            lab,
            machines,
            start_version: fleet
                .iter()
                .map(|(m, v)| (m.to_string(), v.to_string()))
                .collect(),
            ids: HashMap::new(),
            syncs: Vec::new(),
            rounds: Vec::new(),
        }
    }

    fn machine(&self, name: &str) -> &Machine {
        self.machines.iter().find(|m| m.name == name).unwrap()
    }

    fn has(&self, name: &str) -> bool {
        self.machines.iter().any(|m| m.name == name)
    }

    async fn run(&mut self, rnd: &str, phase: &str, name: &str, command: &str) -> i64 {
        let lab = &self.lab;
        let tree_before = lab.tree("manifests").await;
        let head_before = lab.head().await;
        let m = self.machines.iter().find(|m| m.name == name).unwrap();
        let out = m
            .sh(&format!(
                "echo {rnd} > /state/round && {command} </dev/null"
            ))
            .await;
        let tree_after = lab.tree("manifests").await;
        let head_after = lab.head().await;
        let changed = lab.changed(&head_before, &head_after).await;
        if !self.ids.contains_key(name) {
            let state = m.read("/root/.tether/state.json").await;
            if let Ok(v) = serde_json::from_str::<Value>(&state) {
                self.ids
                    .insert(name.to_string(), v["machine_id"].as_str().unwrap().into());
            }
        }
        let own = format!("machines/{}.json", self.ids.get(name).map_or("", |s| s));
        let text = out.text();
        let re = parse_error();
        let sync = Sync {
            round: rnd.to_string(),
            phase: phase.to_string(),
            machine: name.to_string(),
            version: m.version(),
            rc: out.code,
            tree_before,
            tree_after,
            foreign: changed
                .iter()
                .filter(|c| **c != own && **c != format!("{own}.sig"))
                .cloned()
                .collect(),
            changed,
            parse_errors: text
                .lines()
                .filter(|l| re.is_match(l))
                .take(5)
                .map(|l| l.trim().to_string())
                .collect(),
            text,
        };
        self.lab.note(&format!(
            "{rnd} {name} [{}] rc={} manifests={} {}",
            sync.version,
            sync.rc,
            &sync.tree_after[..8.min(sync.tree_after.len())],
            sync.foreign.join(" ")
        ));
        let rc = sync.rc;
        self.syncs.push(sync);
        rc
    }

    /// Servers join first and set their profile, so no config.toml change follows once 1.x
    /// machines have joined.
    async fn init(&mut self, servers: &[&str]) {
        let url = self.lab.repo_url();
        let mut order: Vec<String> = servers.iter().map(|s| s.to_string()).collect();
        order.extend(
            self.machines
                .iter()
                .map(|m| m.name.clone())
                .filter(|m| !servers.contains(&m.as_str())),
        );
        for name in order {
            self.run(
                "init",
                "init",
                &name,
                &format!("expect -f {INIT_SCRIPT} tether init --repo {url} --no-daemon"),
            )
            .await;
            if servers.contains(&name.as_str()) {
                for command in [
                    "tether config set profiles.server.dotfiles '[]'",
                    "tether machines profile set server",
                    "tether sync",
                ] {
                    let rc = self.run("init", "profile", &name, command).await;
                    assert_eq!(rc, 0, "{name}: {command} failed");
                }
            }
        }
    }

    /// HEAD machines trust each other once each has published a signed record.
    async fn trust(&self) {
        let heads: Vec<&Machine> = self
            .machines
            .iter()
            .filter(|m| m.version() == HEAD)
            .collect();
        for a in &heads {
            for b in &heads {
                if a.name == b.name {
                    continue;
                }
                let id = b.machine_id().await;
                let fp = b.fingerprint().await;
                a.tether_ok(&format!("machines trust {id} --fingerprint {fp}"))
                    .await;
            }
        }
    }

    async fn round(&mut self, phase: &str) {
        let rnd = format!("R{}", self.rounds.len() + 1);
        let names: Vec<String> = self.machines.iter().map(|m| m.name.clone()).collect();
        for name in names {
            self.run(&rnd, phase, &name, "tether sync").await;
        }
        self.rounds.push(Round {
            round: rnd,
            phase: phase.to_string(),
        });
    }

    async fn event(&mut self, name: &str, phase: &str, command: &str) {
        let rnd = format!("{phase}{}", self.rounds.len() + 1);
        let rc = self.run(&rnd, phase, name, command).await;
        assert_eq!(rc, 0, "{name}: {command} failed, see the logs");
    }

    async fn in_profile(&self, name: &str, profile: &str) -> bool {
        let id = &self.ids[name];
        let config = self.machine(name).read("/root/.tether/config.toml").await;
        let re = Regex::new(&format!(r#"(?m)^"?{}"? = "{profile}"$"#, regex::escape(id))).unwrap();
        re.is_match(&config)
    }

    fn version_at(&self) -> HashMap<(String, String), String> {
        self.syncs
            .iter()
            .map(|s| ((s.round.clone(), s.machine.clone()), s.version.clone()))
            .collect()
    }

    /// (a): every command exits 0 and reads its config and records.
    fn check_a(&self) -> Vec<String> {
        self.syncs
            .iter()
            .filter(|s| s.rc != 0 || !s.parse_errors.is_empty())
            .map(|s| {
                format!(
                    "{} {} [{}] rc={} {}",
                    s.round,
                    s.machine,
                    s.version,
                    s.rc,
                    s.parse_errors.join(" | ")
                )
            })
            .collect()
    }
}

fn seeds(machine: &str) -> impl Iterator<Item = &'static str> + '_ {
    SEEDS.iter().filter(move |s| s.0 == machine).map(|s| s.2)
}

fn installs(calls: &[Value]) -> impl Iterator<Item = (&str, &Value)> {
    calls.iter().flat_map(|c| {
        c["installs"]
            .as_array()
            .into_iter()
            .flatten()
            .map(move |i| (c["r"].as_str().unwrap_or(""), i))
    })
}

fn uninstalls(calls: &[Value]) -> impl Iterator<Item = (&str, &Value)> {
    calls.iter().flat_map(|c| {
        c["uninstalls"]
            .as_array()
            .into_iter()
            .flatten()
            .map(move |i| (c["r"].as_str().unwrap_or(""), i))
    })
}

/// Fails the test with every finding, and writes them to the test's log directory.
fn report(lab: &Lab, results: &BTreeMap<&str, (&str, Option<Vec<String>>)>, extra: &str) {
    let mut text = String::new();
    let mut failed = false;
    for (k, (desc, v)) in results {
        match v {
            None => writeln!(text, "  {k}  N/A   {desc}").unwrap(),
            Some(v) => {
                failed |= !v.is_empty();
                let mark = if v.is_empty() { "pass" } else { "FAIL" };
                writeln!(text, "  {k}  {mark}  {desc}").unwrap();
                for x in v.iter().take(8) {
                    writeln!(text, "         {x}").unwrap();
                }
                if v.len() > 8 {
                    writeln!(text, "         ... {} more", v.len() - 8).unwrap();
                }
            }
        }
    }
    text.push_str(extra);
    std::fs::write(lab.logs.join("summary.txt"), &text).unwrap();
    eprintln!("{text}");
    assert!(!failed, "fleet checks failed, see {}", lab.logs.display());
}

#[tokio::test(flavor = "multi_thread")]
async fn fleet() {
    if !enabled("fleet") {
        return;
    }
    let machines = [
        ("m1", OLD[0]),
        ("m2", OLD[1]),
        ("m3", OLD[2]),
        ("m4", OLD[2]),
        ("h1", HEAD),
        ("h2", HEAD),
        ("h3", HEAD),
    ];
    let upgrade = ("m4", HEAD);
    let mut f = Fleet::start("fleet", &machines, &[HEAD]).await;
    let servers: Vec<&str> = SERVERS.iter().copied().filter(|s| f.has(s)).collect();

    f.init(&servers).await;
    f.round("settle").await;
    f.trust().await;
    for _ in 0..SETTLE {
        f.round("settle").await;
    }
    for _ in 0..STEADY {
        f.round("steady").await;
    }
    let (owner, key, name) = SHARED;
    f.event(
        owner,
        "share",
        &format!("tether packages share {key}:{name} --to server"),
    )
    .await;
    for _ in 0..SETTLE {
        f.round("settle-share").await;
    }
    f.round("steady-share").await;
    for s in &servers {
        f.event(s, "leave", &format!("tether packages remove {key}:{name}"))
            .await;
    }
    for _ in 0..SETTLE {
        f.round("settle-leave").await;
    }
    f.round("steady-leave").await;
    let (rm_machine, rm_key, rm_name) = REMOVE;
    f.machine(rm_machine)
        .ok(&format!("sed -i '/^{rm_name} /d' /state/pkgs/{rm_key}"))
        .await;
    for _ in 0..SETTLE {
        f.round("settle-remove").await;
    }
    for _ in 0..STEADY {
        f.round("steady-remove").await;
    }
    f.machine(upgrade.0).use_version(upgrade.1).await;
    for _ in 0..SETTLE {
        f.round("settle-upgrade").await;
    }
    for _ in 0..STEADY {
        f.round("steady-upgrade").await;
    }
    let mut lost = Vec::new();
    for s in &servers {
        if !f.in_profile(s, "server").await {
            lost.push(s.to_string());
        }
    }
    let mut calls: HashMap<String, Vec<Value>> = HashMap::new();
    for m in &f.machines {
        calls.insert(m.name.clone(), m.calls().await);
    }

    let version_at = f.version_at();
    let order: Vec<&str> = f.rounds.iter().map(|r| r.round.as_str()).collect();
    let pos = |r: &str| order.iter().position(|o| *o == r);
    let mut results: BTreeMap<&str, (&str, Option<Vec<String>>)> = BTreeMap::new();

    let a = f.check_a();

    // b: 1.x rewrites its own record (last_sync) on every sync, so a settled fleet with
    // 1.x machines still commits. Only commits that change anything else count.
    let b: Vec<String> = f
        .syncs
        .iter()
        .filter(|s| s.phase.starts_with("steady") && !s.foreign.is_empty())
        .map(|s| {
            format!(
                "{} {} [{}] committed {}",
                s.round,
                s.machine,
                s.version,
                s.foreign.join(", ")
            )
        })
        .collect();

    // c: reinstalls on 1.x machines, and installs of the removed package after removal
    let removed_from = f
        .syncs
        .iter()
        .find(|s| s.phase == "settle-remove")
        .map(|s| s.round.clone());
    let mut c = Vec::new();
    for (m, entries) in &calls {
        for (rnd, i) in installs(entries) {
            let label = version_at
                .get(&(rnd.to_string(), m.clone()))
                .cloned()
                .or_else(|| (rnd == "init").then(|| f.start_version[m].clone()));
            let Some(label) = label.filter(|l| is_old(l)) else {
                continue;
            };
            let after_removal = match (&removed_from, pos(rnd)) {
                (Some(from), Some(at)) => pos(from).is_some_and(|from| at >= from),
                _ => false,
            };
            let (ikey, iname) = (i["key"].as_str().unwrap(), i["name"].as_str().unwrap());
            if i["already"] == true {
                c.push(format!("{rnd} {m} [{label}] reinstalled {ikey} {iname}"));
            } else if m == rm_machine && ikey == rm_key && iname == rm_name && after_removal {
                c.push(format!(
                    "{rnd} {m} [{label}] reinstalled removed {ikey} {iname}"
                ));
            }
        }
    }

    // d: manifests never return to an earlier tree, and do not change in settled rounds
    let mut d = Vec::new();
    let mut seen: Vec<String> = Vec::new();
    let mut last: Option<String> = None;
    for s in f.syncs.iter().filter(|s| s.round != "init") {
        let t = &s.tree_after;
        if last.as_ref() != Some(t) && seen.contains(t) {
            d.push(format!(
                "{} {} [{}] set manifests back to {t} (seen before)",
                s.round, s.machine, s.version
            ));
        }
        if s.phase.starts_with("steady") && s.tree_before != s.tree_after {
            d.push(format!(
                "{} {} [{}] changed manifests {} -> {}",
                s.round, s.machine, s.version, s.tree_before, s.tree_after
            ));
        }
        if last.as_ref() != Some(t) {
            seen.push(t.clone());
            last = Some(t.clone());
        }
    }

    // e: machines that start on HEAD never install packages only 1.x machines list
    let start_heads: Vec<&str> = machines
        .iter()
        .filter(|(_, v)| *v == HEAD)
        .map(|(m, _)| *m)
        .collect();
    let head_names: HashSet<&str> = start_heads.iter().flat_map(|m| seeds(m)).collect();
    let old_only: HashSet<&str> = machines
        .iter()
        .filter(|(_, v)| is_old(v))
        .flat_map(|(m, _)| seeds(m))
        .filter(|n| !head_names.contains(n))
        .collect();
    let mut e = Vec::new();
    let mut trusted_installs = 0;
    for m in &start_heads {
        for (rnd, i) in installs(&calls[*m]) {
            let name = i["name"].as_str().unwrap();
            if old_only.contains(name) {
                e.push(format!(
                    "{rnd} {m} installed {name} that only 1.x records list"
                ));
            } else if head_names.contains(name) && i["already"] != true {
                trusted_installs += 1;
            }
        }
    }

    // g to j: per-profile package sets. Event rounds count in the order they ran.
    let mut all_order: Vec<&str> = Vec::new();
    for s in &f.syncs {
        if !all_order.contains(&s.round.as_str()) {
            all_order.push(&s.round);
        }
    }
    let at = |r: &str| all_order.iter().position(|o| *o == r);
    let share_at = f
        .syncs
        .iter()
        .find(|s| s.phase == "share")
        .and_then(|s| at(&s.round));
    let leave_at = f
        .syncs
        .iter()
        .find(|s| s.phase == "leave")
        .and_then(|s| at(&s.round));
    let shared = SHARED.2;
    let devs: Vec<&str> = start_heads
        .iter()
        .copied()
        .filter(|m| !servers.contains(m))
        .collect();
    let server_names: HashSet<&str> = servers.iter().flat_map(|m| seeds(m)).collect();
    let dev_names: HashSet<&str> = devs.iter().flat_map(|m| seeds(m)).collect();
    let dev_only: HashSet<&str> = dev_names.difference(&server_names).copied().collect();
    let server_only: HashSet<&str> = server_names.difference(&dev_names).copied().collect();
    let (mut g, mut h, mut i_, mut j) = (Vec::new(), Vec::new(), Vec::new(), Vec::new());
    let mut dev_installs: HashSet<(&str, &str)> = HashSet::new();
    for m in &servers {
        for (rnd, x) in installs(&calls[*m]) {
            let name = x["name"].as_str().unwrap();
            if !dev_only.contains(name) || x["already"] == true {
                continue;
            }
            let r = at(rnd);
            let shared_now =
                share_at.is_some_and(|s| Some(s) < r) && leave_at.is_none_or(|l| r < Some(l));
            if name == shared && shared_now {
                h.push(format!("{rnd} {m}"));
            } else if name == shared && leave_at.is_some_and(|l| r > Some(l)) {
                i_.push(format!(
                    "{rnd} {m} installed {shared} again after leaving it"
                ));
            } else {
                g.push(format!(
                    "{rnd} {m} installed {name} that only dev machines list"
                ));
            }
        }
    }
    for m in &devs {
        for (rnd, x) in uninstalls(&calls[*m]) {
            if x["name"] == shared {
                i_.push(format!("{rnd} {m} uninstalled {shared}"));
            }
        }
        for (rnd, x) in installs(&calls[*m]) {
            let name = x["name"].as_str().unwrap();
            if server_only.contains(name) {
                j.push(format!(
                    "{rnd} {m} installed {name} that only the server lists"
                ));
            } else if dev_names.contains(name) && x["already"] != true {
                dev_installs.insert((m, name));
            }
        }
    }
    let left = servers
        .iter()
        .any(|m| uninstalls(&calls[*m]).any(|(_, x)| x["name"] == shared));
    if leave_at.is_some() && !left {
        i_.push(format!("no server machine uninstalled {shared}"));
    }
    let h_check = if h.is_empty() && share_at.is_some() {
        vec![format!(
            "no server machine installed {shared} after it was shared"
        )]
    } else {
        Vec::new()
    };
    for m in &devs {
        let own: HashSet<&str> = seeds(m).collect();
        let mut missing: Vec<&str> = devs
            .iter()
            .filter(|d| *d != m)
            .flat_map(|d| seeds(d))
            .filter(|n| !own.contains(n) && !dev_installs.contains(&(*m, *n)))
            .collect();
        missing.sort();
        missing.dedup();
        if !missing.is_empty() {
            j.push(format!(
                "{m} never installed {} from its dev peers",
                missing.join(", ")
            ));
        }
    }
    let k: Vec<String> = lost
        .iter()
        .map(|m| {
            // warn_changed_profile in src/cli/commands/sync.rs
            let warned = f.syncs.iter().find(|s| {
                s.machine == *m
                    && s.text
                        .contains("The synced config changed this machine's profile from server")
            });
            match warned {
                Some(s) => format!(
                    "{m} lost its server profile; its sync in round {} warned about it",
                    s.round
                ),
                None => format!("{m} lost its server profile, and no sync of it warned"),
            }
        })
        .collect();

    // f: (a) to (d) in the rounds after the upgrade
    let after: HashSet<&str> = f
        .rounds
        .iter()
        .filter(|r| r.phase.ends_with("-upgrade"))
        .map(|r| r.round.as_str())
        .collect();
    let f_check: Vec<String> = [&a, &b, &c, &d]
        .into_iter()
        .flatten()
        .filter(|x| {
            x.split_whitespace()
                .next()
                .is_some_and(|r| after.contains(r))
        })
        .cloned()
        .collect();

    results.insert(
        "a",
        ("every machine loads config and syncs every round", Some(a)),
    );
    results.insert(
        "b",
        (
            "settled rounds commit nothing beyond each machine's own record",
            Some(b),
        ),
    );
    results.insert(
        "c",
        (
            "1.x machines never reinstall a present or removed package",
            Some(c),
        ),
    );
    results.insert("d", ("manifests never flip", Some(d)));
    results.insert(
        "e",
        (
            "HEAD machines never install packages only 1.x records list",
            Some(e),
        ),
    );
    results.insert(
        "f",
        (
            "(a)-(d) hold after a 1.13.1 machine upgrades to HEAD",
            Some(f_check),
        ),
    );
    results.insert(
        "g",
        (
            "the server installs no dev-only package that is not shared with it",
            Some(g),
        ),
    );
    results.insert(
        "h",
        (
            "the server installs a package once it is shared with its profile",
            Some(h_check),
        ),
    );
    results.insert(
        "i",
        (
            "after the server leaves a shared package, dev machines keep it and the server stops",
            Some(i_),
        ),
    );
    results.insert(
        "j",
        (
            "dev HEAD machines install each other's packages and never the server's",
            Some(j),
        ),
    );
    results.insert(
        "k",
        (
            "the server keeps its profile assignment to the end",
            Some(k),
        ),
    );
    report(
        &f.lab,
        &results,
        &format!("  sanity: HEAD machines installed {trusted_installs} package(s) from trusted HEAD peers\n"),
    );
    assert!(
        trusted_installs > 0,
        "HEAD machines installed nothing from trusted peers"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn config_flap() {
    if !enabled("config_flap") {
        return;
    }
    let flap = flap_ref();
    let machines = [
        ("m1", flap.as_str()),
        ("m2", OLD[0]),
        ("h1", HEAD),
        ("h2", HEAD),
    ];
    let mut f = Fleet::start("config_flap", &machines, &[]).await;
    f.init(&[]).await;
    f.round("settle").await;
    f.trust().await;
    for _ in 0..SETTLE {
        f.round("settle").await;
    }
    f.event("h1", "change", "tether config set dashboard.theme mocha")
        .await;
    for _ in 0..SETTLE + 2 * STEADY {
        f.round("after-change").await;
    }
    // A HEAD config.toml without the fields 1.x requires: every 1.x machine still loads the
    // synced copy (a)
    f.event(
        "h1",
        "change2",
        "sed -i '/sync_versions/d' /root/.tether/config.toml && \
         tether config set packages.min_release_age_days 14",
    )
    .await;
    for _ in 0..SETTLE + STEADY {
        f.round("after-change2").await;
    }
    // The 1.x machine saves its config again, without the marker and the 2.0 keys, and a HEAD
    // machine pulls it next. In a round, the other 1.x machine would push its stale copy over
    // it first, and no 2.0 machine could see the change
    for (machine, command) in [
        ("m1", "tether config set packages.brew.sync_taps false"),
        ("m1", "tether sync"),
        ("h1", "tether sync"),
    ] {
        f.event(machine, "resave", command).await;
    }
    for _ in 0..SETTLE + 2 * STEADY {
        f.round("after-resave").await;
    }

    let after: Vec<&str> = f
        .rounds
        .iter()
        .filter(|r| r.phase == "after-change")
        .map(|r| r.round.as_str())
        .collect();
    // A HEAD machine may export in the round it applies the change. Later, it exports only
    // to restore the change after a 1.x machine exported its stale copy earlier that round
    let l: Vec<String> = f
        .syncs
        .iter()
        .enumerate()
        .filter(|(_, s)| after[1..].contains(&s.round.as_str()) && s.version == HEAD)
        .filter(|(_, s)| s.changed.iter().any(|c| c == CONFIG))
        .filter(|(i, s)| {
            !f.syncs[..*i].iter().any(|o| {
                o.round == s.round && is_old(&o.version) && o.changed.iter().any(|c| c == CONFIG)
            })
        })
        .map(|(_, s)| format!("{} {} committed {CONFIG}", s.round, s.machine))
        .collect();
    let last = after[after.len() - STEADY..].to_vec();
    let steady: Vec<String> = f
        .syncs
        .iter()
        .filter(|s| last.contains(&s.round.as_str()) && s.changed.iter().any(|c| c == CONFIG))
        .map(|s| format!("{} {} committed {CONFIG}", s.round, s.machine))
        .collect();
    // The change survives the stale copies of 1.x machines. 1.x drops the dashboard table
    let mut m = Vec::new();
    for machine in f.machines.iter().filter(|m| m.version() == HEAD) {
        let config: toml::Table =
            toml::from_str(&machine.read("/root/.tether/config.toml").await).unwrap();
        let theme = config
            .get("dashboard")
            .and_then(|d| d.get("theme"))
            .and_then(|t| t.as_str());
        if theme != Some("mocha") {
            m.push(format!("{} has dashboard.theme {:?}", machine.name, theme));
        }
    }
    let mut flaps =
        String::from("\nRound   1.x machines that committed config.toml (reported, not checked)\n");
    for r in &after {
        let ms: Vec<&str> = f
            .syncs
            .iter()
            .filter(|s| {
                s.round == *r && is_old(&s.version) && s.changed.iter().any(|c| c == CONFIG)
            })
            .map(|s| s.machine.as_str())
            .collect();
        writeln!(
            flaps,
            "{r:<7} {}",
            if ms.is_empty() {
                "-".to_string()
            } else {
                ms.join(" ")
            }
        )
        .unwrap();
    }
    let mut results: BTreeMap<&str, (&str, Option<Vec<String>>)> = BTreeMap::new();
    results.insert(
        "a",
        (
            "every machine loads config and syncs every round",
            Some(f.check_a()),
        ),
    );
    results.insert(
        "l",
        (
            "HEAD machines commit config.toml after the change only to restore it",
            Some(l),
        ),
    );
    results.insert(
        "m",
        (
            "the change reaches every machine, and the fleet then stops committing config.toml",
            Some([m, steady].concat()),
        ),
    );
    // No setting is lost: the 1.x save keeps the 2.0 settings and its own change merges
    let mut n = Vec::new();
    for machine in f.machines.iter().filter(|m| m.version() == HEAD) {
        let config: toml::Table =
            toml::from_str(&machine.read("/root/.tether/config.toml").await).unwrap();
        let days = config["packages"].get("min_release_age_days");
        if days.and_then(|d| d.as_integer()) != Some(14) {
            n.push(format!(
                "{} has min_release_age_days {:?}",
                machine.name, days
            ));
        }
        let taps = config["packages"]["brew"].get("sync_taps");
        if taps.and_then(|t| t.as_bool()) != Some(false) {
            n.push(format!("{} has sync_taps {:?}", machine.name, taps));
        }
    }
    let resave: Vec<&str> = f
        .rounds
        .iter()
        .filter(|r| r.phase == "after-resave")
        .map(|r| r.round.as_str())
        .collect();
    let last = &resave[resave.len() - STEADY..];
    let o: Vec<String> = f
        .syncs
        .iter()
        .filter(|s| last.contains(&s.round.as_str()) && s.changed.iter().any(|c| c == CONFIG))
        .map(|s| format!("{} {} committed {CONFIG}", s.round, s.machine))
        .collect();
    results.insert(
        "n",
        (
            "after a 1.x stale copy and a 1.x save, HEAD machines keep every setting",
            Some(n),
        ),
    );
    results.insert(
        "o",
        (
            "after the 1.x save, the fleet stops committing config.toml",
            Some(o),
        ),
    );
    report(&f.lab, &results, &flaps);
}

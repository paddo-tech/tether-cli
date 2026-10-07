#!/usr/bin/env python3
"""Mixed-version fleet test.

Builds Tether at several releases and at HEAD from `git archive`, runs one Docker
container per machine against a bare repo served by git-daemon on an internal Docker
network, and syncs every machine in a fixed order for several rounds. Package managers
are shims (tests/fleet/shims/pm), so nothing installs and nothing reaches the network.

Assertions:
  a  every init and sync exits 0 and prints no config or parse error
  b  after the fleet settles, a round adds no commits to the remote other than a 1.x
     machine rewriting its own record, which 1.x does on every sync
  c  no 1.x machine installs a package it already has, or one it removed
  d  the manifests never return to an earlier state, and do not change once settled
  e  machines that start on HEAD never install a package that only 1.x machines list
  f  (a) to (d) still hold in the rounds after one 1.13.1 machine upgrades to HEAD
  g  the HEAD machine in profile "server" installs no package that only dev machines list,
     except SHARED while it is shared with the server profile
  h  after `packages share SHARED --to server` the server machine installs it
  i  after the server machine runs `packages remove SHARED`, it does not install it again,
     and no dev machine uninstalls it
  j  dev HEAD machines install each other's packages, and never the server's own packages
  k  the server keeps its profile assignment to the end. The server sets its profile before
     the other machines join: in a mixed fleet, a config.toml change made later flaps (see
     --scenario config-flap)

Scenario config-flap (--scenario config-flap): after the fleet settles, one HEAD machine
changes config.toml once. Checks (a), and
  l  HEAD machines stop committing config.toml one round after they applied the change
It also reports how often 1.x machines commit config.toml per round, without failing on it.
A version is a git tag, 'head', or 'ref:<git ref>' for a build of any commit, such as a 1.x
patch: --fleet m1=ref:<sha>,m2=v1.12.0,h1=head --scenario config-flap

Usage: tests/fleet/run.py [--fleet m1=v1.11.10,...] [--upgrade m4=head] [--steady N]
                          [--scenario default|config-flap]
Logs and a summary land in --out (default target/fleet).
"""
import argparse
import concurrent.futures
import json
import os
import re
import subprocess
import sys
import time
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
HERE = Path(__file__).resolve().parent
P = "tether-fleet"
RUST_IMAGE = "rust:1-bookworm"
REPO_URL = "git://repo/fleet.git"
REMOVE = ("m1", "npm", "cowsay")
SETTLE = 2
DEFAULT_FLEET = "m1=v1.11.10,m2=v1.12.0,m3=v1.13.1,m4=v1.13.1,h1=head,h2=head,h3=head"
# HEAD machines in profile "server"; every other machine stays in "dev"
SERVERS = ("h3",)
# A package only dev machines list, which h1 shares with the server profile and the server
# then removes again
SHARED = ("h1", "npm", "head-npm-h1")
# Text a sync prints when it cannot read its config, state or a machine record
PARSE_ERROR = re.compile(
    r"parse error|failed to parse|TOML parse|invalid type|unknown variant|missing field|"
    r"expected .* at line|deserializ|Error: ",
    re.IGNORECASE,
)
ANSI = re.compile(r"\x1b\[[0-9;?]*[a-zA-Z]")


def sh(*args, check=True, input=None, capture=True):
    r = subprocess.run(args, input=input, capture_output=capture, text=input is None or isinstance(input, str))
    if check and r.returncode != 0:
        raise SystemExit(f"command failed ({r.returncode}): {' '.join(args)}\n{r.stdout}\n{r.stderr}")
    return r


def git(*args):
    return sh("git", "-C", str(ROOT), *args).stdout.strip()


def dexec(machine, script, check=False):
    r = subprocess.run(["docker", "exec", f"{P}-{machine}", "sh", "-c", script], capture_output=True, text=True)
    if check and r.returncode != 0:
        raise SystemExit(f"{machine}: {script}\n{r.stdout}{r.stderr}")
    return r


def remote(script):
    return dexec("repo", f"cd /srv/fleet.git && {script}").stdout.strip()


class Fleet:
    def __init__(self, opts):
        self.out = Path(opts.out).resolve()
        self.cache = Path(opts.cache).resolve()
        self.steady = opts.steady
        self.head_sha = git("rev-parse", opts.head_ref)
        self.machines = dict(kv.split("=") for kv in opts.fleet.split(","))
        self.upgrade = dict(kv.split("=") for kv in opts.upgrade.split(",") if kv)
        self.version = dict(self.machines)
        self.start_version = dict(self.machines)
        self.syncs = []
        self.rounds = []
        self.events = []
        self.ids = {}
        # Servers that lost their profile assignment by the end
        self.lost = []

    def ref(self, label):
        if label == "head":
            return self.head_sha
        return label.removeprefix("ref:")

    def binary(self, label):
        key = f"head-{self.head_sha[:12]}" if label == "head" else slug(label)
        return self.cache / f"tether-{key}"

    def build(self, label):
        bin_path = self.binary(label)
        if bin_path.exists():
            return f"{label}: cached {bin_path.name}"
        ref = self.ref(label)
        has_lock = subprocess.run(["git", "-C", str(ROOT), "cat-file", "-e", f"{ref}:Cargo.lock"],
                                  capture_output=True).returncode == 0
        name = f"{P}-build-{slug(label)}"
        sh("docker", "rm", "-f", name, check=False)
        archive = subprocess.run(["git", "-C", str(ROOT), "archive", "--format=tar", ref], capture_output=True, check=True).stdout
        # One registry per build: parallel builds that share one race to unpack crates
        volumes = ["-v", f"{P}-cargo-{slug(label)}:/usr/local/cargo/registry"]
        if label == "head":
            volumes += ["-v", f"{P}-target-head:/src/target"]
        log = self.out / f"build-{slug(label)}.log"
        r = subprocess.run(
            ["docker", "run", "-i", "--name", name, *volumes, RUST_IMAGE, "sh", "-c",
             f"mkdir -p /src && tar x -C /src && cd /src && cargo build --release {'--locked' if has_lock else ''}"],
            input=archive, stdout=open(log, "wb"), stderr=subprocess.STDOUT,
        )
        if r.returncode != 0:
            raise SystemExit(f"build of {label} failed, see {log}")
        self.cache.mkdir(parents=True, exist_ok=True)
        sh("docker", "cp", f"{name}:/src/target/release/tether", str(bin_path))
        sh("docker", "rm", name)
        return f"{label}: built {bin_path.name}"

    def teardown(self):
        names = sh("docker", "ps", "-aq", "--filter", f"name=^{P}-", check=False).stdout.split()
        if names:
            sh("docker", "rm", "-f", *names, check=False)
        sh("docker", "network", "rm", f"{P}-net", check=False)

    def start(self):
        self.teardown()
        sh("docker", "build", "-q", "-t", f"{P}-img", str(HERE))
        sh("docker", "network", "create", "--internal", f"{P}-net")
        sh("docker", "run", "-d", "--name", f"{P}-repo", "--network", f"{P}-net", "--network-alias", "repo",
           f"{P}-img", "sh", "-c",
           "git init -q --bare -b main /srv/fleet.git && exec git daemon --base-path=/srv --export-all "
           "--enable=receive-pack --reuseaddr /srv")
        labels = set(self.machines.values()) | set(self.upgrade.values())
        for m, label in self.machines.items():
            sh("docker", "run", "-d", "--name", f"{P}-{m}", "--hostname", m, "--network", f"{P}-net",
               f"{P}-img", "sleep", "infinity")
            for lb in labels:
                sh("docker", "cp", str(self.binary(lb)), f"{P}-{m}:/usr/local/bin/tether-{slug(lb)}")
            dexec(m, f"ln -sf tether-{slug(label)} /usr/local/bin/tether && mkdir -p /state/pkgs && echo init > /state/round && "
                     f"awk '$1==\"{m}\" {{print $3, $4 >> \"/state/pkgs/\" $2}}' /fleet/fixtures.txt", check=True)

    def set_version(self, m, label):
        dexec(m, f"ln -sf tether-{slug(label)} /usr/local/bin/tether", check=True)
        self.version[m] = label

    def run_tether(self, rnd, phase, m, command):
        commits_before = int(remote("git rev-list --count main 2>/dev/null || echo 0") or 0)
        tree_before = remote("git rev-parse main:manifests 2>/dev/null || echo none")
        head_before = remote("git rev-parse -q --verify main || echo none")
        t0 = time.time()
        r = dexec(m, f"echo {rnd} > /state/round && {command} </dev/null")
        took = time.time() - t0
        commits_after = int(remote("git rev-list --count main 2>/dev/null || echo 0") or 0)
        tree_after = remote("git rev-parse main:manifests 2>/dev/null || echo none")
        head_after = remote("git rev-parse -q --verify main || echo none")
        changed = []
        if head_before not in ("none", head_after):
            changed = remote(f"git diff --name-only {head_before} {head_after}").split()
        if m not in self.ids:
            state = dexec(m, "cat /root/.tether/state.json").stdout
            if state:
                self.ids[m] = json.loads(state)["machine_id"]
        own = f"machines/{self.ids.get(m)}.json"
        text = ANSI.sub("", r.stdout + r.stderr)
        log = self.out / "logs" / f"{rnd}-{m}.log"
        log.write_text(f"$ {command}\n[{self.version[m]}] rc={r.returncode}\n{text}")
        record = {
            "round": rnd, "phase": phase, "machine": m, "version": self.version[m], "rc": r.returncode,
            "seconds": round(took, 1), "commits": commits_after - commits_before,
            "tree_before": tree_before, "tree_after": tree_after, "changed": changed,
            # A sync that changes only this machine's own record is a heartbeat
            "foreign": [c for c in changed if c not in (own, own + ".sig")],
            "parse_errors": [ln.strip() for ln in text.splitlines() if PARSE_ERROR.search(ln)][:5],
            "log": str(log.relative_to(self.out)),
        }
        self.syncs.append(record)
        mark = "ok" if r.returncode == 0 and not record["parse_errors"] else "FAIL"
        print(f"  {rnd:>4} {m} [{self.version[m]}] rc={r.returncode} +{record['commits']} commits "
              f"manifests={tree_after[:8]} {took:.1f}s {mark} {' '.join(record['foreign'])}", flush=True)
        return record

    def init(self, servers):
        # Servers join first and set their profile, so no config.toml change follows once 1.x
        # machines have joined
        for m in servers + [m for m in self.machines if m not in servers]:
            self.run_tether("init", "init", m, f"expect -f /fleet/init.exp tether init --repo {REPO_URL} --no-daemon")
            if m in servers:
                for command in ("tether config set profiles.server.dotfiles '[]'",
                                "tether machines profile set server", "tether sync"):
                    if self.run_tether("init", "profile", m, command)["rc"] != 0:
                        raise SystemExit(f"{m}: {command} failed")

    def trust(self):
        heads = [m for m, v in self.version.items() if v == "head"]
        for a in heads:
            for b in heads:
                if a == b:
                    continue
                mid = json.loads(dexec(b, "cat /root/.tether/state.json", check=True).stdout)["machine_id"]
                fp = dexec(b, "ssh-keygen -lf /root/.tether/signing_key", check=True).stdout.split()[1]
                r = dexec(a, f"tether machines trust {mid} {fp} </dev/null")
                self.events.append(f"trust {a} -> {b} ({mid} {fp}) rc={r.returncode}")
                print(f"  trust {a} -> {b} {fp} rc={r.returncode} {ANSI.sub('', r.stdout + r.stderr).strip()}")
                if r.returncode != 0:
                    raise SystemExit("trust failed: harness cannot set up HEAD peers")

    def round(self, phase):
        rnd = f"R{len(self.rounds) + 1}"
        commits_before = int(remote("git rev-list --count main") or 0)
        for m in self.machines:
            self.run_tether(rnd, phase, m, "tether sync")
        tree = remote("git rev-parse main:manifests")
        snap = self.out / "manifests" / rnd
        snap.mkdir(parents=True, exist_ok=True)
        for name in remote("git ls-tree --name-only main:manifests").split():
            (snap / name).write_text(remote(f"git show main:manifests/{name}"))
        self.rounds.append({
            "round": rnd, "phase": phase, "tree": tree,
            "commits": int(remote("git rev-list --count main")) - commits_before,
            "versions": dict(self.version),
        })

    def phases(self):
        # HEAD peers trust each other once both have published a signed record
        self.round("settle")
        self.trust()
        for _ in range(SETTLE):
            self.round("settle")
        for _ in range(self.steady):
            self.round("steady")
        if any(m in self.machines for m in SERVERS):
            m, key, name = SHARED
            self.event(m, "share", f"tether packages share {key}:{name} --to server")
            for _ in range(SETTLE):
                self.round("settle-share")
            self.round("steady-share")
            for m in SERVERS:
                self.event(m, "leave", f"tether packages remove {key}:{name}")
            for _ in range(SETTLE):
                self.round("settle-leave")
            self.round("steady-leave")
        m, key, name = REMOVE
        dexec(m, f"sed -i '/^{name} /d' /state/pkgs/{key}", check=True)
        self.events.append(f"removed {key} {name} on {m} before R{len(self.rounds) + 1}")
        print(f"  removed {key} {name} on {m}")
        for _ in range(SETTLE):
            self.round("settle-remove")
        for _ in range(self.steady):
            self.round("steady-remove")
        if self.upgrade:
            for m, label in self.upgrade.items():
                self.events.append(f"upgraded {m} {self.version[m]} -> {label} before R{len(self.rounds) + 1}")
                print(f"  upgrade {m} {self.version[m]} -> {label}")
                self.set_version(m, label)
            for _ in range(SETTLE):
                self.round("settle-upgrade")
            for _ in range(self.steady):
                self.round("steady-upgrade")
        self.lost = [m for m in SERVERS if m in self.machines and not self.in_profile(m, "server")]

    def flap(self):
        """One config.toml change on a HEAD machine after the fleet settled."""
        self.round("settle")
        self.trust()
        for _ in range(SETTLE):
            self.round("settle")
        m = next(m for m, v in self.version.items() if v == "head" and m not in SERVERS)
        self.event(m, "change", "tether config set dashboard.theme mocha")
        for _ in range(SETTLE + 2 * self.steady):
            self.round("after-change")

    def in_profile(self, m, profile):
        mid = self.ids.get(m)
        text = dexec(m, "cat /root/.tether/config.toml").stdout
        return re.search(rf'^"?{re.escape(mid)}"? = "{profile}"$', text, re.MULTILINE) is not None

    def event(self, m, phase, command):
        rnd = f"{phase}{len(self.rounds) + 1}"
        self.events.append(f"{m}: {command} before R{len(self.rounds) + 1}")
        print(f"  {m}: {command}")
        r = self.run_tether(rnd, phase, m, command)
        if r["rc"] != 0:
            raise SystemExit(f"{m}: {command} failed, see {r['log']}")

    def collect(self):
        calls = {}
        for m in self.machines:
            d = self.out / "machines" / m
            d.mkdir(parents=True, exist_ok=True)
            for path in ("/state", "/root/.tether"):
                sh("docker", "cp", f"{P}-{m}:{path}", str(d), check=False)
            calls[m] = [json.loads(ln) for ln in (d / "state" / "calls.jsonl").read_text().splitlines() if ln]
        sh("docker", "cp", f"{P}-repo:/srv/fleet.git", str(self.out / "remote.git"), check=False)
        (self.out / "remote-log.txt").write_text(remote("git log --format='%h %an %s' --stat main"))
        return calls


def seeded():
    seeds = {}
    for line in (HERE / "fixtures.txt").read_text().splitlines():
        parts = line.split()
        if parts and not line.startswith("#"):
            seeds.setdefault(parts[0], set()).add((parts[1], parts[2]))
    return seeds


def slug(label):
    """A label as a file and container name: 'ref:abc/def' becomes 'ref-abc-def'."""
    return re.sub(r"[^A-Za-z0-9._-]", "-", label)


def is_old(label):
    return label != "head"


def check(fleet, calls):
    results = {}
    seeds = seeded()
    version_at = {(s["round"], s["machine"]): s["version"] for s in fleet.syncs}
    rounds_after = [r["round"] for r in fleet.rounds if r["phase"].endswith("-upgrade")]

    # a: every command exits 0 and reads its config and records
    a = [f"{s['round']} {s['machine']} [{s['version']}] rc={s['rc']} {' | '.join(s['parse_errors'])} ({s['log']})"
         for s in fleet.syncs if s["rc"] != 0 or s["parse_errors"]]
    results["a"] = a

    # b: no commits in settled rounds
    # 1.x rewrites its own record (last_sync) on every sync, so a settled fleet with 1.x
    # machines still commits. Only commits that change anything else count.
    b = []
    for s in fleet.syncs:
        if s["phase"].startswith("steady") and s["foreign"]:
            b.append(f"{s['round']} {s['machine']} [{s['version']}] committed {', '.join(s['foreign'])}")
    results["b"] = b

    # c: reinstalls on 1.x machines, and installs of the removed package after removal
    rm_machine, rm_key, rm_name = REMOVE
    removed_from = next((s["round"] for s in fleet.syncs if s["phase"] == "settle-remove"), None)
    order = [r["round"] for r in fleet.rounds]
    c, per_round = [], {}
    for m, entries in calls.items():
        for e in entries:
            rnd = e["r"]
            label = version_at.get((rnd, m), fleet.start_version[m] if rnd == "init" else None)
            for i in e.get("installs", []):
                if label is None or not is_old(label):
                    continue
                after_removal = (removed_from and rnd in order and order.index(rnd) >= order.index(removed_from))
                if i["already"]:
                    per_round[rnd] = per_round.get(rnd, 0) + 1
                    c.append(f"{rnd} {m} [{label}] reinstalled {i['key']} {i['name']} via `{e['tool']} {' '.join(e['argv'])}`")
                elif m == rm_machine and i["key"] == rm_key and i["name"] == rm_name and after_removal:
                    per_round[rnd] = per_round.get(rnd, 0) + 1
                    c.append(f"{rnd} {m} [{label}] reinstalled removed {rm_key} {rm_name} via `{e['tool']} {' '.join(e['argv'])}`")
    results["c"] = c
    results["c_per_round"] = per_round

    # d: manifests never return to an earlier tree, and do not change in settled rounds
    d, seen, last = [], [], None
    for s in fleet.syncs:
        if s["round"] == "init":
            continue
        t = s["tree_after"]
        if t != last and t in seen:
            d.append(f"{s['round']} {s['machine']} [{s['version']}] set manifests back to {t[:8]} (seen before)")
        if s["phase"].startswith("steady") and s["tree_before"] != s["tree_after"]:
            d.append(f"{s['round']} {s['machine']} [{s['version']}] changed manifests {s['tree_before'][:8]} -> {s['tree_after'][:8]}")
        if t != last:
            seen.append(t)
            last = t
    results["d"] = d

    # e: machines that start on HEAD never install packages only 1.x machines list
    start_heads = [m for m, v in fleet.start_version.items() if v == "head"]
    head_names = {n for m in start_heads for _, n in seeds.get(m, ())}
    old_only = {n for m, v in fleet.start_version.items() if is_old(v) for _, n in seeds.get(m, ())} - head_names
    e, trusted_installs = [], []
    for m in start_heads:
        for entry in calls.get(m, []):
            for i in entry.get("installs", []):
                if i["name"] in old_only:
                    e.append(f"{entry['r']} {m} installed {i['key']} {i['name']} that only 1.x records list")
                elif i["name"] in head_names and not i["already"]:
                    trusted_installs.append(f"{entry['r']} {m} {i['key']} {i['name']}")
    results["e"] = e if start_heads else None
    results["e_trusted_installs"] = trusted_installs

    # g to j: per-profile package sets
    servers = [m for m in SERVERS if m in fleet.machines]
    if servers:
        g_to_j(fleet, calls, seeds, results, servers)
    else:
        results.update(g=None, h=None, i=None, j=None, k=None)

    # f: a to d in the rounds after the upgrade
    if rounds_after:
        f = [x for k in "abcd" for x in results[k] if x.split()[0] in rounds_after]
        results["f"] = f
    else:
        results["f"] = None
    return results


def g_to_j(fleet, calls, seeds, results, servers):
    order = []
    for s in fleet.syncs:
        if s["round"] not in order:
            order.append(s["round"])

    def at(rnd):
        return order.index(rnd) if rnd in order else -1

    share_at = next((at(s["round"]) for s in fleet.syncs if s["phase"] == "share"), None)
    leave_at = next((at(s["round"]) for s in fleet.syncs if s["phase"] == "leave"), None)
    _, _, shared = SHARED
    devs = [m for m, v in fleet.start_version.items() if v == "head" and m not in servers]
    server_names = {n for m in servers for _, n in seeds.get(m, ())}
    dev_names = {n for m in devs for _, n in seeds.get(m, ())}
    dev_only = dev_names - server_names
    server_only = server_names - dev_names

    g, h, i, j, dev_installs = [], [], [], [], []
    for m in servers:
        for e in calls.get(m, []):
            for x in e.get("installs", []):
                if x["name"] not in dev_only or x["already"]:
                    continue
                r = at(e["r"])
                if x["name"] == shared and share_at is not None and share_at < r and (leave_at is None or r < leave_at):
                    h.append(f"{e['r']} {m}")
                elif x["name"] == shared and leave_at is not None and r > leave_at:
                    i.append(f"{e['r']} {m} installed {shared} again after leaving it")
                else:
                    g.append(f"{e['r']} {m} installed {x['key']} {x['name']} that only dev machines list")
    for m in devs:
        for e in calls.get(m, []):
            for x in e.get("uninstalls", []):
                if x["name"] == shared:
                    i.append(f"{e['r']} {m} uninstalled {shared}")
            for x in e.get("installs", []):
                if x["name"] in server_only:
                    j.append(f"{e['r']} {m} installed {x['key']} {x['name']} that only the server lists")
                elif x["name"] in dev_names and not x["already"]:
                    dev_installs.append((m, x["name"]))
    left = [x for m in servers for e in calls.get(m, []) for x in e.get("uninstalls", []) if x["name"] == shared]
    if leave_at is not None and not left:
        i.append(f"no server machine uninstalled {shared}")
    results["g"] = g
    results["h"] = [] if h or share_at is None else [f"no server machine installed {shared} after it was shared"]
    results["h_installs"] = h
    results["i"] = i
    for m in devs:
        others = {n for d in devs if d != m for _, n in seeds.get(d, ())} - {n for _, n in seeds.get(m, ())}
        missing = others - {n for d, n in dev_installs if d == m}
        if missing:
            j.append(f"{m} never installed {', '.join(sorted(missing))} from its dev peers")
    results["j"] = j
    k = []
    for m in fleet.lost:
        warned = any("no longer puts this machine in profile server" in (fleet.out / s["log"]).read_text()
                     for s in fleet.syncs if s["machine"] == m)
        k.append(f"{m} lost its server profile ({'with' if warned else 'without'} the warning)")
    results["k"] = k


def check_flap(fleet):
    """(a), and whether HEAD machines settle after one config.toml change."""
    results = {"a": [f"{s['round']} {s['machine']} [{s['version']}] rc={s['rc']} {' | '.join(s['parse_errors'])} ({s['log']})"
                     for s in fleet.syncs if s["rc"] != 0 or s["parse_errors"]]}
    after = [r["round"] for r in fleet.rounds if r["phase"] == "after-change"]
    config = "configs/tether/config.toml.enc"
    # A HEAD machine may export once, in the round it applies the change; later rounds must be quiet
    results["l"] = [f"{s['round']} {s['machine']} committed {config}" for s in fleet.syncs
                    if s["round"] in after[1:] and s["version"] == "head" and config in s["changed"]]
    results["flap_1x"] = {r: [s["machine"] for s in fleet.syncs
                              if s["round"] == r and is_old(s["version"]) and config in s["changed"]] for r in after}
    return results


def report_flap(fleet, results):
    lines = ["", "Round   1.x machines that committed config.toml"]
    for r, ms in results["flap_1x"].items():
        lines.append(f"{r:<7} {' '.join(ms) or '-'}")
    lines.append("")
    failed = False
    for k, desc in (("a", "every machine loads config and syncs every round"),
                    ("l", "HEAD machines stop committing config.toml after they applied the change")):
        v = results[k]
        failed |= bool(v)
        lines.append(f"  {k}  {'FAIL' if v else 'pass'}  {desc}" + (f" ({len(v)} findings)" if v else ""))
        for x in v[:8]:
            lines.append(f"         {x}")
    last = list(results["flap_1x"].values())[-2:]
    lines.append(f"  1.x config flapping in the last two rounds: {'yes' if any(last) else 'no'} (reported, not checked)")
    lines.append(f"\nLogs: {fleet.out}")
    text = "\n".join(lines)
    print(text)
    (fleet.out / "summary.txt").write_text(text + "\n")
    (fleet.out / "summary.json").write_text(json.dumps(
        {"fleet": fleet.start_version, "head": fleet.head_sha, "events": fleet.events,
         "rounds": fleet.rounds, "syncs": fleet.syncs, "results": results}, indent=1))
    return failed


def report(fleet, results):
    lines = ["", "Round   Phase           Commits  Other  Manifests  1.x reinstalls  Versions"]
    for r in fleet.rounds:
        vs = " ".join(f"{m}={v}" for m, v in r["versions"].items())
        other = sum(1 for s in fleet.syncs if s["round"] == r["round"] and s["foreign"])
        lines.append(f"{r['round']:<7} {r['phase']:<15} {r['commits']:>7}  {other:>5}  {r['tree'][:8]:<9}  "
                     f"{results['c_per_round'].get(r['round'], 0):>14}  {vs}")
    lines.append("")
    names = {
        "a": "every machine loads config and syncs every round",
        "b": "settled rounds commit nothing beyond each machine's own record",
        "c": "1.x machines never reinstall a present or removed package",
        "d": "manifests never flip",
        "e": "HEAD machines never install packages only 1.x records list",
        "f": "(a)-(d) hold after a 1.13.1 machine upgrades to HEAD",
        "g": "the server machine installs no dev-only package that is not shared with it",
        "h": "the server machine installs a package once it is shared with its profile",
        "i": "after the server leaves a shared package, dev machines keep it and the server stops",
        "j": "dev HEAD machines install each other's packages and never the server's",
        "k": "the server keeps its profile assignment to the end",
    }
    failed = False
    for k, desc in names.items():
        v = results[k]
        if v is None:
            lines.append(f"  {k}  N/A   {desc}")
            continue
        failed |= bool(v)
        lines.append(f"  {k}  {'FAIL' if v else 'pass'}  {desc}" + (f" ({len(v)} findings)" if v else ""))
        for x in v[:8]:
            lines.append(f"         {x}")
        if len(v) > 8:
            lines.append(f"         ... {len(v) - 8} more in summary.json")
    if results["e"] is not None:
        lines.append(f"  sanity: HEAD machines installed {len(results['e_trusted_installs'])} package(s) from trusted HEAD peers")
    lines.append(f"\nLogs: {fleet.out}")
    text = "\n".join(lines)
    print(text)
    (fleet.out / "summary.txt").write_text(text + "\n")
    (fleet.out / "summary.json").write_text(json.dumps(
        {"fleet": fleet.start_version, "upgrade": fleet.upgrade, "head": fleet.head_sha, "events": fleet.events,
         "rounds": fleet.rounds, "syncs": fleet.syncs, "results": results}, indent=1))
    return failed


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--fleet", default=os.environ.get("FLEET", DEFAULT_FLEET),
                    help="machine=version pairs; version is a git tag or 'head'")
    ap.add_argument("--upgrade", default=os.environ.get("FLEET_UPGRADE", "m4=head"),
                    help="machine=version pairs to switch after the removal phase; empty for none")
    ap.add_argument("--steady", type=int, default=2, help="settled rounds per phase")
    ap.add_argument("--head-ref", default="HEAD")
    ap.add_argument("--out", default=str(ROOT / "target" / "fleet"))
    ap.add_argument("--cache", default=str(ROOT / "target" / "fleet-bin"))
    ap.add_argument("--keep", action="store_true", help="leave the containers running")
    ap.add_argument("--scenario", choices=("default", "config-flap"), default="default")
    opts = ap.parse_args()

    fleet = Fleet(opts)
    sh("rm", "-rf", str(fleet.out))
    (fleet.out / "logs").mkdir(parents=True)
    t0 = time.time()
    if opts.scenario == "config-flap":
        fleet.upgrade = {}
    labels = sorted(set(fleet.machines.values()) | set(fleet.upgrade.values()))
    print(f"Building {', '.join(labels)} (HEAD {fleet.head_sha[:12]})", flush=True)
    with concurrent.futures.ThreadPoolExecutor() as pool:
        for msg in pool.map(fleet.build, labels):
            print(f"  {msg}", flush=True)
    print(f"Starting fleet: {', '.join(f'{m}={v}' for m, v in fleet.machines.items())}", flush=True)
    try:
        fleet.start()
        if opts.scenario == "config-flap":
            fleet.init([])
            fleet.flap()
        else:
            fleet.init([m for m in SERVERS if m in fleet.machines])
            fleet.phases()
            calls = fleet.collect()
    finally:
        if not opts.keep:
            fleet.teardown()
    if opts.scenario == "config-flap":
        failed = report_flap(fleet, check_flap(fleet))
    else:
        failed = report(fleet, check(fleet, calls))
    print(f"Took {time.time() - t0:.0f}s")
    sys.exit(1 if failed else 0)


if __name__ == "__main__":
    main()

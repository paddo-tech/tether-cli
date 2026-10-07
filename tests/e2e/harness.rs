//! Containers for the e2e tests. Each test gets its own Docker network, a git server and its
//! machines, named after the test and this process so parallel tests and runs never meet.
//! Every command a test runs is logged to target/e2e/logs/<test>/<machine>.log.

use std::io::Write;
use std::path::PathBuf;
use std::process::Command;
use std::sync::OnceLock;
use std::time::Duration;
use testcontainers::{
    core::{ExecCommand, WaitFor},
    runners::AsyncRunner,
    ContainerAsync, CopyTargetOptions, GenericImage, ImageExt,
};

/// The 1.x releases a mixed fleet runs, built from their tags.
pub const OLD: [&str; 3] = ["v1.11.10", "v1.12.0", "v1.13.1"];
pub const HEAD: &str = "head";
pub const INIT_SCRIPT: &str = "/e2e/init.exp";

pub fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

/// The git ref the config-flap test runs as its 1.x machine: any commit, such as a 1.x patch.
pub fn flap_ref() -> String {
    std::env::var("TETHER_E2E_FLAP_REF").unwrap_or_else(|_| "v1.13.1".to_string())
}

/// The binary label images.sh gives a ref.
pub fn label(git_ref: &str) -> String {
    let tag = git_ref.starts_with('v') && git_ref[1..].starts_with(|c: char| c.is_ascii_digit());
    if git_ref == HEAD || tag {
        return git_ref.to_string();
    }
    let out = Command::new("git")
        .args(["rev-parse", "--short=12", &format!("{git_ref}^{{commit}}")])
        .current_dir(root())
        .output()
        .expect("git rev-parse");
    assert!(out.status.success(), "unknown git ref {git_ref}");
    format!("ref-{}", String::from_utf8_lossy(&out.stdout).trim())
}

/// Whether the e2e suite runs. It needs TETHER_E2E=1 and Docker; without either the test
/// prints why and passes. The first test to get here builds binaries and images once.
pub fn enabled(test: &str) -> bool {
    if std::env::var("TETHER_E2E").as_deref() != Ok("1") {
        eprintln!("{test}: skipped, set TETHER_E2E=1 to run the e2e suite (see AGENTS.md)");
        return false;
    }
    static DOCKER: OnceLock<Result<(), String>> = OnceLock::new();
    let docker = DOCKER.get_or_init(|| {
        let out = Command::new("docker")
            .args(["info", "--format", "{{.ServerVersion}}"])
            .output();
        match out {
            Ok(o) if o.status.success() => Ok(()),
            Ok(o) => Err(String::from_utf8_lossy(&o.stderr).trim().to_string()),
            Err(e) => Err(e.to_string()),
        }
    });
    if let Err(why) = docker {
        eprintln!("{test}: skipped, Docker is not available: {why}");
        return false;
    }
    static BUILT: OnceLock<()> = OnceLock::new();
    BUILT.get_or_init(|| {
        let mut refs: Vec<String> = OLD.iter().map(|s| s.to_string()).collect();
        refs.push(flap_ref());
        let status = Command::new(root().join("tests/e2e/images.sh"))
            .args(&refs)
            .current_dir(root())
            .status()
            .expect("run tests/e2e/images.sh");
        assert!(status.success(), "tests/e2e/images.sh failed");
    });
    true
}

pub struct Out {
    pub code: i64,
    pub stdout: String,
    pub stderr: String,
}

impl Out {
    /// stdout and stderr, without colour codes.
    pub fn text(&self) -> String {
        let ansi = regex::Regex::new(r"\x1b\[[0-9;?]*[a-zA-Z]").unwrap();
        ansi.replace_all(&format!("{}{}", self.stdout, self.stderr), "")
            .into_owned()
    }
}

/// A test's network, git server and log directory.
pub struct Lab {
    prefix: String,
    pub logs: PathBuf,
    server: ContainerAsync<GenericImage>,
}

impl Lab {
    pub async fn new(test: &str) -> Lab {
        let prefix = format!(
            "tether-e2e-{}-{}",
            test.replace('_', "-"),
            std::process::id()
        );
        let logs = root().join("target/e2e/logs").join(test);
        let _ = std::fs::remove_dir_all(&logs);
        std::fs::create_dir_all(&logs).unwrap();
        let server = GenericImage::new("tether-e2e-machine", "latest")
            .with_wait_for(WaitFor::message_on_stderr("Ready to rumble"))
            .with_network(format!("{prefix}-net"))
            .with_container_name(format!("{prefix}-git"))
            .with_hostname("git")
            .with_cmd([
                "sh",
                "-c",
                "git init -q --bare -b main /srv/e2e.git && exec git daemon --verbose \
                 --base-path=/srv --export-all --enable=receive-pack --reuseaddr /srv",
            ])
            .start()
            .await
            .expect("start git server");
        Lab {
            prefix,
            logs,
            server,
        }
    }

    pub fn repo_url(&self) -> String {
        format!("git://{}-git/e2e.git", self.prefix)
    }

    /// A machine with shimmed package managers and every binary in `versions`; `tether`
    /// runs the first.
    pub async fn machine(&self, name: &str, versions: &[&str]) -> Machine {
        self.start("tether-e2e-machine", name, versions).await
    }

    /// A machine with real npm, uv and curl, running HEAD.
    pub async fn real_machine(&self, name: &str) -> Machine {
        self.start("tether-e2e-real", name, &[HEAD]).await
    }

    async fn start(&self, image: &str, name: &str, versions: &[&str]) -> Machine {
        let mut req = GenericImage::new(image, "latest")
            .with_wait_for(WaitFor::Nothing)
            .with_network(format!("{}-net", self.prefix))
            .with_container_name(format!("{}-{name}", self.prefix))
            .with_hostname(name)
            .with_cmd(["sleep", "infinity"]);
        for v in versions {
            let label = label(v);
            let bin = root()
                .join("target/e2e/bin")
                .join(format!("tether-{label}"));
            assert!(bin.exists(), "{} is missing", bin.display());
            req = req.with_copy_to(
                CopyTargetOptions::new(format!("/usr/local/bin/tether-{label}")).with_mode(0o755),
                bin,
            );
        }
        let c = req.start().await.expect("start machine");
        let m = Machine {
            name: name.to_string(),
            log: self.logs.join(format!("{name}.log")),
            c,
            version: std::sync::Mutex::new(String::new()),
        };
        m.use_version(versions[0]).await;
        m
    }

    /// Runs a shell script in the bare repo on the git server.
    pub async fn remote(&self, script: &str) -> String {
        let out = exec(&self.server, &format!("cd /srv/e2e.git && {script}")).await;
        out.stdout.trim().to_string()
    }

    pub async fn head(&self) -> String {
        self.remote("git rev-parse -q --verify main || echo none")
            .await
    }

    pub async fn tree(&self, path: &str) -> String {
        self.remote(&format!(
            "git rev-parse main:{path} 2>/dev/null || echo none"
        ))
        .await
    }

    pub async fn changed(&self, from: &str, to: &str) -> Vec<String> {
        if from == "none" || from == to {
            return Vec::new();
        }
        self.remote(&format!("git diff --name-only {from} {to}"))
            .await
            .split_whitespace()
            .map(str::to_string)
            .collect()
    }

    /// Commits a change to the remote as someone with push access but no machine key.
    pub async fn push_edit(&self, script: &str) {
        let out = exec(
            &self.server,
            &format!(
                "set -e; rm -rf /tmp/w && git clone -q /srv/e2e.git /tmp/w && cd /tmp/w && \
                 {script} && git add -A && git commit -qm edit && git push -q origin HEAD:main"
            ),
        )
        .await;
        assert_eq!(out.code, 0, "remote edit failed: {}", out.text());
    }

    pub fn note(&self, line: &str) {
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.logs.join("events.log"))
            .unwrap();
        writeln!(f, "{line}").unwrap();
    }
}

pub struct Machine {
    pub name: String,
    log: PathBuf,
    c: ContainerAsync<GenericImage>,
    version: std::sync::Mutex<String>,
}

impl Machine {
    pub fn version(&self) -> String {
        self.version.lock().unwrap().clone()
    }

    /// Points `tether` at another binary this machine has.
    pub async fn use_version(&self, version: &str) {
        self.ok(&format!(
            "ln -sf tether-{} /usr/local/bin/tether",
            label(version)
        ))
        .await;
        *self.version.lock().unwrap() = version.to_string();
    }

    pub async fn sh(&self, script: &str) -> Out {
        let out = exec(&self.c, script).await;
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.log)
            .unwrap();
        writeln!(
            f,
            "$ {script}\n[{}] rc={}\n{}",
            self.version(),
            out.code,
            out.text()
        )
        .unwrap();
        out
    }

    /// Runs a script that must succeed.
    pub async fn ok(&self, script: &str) -> Out {
        let out = self.sh(script).await;
        assert_eq!(
            out.code,
            0,
            "{}: `{script}` failed:\n{}",
            self.name,
            out.text()
        );
        out
    }

    /// Runs tether without a terminal.
    pub async fn tether(&self, args: &str) -> Out {
        self.sh(&format!("tether {args} </dev/null")).await
    }

    /// Runs tether without a terminal; it must succeed.
    pub async fn tether_ok(&self, args: &str) -> Out {
        self.ok(&format!("tether {args} </dev/null")).await
    }

    /// `tether init` against the lab's repo, with each prompt's default answer.
    pub async fn init(&self, lab: &Lab) -> Out {
        self.sh(&format!(
            "expect -f {INIT_SCRIPT} tether init --repo {} --no-daemon",
            lab.repo_url()
        ))
        .await
    }

    pub async fn read(&self, path: &str) -> String {
        exec(&self.c, &format!("cat {path}")).await.stdout
    }

    pub async fn machine_id(&self) -> String {
        let state: serde_json::Value =
            serde_json::from_str(&self.read("/root/.tether/state.json").await).expect("state.json");
        state["machine_id"].as_str().unwrap().to_string()
    }

    pub async fn fingerprint(&self) -> String {
        let out = self.ok("ssh-keygen -lf /root/.tether/signing_key").await;
        out.stdout.split_whitespace().nth(1).unwrap().to_string()
    }

    /// Seeds the shim's installed packages: `/state/pkgs/<key>` lines of `name version`.
    pub async fn seed(&self, key: &str, name: &str, version: &str) {
        self.ok(&format!("echo '{name} {version}' >> /state/pkgs/{key}"))
            .await;
    }

    /// The version of a package the shims list as installed.
    pub async fn installed(&self, key: &str, name: &str) -> Option<String> {
        self.read(&format!("/state/pkgs/{key}"))
            .await
            .lines()
            .filter_map(|l| l.split_once(' '))
            .find(|(n, _)| *n == name)
            .map(|(_, v)| v.to_string())
    }

    /// `tether packages inbox --json`.
    pub async fn inbox(&self) -> Vec<serde_json::Value> {
        let out = self.tether_ok("packages inbox --json").await;
        serde_json::from_str(&out.stdout).expect("packages inbox --json")
    }

    /// Every call the shims logged, in order.
    pub async fn calls(&self) -> Vec<serde_json::Value> {
        self.read("/state/calls.jsonl")
            .await
            .lines()
            .filter(|l| !l.is_empty())
            .map(|l| serde_json::from_str(l).unwrap())
            .collect()
    }

    /// Installs the shims logged.
    pub async fn installs(&self) -> Vec<Install> {
        let mut out = Vec::new();
        for c in self.calls().await {
            for i in c["installs"].as_array().into_iter().flatten() {
                out.push(Install {
                    key: i["key"].as_str().unwrap().to_string(),
                    name: i["name"].as_str().unwrap().to_string(),
                    spec: i["spec"].as_str().unwrap().to_string(),
                });
            }
        }
        out
    }
}

#[derive(Debug, Clone)]
pub struct Install {
    pub key: String,
    pub name: String,
    pub spec: String,
}

async fn exec(c: &ContainerAsync<GenericImage>, script: &str) -> Out {
    let mut r = c
        .exec(ExecCommand::new(["sh", "-c", script]))
        .await
        .expect("docker exec");
    let stdout = String::from_utf8_lossy(&r.stdout_to_vec().await.unwrap()).into_owned();
    let stderr = String::from_utf8_lossy(&r.stderr_to_vec().await.unwrap()).into_owned();
    // The exit code can lag the end of the output streams
    let code = loop {
        if let Some(code) = r.exit_code().await.unwrap() {
            break code;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    };
    Out {
        code,
        stdout,
        stderr,
    }
}

//! Signed fleet commands: one machine asks another to change itself, such as to uninstall a
//! package. A command is `commands/<target>/<signer>-<seq>.toml` in the personal sync repo,
//! with a `.sig` beside it. The target runs it on its next sync when the signer's key is the
//! key it trusts for that signer, the sequence number is new for that key, and the command
//! has not expired. Trusting a machine already lets its signed record install packages here,
//! so trust also grants commands. Team and collab repos never read `commands/`.

use crate::cli::Output;
use crate::config::Config;
use crate::sync::signing::{self, TrustStore};
use crate::sync::state::{valid_machine_id, CommandResult, CommandStatus};
use crate::sync::{GitBackend, MachineState, SyncState};
use anyhow::{anyhow, bail, Context, Result};
use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use ssh_key::{Algorithm, HashAlg, LineEnding, PrivateKey, PublicKey, SshSig};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// A namespace of its own, so no record or commit signature passes as a command signature.
const COMMAND_NAMESPACE: &str = "tether-command";
pub const EXPIRY_DAYS: i64 = 7;
/// Larger files are not commands Tether wrote, and are not read.
const MAX_COMMAND_BYTES: u64 = 16 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum Action {
    /// Uninstall a package, and record the removal so syncs stop installing it
    Uninstall { manager: String, name: String },
    /// Install a package through the gates of a manual install
    Install { manager: String, name: String },
    /// Assign the target's profile
    Profile { profile: String },
    /// Trust the key that signs `machine`'s record, when its fingerprint matches
    Trust {
        machine: String,
        fingerprint: String,
    },
}

impl Action {
    pub fn describe(&self) -> String {
        match self {
            Action::Uninstall { manager, name } => format!("uninstall {}:{}", manager, name),
            Action::Install { manager, name } => format!("install {}:{}", manager, name),
            Action::Profile { profile } => format!("set profile {}", profile),
            Action::Trust {
                machine,
                fingerprint,
            } => format!("trust {} {}", machine, fingerprint),
        }
    }

    /// Fields a command file must hold, so a target never acts on a name a package manager
    /// or the trust store would refuse.
    fn check(&self) -> Result<()> {
        match self {
            Action::Uninstall { manager, name } | Action::Install { manager, name } => {
                if !crate::packages::MANAGER_KEYS.contains(&manager.as_str()) {
                    bail!("unknown package manager {}", manager);
                }
                if !MachineState::is_safe_package_name(name) || name.starts_with('-') {
                    bail!("invalid package name {}", name);
                }
            }
            Action::Profile { profile } => {
                if !Config::is_safe_profile_name(profile) {
                    bail!("invalid profile name {}", profile);
                }
            }
            Action::Trust { machine, .. } => {
                if !valid_machine_id(machine) {
                    bail!("invalid machine id {}", machine);
                }
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Command {
    pub target: String,
    pub signer: String,
    pub seq: u64,
    pub expires_at: DateTime<Utc>,
    #[serde(flatten)]
    pub action: Action,
}

/// What a command signature covers. The body hash binds the action; the other fields are
/// also in the body and in the file path, and a target checks that all three agree.
fn message(command: &Command, body: &[u8]) -> Vec<u8> {
    format!(
        "tether-command-v1\n{}\n{}\n{}\n{}\n{}\n",
        command.target,
        command.signer,
        command.seq,
        command.expires_at.timestamp(),
        crate::sha256_hex(body)
    )
    .into_bytes()
}

fn commands_dir(sync_path: &Path) -> PathBuf {
    sync_path.join("commands")
}

fn command_path(sync_path: &Path, target: &str, signer: &str, seq: u64) -> PathBuf {
    commands_dir(sync_path)
        .join(target)
        .join(format!("{}-{}.toml", signer, seq))
}

/// `commands/<target>`, when it and `commands/` are real directories; `create` makes them
/// first. A repo writer can commit symlinks, and access through one lands outside the repo.
fn target_dir(sync_path: &Path, target: &str, create: bool) -> Option<PathBuf> {
    let commands = commands_dir(sync_path);
    let dir = commands.join(target);
    for d in [&commands, &dir] {
        if create {
            let _ = std::fs::create_dir(d);
        }
        if !std::fs::symlink_metadata(d).is_ok_and(|m| m.is_dir()) {
            return None;
        }
    }
    Some(dir)
}

fn sig_path(path: &Path) -> PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(".sig");
    PathBuf::from(name)
}

/// The signer and sequence number in a command file name, `<signer>-<seq>.toml`.
fn parse_file_name(name: &str) -> Option<(&str, u64)> {
    let (signer, seq) = name.strip_suffix(".toml")?.rsplit_once('-')?;
    let parsed: u64 = seq.parse().ok()?;
    // One number has one file name, so a copy under `a-07.toml` cannot run `a-7.toml` again
    if !valid_machine_id(signer) || parsed.to_string() != seq {
        return None;
    }
    Some((signer, parsed))
}

/// Write `command` and its signature into the sync repo.
fn write(sync_path: &Path, command: &Command, key: &PrivateKey) -> Result<PathBuf> {
    let body = toml::to_string(command)?;
    let signature = key
        .sign(
            COMMAND_NAMESPACE,
            HashAlg::Sha512,
            &message(command, body.as_bytes()),
        )?
        .to_pem(LineEnding::LF)?;
    let path = command_path(sync_path, &command.target, &command.signer, command.seq);
    if target_dir(sync_path, &command.target, true).is_none() {
        bail!("{} is not a directory", path.parent().unwrap().display());
    }
    for file in [path.clone(), sig_path(&path)] {
        if std::fs::symlink_metadata(&file).is_ok() {
            bail!("{} exists already", file.display());
        }
    }
    crate::sync::atomic_write(&path, body.as_bytes())?;
    if let Err(e) = crate::sync::atomic_write(&sig_path(&path), signature.as_bytes()) {
        let _ = std::fs::remove_file(&path);
        return Err(e);
    }
    Ok(path)
}

/// A file of at most `MAX_COMMAND_BYTES`. A repo writer can commit a symlink, so a link to a
/// device or a pipe is never followed.
fn read_small(path: &Path) -> Result<Vec<u8>> {
    let meta = std::fs::symlink_metadata(path)?;
    if !meta.is_file() {
        bail!("not a regular file");
    }
    if meta.len() > MAX_COMMAND_BYTES {
        bail!("file too large");
    }
    Ok(std::fs::read(path)?)
}

/// A command file as it is in the repo. `verified` is true when `key` signed it.
fn read(path: &Path, target: &str, key: Option<&PublicKey>) -> Result<(Command, bool)> {
    let name = path
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or_else(|| anyhow!("invalid file name"))?;
    let (signer, seq) = parse_file_name(name).ok_or_else(|| anyhow!("invalid file name"))?;
    let body = read_small(path)?;
    let command: Command = toml::from_str(std::str::from_utf8(&body)?)?;
    if command.target != target || command.signer != signer || command.seq != seq {
        bail!("its fields do not match its path");
    }
    command.action.check()?;
    let verified = key.is_some_and(|key| {
        read_small(&sig_path(path))
            .ok()
            .and_then(|pem| SshSig::from_pem(pem).ok())
            .is_some_and(|sig| {
                key.algorithm() == Algorithm::Ed25519
                    && key
                        .verify(COMMAND_NAMESPACE, &message(&command, &body), &sig)
                        .is_ok()
            })
    });
    Ok((command, verified))
}

/// Command files in `commands/<target>/`, sorted by signer and sequence number.
fn files_for(sync_path: &Path, target: &str) -> Vec<PathBuf> {
    let Some(Ok(entries)) = target_dir(sync_path, target, false).map(std::fs::read_dir) else {
        return Vec::new();
    };
    let mut files: Vec<(String, u64, PathBuf)> = entries
        .flatten()
        .filter_map(|e| {
            let name = e.file_name().into_string().ok()?;
            let (signer, seq) = parse_file_name(&name)?;
            Some((signer.to_string(), seq, e.path()))
        })
        .collect();
    files.sort();
    files.into_iter().map(|(_, _, path)| path).collect()
}

/// Sequence numbers in `~/.tether/command_seqs.json`, never synced: the last number this
/// machine sent, and the newest number it accepted from each signer key.
#[derive(Debug, Default, Serialize, Deserialize)]
pub struct Seqs {
    #[serde(default)]
    sent: u64,
    #[serde(default)]
    accepted: BTreeMap<String, u64>,
}

impl Seqs {
    fn path() -> Result<PathBuf> {
        Ok(crate::home_dir()?.join(".tether").join("command_seqs.json"))
    }

    pub fn load() -> Result<Self> {
        let path = Self::path()?;
        if !path.exists() {
            return Ok(Self::default());
        }
        serde_json::from_slice(&std::fs::read(&path)?)
            .with_context(|| format!("Invalid command sequence file {}", path.display()))
    }

    pub fn save(&self) -> Result<()> {
        crate::sync::atomic_write(&Self::path()?, &serde_json::to_vec_pretty(self)?)
    }
}

/// A command this machine may run, with the fingerprint of the key that signed it.
#[derive(Debug, Clone, PartialEq)]
pub struct Incoming {
    pub command: Command,
    pub fingerprint: String,
}

/// The commands for `this_machine` that a trusted key signed and that `seqs` has not
/// accepted yet, in order. Files that do not verify are skipped with one warning each.
pub fn incoming(
    sync_path: &Path,
    this_machine: &str,
    store: &TrustStore,
    seqs: &Seqs,
) -> Vec<Incoming> {
    let mut found = Vec::new();
    for path in files_for(sync_path, this_machine) {
        let shown = path
            .strip_prefix(sync_path)
            .unwrap_or(&path)
            .display()
            .to_string();
        let signer = path
            .file_name()
            .and_then(|n| n.to_str())
            .and_then(parse_file_name)
            .map(|(signer, _)| signer.to_string())
            .unwrap_or_default();
        if signer == this_machine {
            continue;
        }
        let key = store.key_for(&signer);
        let (command, verified) = match read(&path, this_machine, key) {
            Ok(read) => read,
            Err(e) => {
                warn_once(&shown, &format!("Ignoring {}: {}", shown, e));
                continue;
            }
        };
        let Some(key) = key else {
            warn_once(
                &shown,
                &format!(
                    "Ignoring {}: this machine does not trust {}. Trust it to run its commands",
                    shown, signer
                ),
            );
            continue;
        };
        if !verified {
            warn_once(
                &shown,
                &format!(
                    "Ignoring {}: its signature is not from the key trusted for {}",
                    shown, signer
                ),
            );
            continue;
        }
        let fingerprint = signing::fingerprint(key);
        if seqs
            .accepted
            .get(&fingerprint)
            .is_some_and(|&last| command.seq <= last)
        {
            continue;
        }
        found.push(Incoming {
            command,
            fingerprint,
        });
    }
    found
}

fn warn_once(key: &str, message: &str) {
    if crate::packages::policy::first_warning(&format!("command {}", key)) {
        Output::warning(message);
    }
}

/// Run the commands for this machine, after the config merge and before the sync reads
/// this machine's packages or profile. Each command's number is saved before it runs, so a
/// crash never runs it twice. Results go into this machine's local record; the sync signs
/// and pushes it. The caller holds the sync lock.
pub async fn apply_incoming(
    config: &mut Config,
    sync_path: &Path,
    state: &mut SyncState,
) -> Result<Vec<CommandResult>> {
    let this_machine = state.machine_id.clone();
    let store = TrustStore::load()?;
    let mut seqs = Seqs::load()?;
    // A lost command_seqs.json never replays a command that this machine's record reports
    if let Some(record) = signing::own_record(sync_path, &this_machine)? {
        for (fingerprint, reported) in &record.commands_applied {
            if let Some(last) = reported.iter().map(|r| r.seq).max() {
                let accepted = seqs.accepted.entry(fingerprint.clone()).or_default();
                *accepted = (*accepted).max(last);
            }
        }
    }
    let mut results = Vec::new();
    for incoming in incoming(sync_path, &this_machine, &store, &seqs) {
        let command = &incoming.command;
        // An earlier command in this batch can replace the signer's key
        let still_trusted = TrustStore::load()?
            .key_for(&command.signer)
            .is_some_and(|key| signing::fingerprint(key) == incoming.fingerprint);
        let seen = seqs
            .accepted
            .get(&incoming.fingerprint)
            .is_some_and(|&last| command.seq <= last);
        if !still_trusted || seen {
            continue;
        }
        seqs.accepted
            .insert(incoming.fingerprint.clone(), command.seq);
        seqs.save()?;
        let outcome = if command.expires_at <= Utc::now() {
            Err(None)
        } else {
            run(&command.action, config, sync_path, state)
                .await
                .map_err(Some)
        };
        let (status, error) = match outcome {
            Ok(()) => (CommandStatus::Applied, None),
            Err(None) => (CommandStatus::Expired, None),
            Err(Some(e)) => (CommandStatus::Failed, Some(e.to_string())),
        };
        let result = CommandResult {
            seq: command.seq,
            action: format!("{} from {}", command.action.describe(), command.signer),
            status,
            error,
            at: Utc::now(),
        };
        let mut record = signing::own_record(sync_path, &this_machine)?
            .unwrap_or_else(|| MachineState::new(&this_machine));
        let list = record
            .commands_applied
            .entry(incoming.fingerprint.clone())
            .or_default();
        list.push(result.clone());
        let extra = list.len().saturating_sub(MachineState::MAX_COMMAND_RESULTS);
        list.drain(..extra);
        // A record over the signer limit fails validation, so the stalest signer key goes
        while record.commands_applied.len() > MachineState::MAX_COMMAND_SIGNERS {
            let Some(stalest) = record
                .commands_applied
                .iter()
                .min_by_key(|(_, list)| list.iter().map(|r| r.at).max())
                .map(|(fingerprint, _)| fingerprint.clone())
            else {
                break;
            };
            record.commands_applied.remove(&stalest);
        }
        signing::save_local_record(&record)?;
        results.push(result);
    }
    Ok(results)
}

async fn run(
    action: &Action,
    config: &mut Config,
    sync_path: &Path,
    state: &mut SyncState,
) -> Result<()> {
    let this_machine = state.machine_id.clone();
    match action {
        Action::Uninstall { manager, name } => {
            if manager == "brew_taps" {
                bail!("Tether does not remove taps");
            }
            if crate::packages::installed_here(manager, name).await? {
                crate::packages::uninstall(manager, name, state).await?;
            }
            // The removal stops syncs from installing it again, also when it was not here
            crate::packages::stop_installing(manager, name, state)
        }
        Action::Install { manager, name } => {
            if crate::packages::installed_here(manager, name).await? {
                return Ok(());
            }
            // An install that OSV could not check fails, as no one is here to decide
            let checked = crate::packages::inbox::check_manual_install(manager, name, true).await?;
            crate::packages::inbox::install_from_machine(manager, name, checked, false).await
        }
        Action::Profile { profile } => {
            if !config.profiles.contains_key(profile) && profile != crate::config::DEFAULT_PROFILE {
                bail!("No profile {}", profile);
            }
            config
                .machine_profiles
                .insert(this_machine, profile.clone());
            config.save()
        }
        Action::Trust {
            machine,
            fingerprint,
        } => {
            if *machine == this_machine {
                bail!("A machine does not trust its own key by command");
            }
            crate::packages::inbox::trust_machine(sync_path, machine, fingerprint)?;
            Ok(())
        }
    }
}

/// The next sequence number for this machine's key: past the local count, every command
/// file this key signed in the repo, and every result that a trusted record in `trusted`
/// reported, so a lost local count never reuses a number that a target already accepted.
/// Unsigned files and untrusted records do not count, so a repo writer cannot exhaust the
/// numbers.
fn next_seq(
    sync_path: &Path,
    signer: &str,
    key: &PublicKey,
    trusted: &[String],
    seqs: &Seqs,
) -> Result<u64> {
    let fingerprint = signing::fingerprint(key);
    let in_files = std::fs::read_dir(commands_dir(sync_path))
        .into_iter()
        .flatten()
        .flatten()
        .flat_map(|target| {
            let target = target.file_name().to_string_lossy().to_string();
            files_for(sync_path, &target)
                .into_iter()
                .filter_map(move |path| {
                    let (command, verified) = read(&path, &target, Some(key)).ok()?;
                    (verified && command.signer == signer).then_some(command.seq)
                })
        })
        .max()
        .unwrap_or(0);
    let in_results = MachineState::list_all(sync_path)
        .unwrap_or_default()
        .iter()
        .filter(|m| trusted.contains(&m.machine_id))
        .filter_map(|m| m.commands_applied.get(&fingerprint))
        .flatten()
        .map(|r| r.seq)
        .max()
        .unwrap_or(0);
    seqs.sent
        .max(in_files)
        .max(in_results)
        .checked_add(1)
        .ok_or_else(|| anyhow!("This machine has no fleet command numbers left"))
}

/// Sign `action` for each target and write it into the sync repo. The caller holds the sync
/// lock, then commits and pushes. Returns each target with its command's number.
pub fn send(
    sync_path: &Path,
    signer: &str,
    key: &PrivateKey,
    targets: &[String],
    action: &Action,
) -> Result<Vec<(String, u64)>> {
    action.check()?;
    let mut seqs = Seqs::load()?;
    let trusted = trusted_ids(sync_path, signer)?;
    let first = next_seq(sync_path, signer, key.public_key(), &trusted, &seqs)?;
    if first.checked_add(targets.len() as u64).is_none() {
        bail!("This machine has no fleet command numbers left");
    }
    let expires_at = Utc::now() + Duration::days(EXPIRY_DAYS);
    let mut sent: Vec<(String, u64)> = Vec::new();
    let mut written: Vec<PathBuf> = Vec::new();
    for (seq, target) in (first..).zip(targets) {
        let command = Command {
            target: target.clone(),
            signer: signer.to_string(),
            seq,
            expires_at,
            action: action.clone(),
        };
        // A failed send leaves no command behind, or the next sync would push part of it
        match write(sync_path, &command, key) {
            Ok(path) => written.push(path),
            Err(e) => {
                for path in &written {
                    let _ = std::fs::remove_file(sig_path(path));
                    let _ = std::fs::remove_file(path);
                }
                return Err(e);
            }
        }
        sent.push((target.clone(), seq));
        seqs.sent = seq;
    }
    seqs.save()?;
    Ok(sent)
}

/// Why this machine cannot send fleet commands to `id`, or `None` when it can. `record` is
/// the target's record, and `trusted` whether it verifies with the key this machine trusts.
pub fn refusal(
    this: &str,
    id: &str,
    record: Option<&MachineState>,
    trusted: bool,
) -> Option<String> {
    if id == this {
        return Some(format!("{} is this machine", id));
    }
    if !trusted {
        return Some(format!(
            "Machine {} is not trusted. Trust it first: tether machines trust {}",
            id, id
        ));
    }
    let Some(record) = record else {
        return Some(format!("Machine {} has no machine record", id));
    };
    (!record.runs_commands).then(|| {
        let version = match record.cli_version.as_str() {
            "" => "1.x",
            v => v,
        };
        format!(
            "Machine {} runs Tether {}, which cannot run fleet commands. Upgrade it first",
            id, version
        )
    })
}

/// The ids of the machines whose records verify with the key this machine trusts.
pub fn trusted_ids(sync_path: &Path, this: &str) -> Result<Vec<String>> {
    Ok(signing::record_statuses(sync_path, this)?
        .into_iter()
        .filter(|(_, status, _)| *status == signing::RecordStatus::Trusted)
        .map(|(id, _, _)| id)
        .collect())
}

/// Fail on the first of `ids` that cannot run fleet commands from this machine.
pub fn check_targets(sync_path: &Path, this: &str, ids: &[String]) -> Result<()> {
    let trusted = trusted_ids(sync_path, this)?;
    let records = MachineState::list_all(sync_path)?;
    for id in ids {
        let record = records.iter().find(|m| m.machine_id == *id);
        if let Some(why) = refusal(this, id, record, trusted.contains(id)) {
            bail!(why);
        }
    }
    Ok(())
}

/// Pull, sign `action` for the machines that `pick` names from the pulled repo and this
/// machine's id, then commit and push. The caller holds the sync lock.
pub fn deliver(
    sync_path: &Path,
    action: &Action,
    pick: impl FnOnce(&Path, &str) -> Result<Vec<String>>,
) -> Result<Vec<(String, u64)>> {
    if !Config::load()?.has_personal_features() {
        bail!("Fleet commands are not available in team-only mode");
    }
    let git = GitBackend::open(sync_path)?;
    // The commit takes every change in the repo, so other changes would ship as the command
    if git.has_changes()? {
        bail!("The sync repo has uncommitted changes. Run 'tether sync' first");
    }
    git.pull()?;
    if let Action::Profile { profile } = action {
        check_profile_pushed(&git, profile)?;
    }
    let this = SyncState::load()?.machine_id;
    let mut targets = pick(sync_path, &this)?;
    // A machine trusts its own key already
    if let Action::Trust { machine, .. } = action {
        targets.retain(|t| t != machine);
        if targets.is_empty() {
            bail!("Name machines other than {} itself", machine);
        }
    }
    let key = signing::load_or_create(&this)?;
    let sent = send(sync_path, &this, &key, &targets, action)?;
    git.commit(
        &format!("Send fleet command: {}", action.describe()),
        &crate::sync::local_hostname(),
    )?;
    if let Err(e) = git.push() {
        bail!(
            "Saved the command, but the push failed: {}. The next sync pushes it",
            e
        );
    }
    Ok(sent)
}

/// A target reads profiles from the synced config at origin/main, so a profile that only
/// this machine's config defines yet would fail there. Without a synced config, each
/// machine keeps its own profiles and the target decides.
fn check_profile_pushed(git: &GitBackend, profile: &str) -> Result<()> {
    if profile == crate::config::DEFAULT_PROFILE {
        return Ok(());
    }
    let Some(encrypted) = crate::sync::SYNCED_CONFIG
        .iter()
        .find_map(|path| git.show_pushed(path).ok())
    else {
        return Ok(());
    };
    let plain = crate::security::decrypt(&encrypted, &crate::security::get_encryption_key()?)?;
    let synced: toml::Value = toml::from_str(std::str::from_utf8(&plain)?)?;
    if synced
        .get("profiles")
        .and_then(|p| p.get(profile))
        .is_none()
    {
        bail!(
            "The synced config does not have profile {} yet. Run 'tether sync' first, then send the command again",
            profile
        );
    }
    Ok(())
}

/// What a cancel found in the pulled repo.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Cancelled {
    /// The command was deleted and the deletion pushed
    Deleted,
    /// The target's trusted record reports the command already
    Reported(CommandStatus),
    /// The command is no longer in the repo
    Gone,
}

/// Delete a command this machine sent to `target`, then commit and push, unless the target's
/// trusted record reports it already. A target that pulled the command before the push may
/// still run it; its record then reports it. The caller holds the sync lock.
pub fn cancel(sync_path: &Path, target: &str, seq: u64) -> Result<Cancelled> {
    let git = GitBackend::open(sync_path)?;
    // The commit takes every change in the repo, so other changes would ship as the cancel
    if git.has_changes()? {
        bail!("The sync repo has uncommitted changes. Run 'tether sync' first");
    }
    git.pull()?;
    let this = SyncState::load()?.machine_id;
    let own =
        signing::own_fingerprint().ok_or_else(|| anyhow!("This machine has no signing key"))?;
    // A record that does not verify could claim a result to keep the command alive
    if trusted_ids(sync_path, &this)?.iter().any(|id| id == target) {
        let reported = MachineState::list_all(sync_path)?
            .into_iter()
            .find(|m| m.machine_id == target)
            .and_then(|m| m.commands_applied.get(&own).cloned())
            .and_then(|results| results.into_iter().find(|r| r.seq == seq));
        if let Some(result) = reported {
            return Ok(Cancelled::Reported(result.status));
        }
    }
    let path = command_path(sync_path, target, &this, seq);
    if target_dir(sync_path, target, false).is_none() || !path.exists() {
        return Ok(Cancelled::Gone);
    }
    let action = read(&path, target, None).map(|(c, _)| c.action.describe());
    std::fs::remove_file(&path)?;
    let _ = std::fs::remove_file(sig_path(&path));
    git.commit(
        &format!(
            "Cancel fleet command: {}",
            action.unwrap_or_else(|_| format!("#{}", seq))
        ),
        &crate::sync::local_hostname(),
    )?;
    if let Err(e) = git.push() {
        bail!(
            "Deleted the command here, but the push failed: {}. The next sync pushes it",
            e
        );
    }
    Ok(Cancelled::Deleted)
}

/// A command still in the repo, as `machines show` lists it.
#[derive(Debug, Clone)]
pub struct Pending {
    pub command: Command,
    /// Whether the key this machine trusts for the signer signed it
    pub verified: bool,
}

/// The commands for `target` that wait in the repo: newer than the latest result `target`
/// reported for their signer key, as this machine reads that key.
pub fn pending(sync_path: &Path, target: &MachineState, store: &TrustStore) -> Vec<Pending> {
    files_for(sync_path, &target.machine_id)
        .into_iter()
        .filter_map(|path| {
            let signer = parse_file_name(path.file_name()?.to_str()?)?.0.to_string();
            let key = store.key_for(&signer);
            let (command, verified) = read(&path, &target.machine_id, key).ok()?;
            let done = key
                .and_then(|k| target.commands_applied.get(&signing::fingerprint(k)))
                .and_then(|results| results.iter().map(|r| r.seq).max())
                .is_some_and(|last| command.seq <= last);
            (!done).then_some(Pending { command, verified })
        })
        .collect()
}

/// Delete the commands this machine sent once their target's trusted record shows a result
/// at or past their number, or once they expired. A target runs its commands in order, so a
/// lower number it skipped never runs. Also removes command folders that end up empty.
pub fn prune_sent(sync_path: &Path, this_machine: &str) -> Result<()> {
    let Some(own) = signing::own_fingerprint() else {
        return Ok(());
    };
    let Ok(entries) = std::fs::read_dir(commands_dir(sync_path)) else {
        return Ok(());
    };
    let trusted = trusted_ids(sync_path, this_machine)?;
    let records = MachineState::list_all(sync_path)?;
    let reported = |target: &str| -> Option<u64> {
        if !trusted.iter().any(|id| id == target) {
            return None;
        }
        records
            .iter()
            .find(|m| m.machine_id == target)?
            .commands_applied
            .get(&own)?
            .iter()
            .map(|r| r.seq)
            .max()
    };
    for entry in entries.flatten() {
        let target_id = entry.file_name().to_string_lossy().to_string();
        let Some(dir) = target_dir(sync_path, &target_id, false) else {
            continue;
        };
        let last = reported(&target_id);
        for path in files_for(sync_path, &target_id) {
            let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
                continue;
            };
            let Some((signer, seq)) = parse_file_name(name) else {
                continue;
            };
            if signer != this_machine {
                continue;
            }
            let expired = read(&path, &target_id, None)
                .map(|(c, _)| c.expires_at <= Utc::now())
                .unwrap_or(true);
            if expired || last.is_some_and(|last| seq <= last) {
                std::fs::remove_file(&path)?;
                let _ = std::fs::remove_file(sig_path(&path));
            }
        }
        // Not a race: git keeps no empty folders, so one left here is only local
        if std::fs::read_dir(&dir).is_ok_and(|mut d| d.next().is_none()) {
            std::fs::remove_dir(&dir)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use ssh_key::rand_core::OsRng;

    fn key() -> PrivateKey {
        PrivateKey::random(&mut OsRng, Algorithm::Ed25519).unwrap()
    }

    fn command(target: &str, signer: &str, seq: u64) -> Command {
        Command {
            target: target.to_string(),
            signer: signer.to_string(),
            seq,
            expires_at: Utc::now() + Duration::days(1),
            action: Action::Uninstall {
                manager: "npm".to_string(),
                name: "typescript".to_string(),
            },
        }
    }

    fn store(id: &str, key: &PrivateKey) -> TrustStore {
        let mut store = TrustStore::default();
        store.trust(id, key.public_key()).unwrap();
        store
    }

    #[cfg(unix)]
    #[test]
    fn a_write_never_follows_a_planted_symlink() {
        let dir = tempfile::tempdir().unwrap();
        let outside = dir.path().join("signing_key");
        std::fs::write(&outside, "secret").unwrap();
        let repo = dir.path().join("sync");
        let target = repo.join("commands").join("mini");
        std::fs::create_dir_all(&target).unwrap();
        std::os::unix::fs::symlink(&outside, target.join("laptop-1.toml.sig")).unwrap();
        assert!(write(&repo, &command("mini", "laptop", 1), &key()).is_err());

        let linked = dir.path().join("linked");
        std::fs::create_dir_all(repo.join("commands")).unwrap();
        std::fs::create_dir_all(&linked).unwrap();
        std::os::unix::fs::symlink(&linked, repo.join("commands").join("studio")).unwrap();
        assert!(write(&repo, &command("studio", "laptop", 1), &key()).is_err());
        assert_eq!(std::fs::read_to_string(&outside).unwrap(), "secret");
        assert_eq!(std::fs::read_dir(&linked).unwrap().count(), 0);
    }

    #[test]
    fn a_trusted_signed_command_is_incoming_once() {
        let dir = tempfile::tempdir().unwrap();
        let laptop = key();
        write(dir.path(), &command("mini", "laptop", 1), &laptop).unwrap();
        let store = store("laptop", &laptop);
        let mut seqs = Seqs::default();
        let found = incoming(dir.path(), "mini", &store, &seqs);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].command.seq, 1);
        assert_eq!(found[0].command.action, command("mini", "laptop", 1).action);
        seqs.accepted
            .insert(found[0].fingerprint.clone(), found[0].command.seq);
        // The same file again is a replay
        assert!(incoming(dir.path(), "mini", &store, &seqs).is_empty());
    }

    #[test]
    fn a_lower_number_after_a_higher_one_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let laptop = key();
        let store = store("laptop", &laptop);
        let mut seqs = Seqs::default();
        seqs.accepted
            .insert(signing::fingerprint(laptop.public_key()), 5);
        write(dir.path(), &command("mini", "laptop", 4), &laptop).unwrap();
        write(dir.path(), &command("mini", "laptop", 5), &laptop).unwrap();
        write(dir.path(), &command("mini", "laptop", 6), &laptop).unwrap();
        let found = incoming(dir.path(), "mini", &store, &seqs);
        assert_eq!(found.iter().map(|i| i.command.seq).collect::<Vec<_>>(), [6]);
    }

    #[test]
    fn an_untrusted_or_other_key_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let laptop = key();
        let intruder = key();
        write(dir.path(), &command("mini", "laptop", 1), &intruder).unwrap();
        assert!(incoming(
            dir.path(),
            "mini",
            &store("laptop", &laptop),
            &Seqs::default()
        )
        .is_empty());
        // No key trusted for the signer at all
        assert!(incoming(dir.path(), "mini", &TrustStore::default(), &Seqs::default()).is_empty());
    }

    #[test]
    fn a_command_for_another_machine_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let laptop = key();
        let path = write(dir.path(), &command("studio", "laptop", 1), &laptop).unwrap();
        // Copied into this machine's folder, the signed target still names the other one
        let moved = command_path(dir.path(), "mini", "laptop", 1);
        std::fs::create_dir_all(moved.parent().unwrap()).unwrap();
        std::fs::copy(&path, &moved).unwrap();
        std::fs::copy(sig_path(&path), sig_path(&moved)).unwrap();
        assert!(incoming(
            dir.path(),
            "mini",
            &store("laptop", &laptop),
            &Seqs::default()
        )
        .is_empty());
    }

    #[test]
    fn an_edited_command_fails_its_signature() {
        let dir = tempfile::tempdir().unwrap();
        let laptop = key();
        let path = write(dir.path(), &command("mini", "laptop", 1), &laptop).unwrap();
        let body = std::fs::read_to_string(&path)
            .unwrap()
            .replace("typescript", "left-pad");
        std::fs::write(&path, body).unwrap();
        assert!(incoming(
            dir.path(),
            "mini",
            &store("laptop", &laptop),
            &Seqs::default()
        )
        .is_empty());
    }

    #[test]
    fn a_later_expiry_fails_the_signature() {
        let dir = tempfile::tempdir().unwrap();
        let laptop = key();
        let mut old = command("mini", "laptop", 1);
        old.expires_at = Utc::now() - Duration::days(1);
        let path = write(dir.path(), &old, &laptop).unwrap();
        let (read_back, verified) = read(&path, "mini", Some(laptop.public_key())).unwrap();
        assert!(verified);
        assert!(read_back.expires_at <= Utc::now());
        let renewed = Command {
            expires_at: Utc::now() + Duration::days(30),
            ..old
        };
        std::fs::write(&path, toml::to_string(&renewed).unwrap()).unwrap();
        assert!(!read(&path, "mini", Some(laptop.public_key())).unwrap().1);
    }

    #[cfg(unix)]
    #[test]
    fn a_symlinked_command_file_is_never_read() {
        let dir = tempfile::tempdir().unwrap();
        let path = command_path(dir.path(), "mini", "laptop", 1);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::os::unix::fs::symlink("/dev/zero", &path).unwrap();
        assert!(read(&path, "mini", None).is_err());
    }

    #[test]
    fn unsafe_package_names_never_parse_as_commands() {
        let bad = Action::Install {
            manager: "npm".to_string(),
            name: "--registry=evil".to_string(),
        };
        assert!(bad.check().is_err());
        let unknown = Action::Uninstall {
            manager: "apt".to_string(),
            name: "curl".to_string(),
        };
        assert!(unknown.check().is_err());
    }

    #[test]
    fn file_names_parse_with_dashes_in_the_signer() {
        assert_eq!(
            parse_file_name("mac-mini-2-12.toml"),
            Some(("mac-mini-2", 12))
        );
        assert_eq!(parse_file_name("laptop-x.toml"), None);
        assert_eq!(parse_file_name("laptop-1.toml.sig"), None);
        // A second name for the same number would let a copy run again
        assert_eq!(parse_file_name("laptop-07.toml"), None);
        assert_eq!(parse_file_name("laptop-+7.toml"), None);
    }

    #[test]
    fn next_number_passes_files_and_reported_results() {
        let dir = tempfile::tempdir().unwrap();
        let laptop = key();
        write(dir.path(), &command("mini", "laptop", 7), &laptop).unwrap();
        let seqs = Seqs {
            sent: 3,
            ..Seqs::default()
        };
        let public = laptop.public_key();
        let trusted = ["studio".to_string()];
        assert_eq!(
            next_seq(dir.path(), "laptop", public, &trusted, &seqs).unwrap(),
            8
        );
        // A file that this key did not sign does not count
        write(dir.path(), &command("mini", "laptop", u64::MAX), &key()).unwrap();
        assert_eq!(
            next_seq(dir.path(), "laptop", public, &trusted, &seqs).unwrap(),
            8
        );
        let mut record = MachineState::new("studio");
        record.commands_applied.insert(
            signing::fingerprint(public),
            vec![CommandResult {
                seq: 20,
                action: "x".to_string(),
                status: CommandStatus::Applied,
                error: None,
                at: Utc::now(),
            }],
        );
        record.save_to_repo(dir.path()).unwrap();
        assert_eq!(
            next_seq(dir.path(), "laptop", public, &trusted, &seqs).unwrap(),
            21
        );
        // A record that this machine does not trust does not count
        assert_eq!(
            next_seq(dir.path(), "laptop", public, &[], &seqs).unwrap(),
            8
        );
    }
}

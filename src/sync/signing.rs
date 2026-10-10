use crate::cli::Output;
use crate::sync::state::valid_machine_id;
use crate::sync::MachineState;
use anyhow::{anyhow, bail, Context, Result};
use serde::{Deserialize, Serialize};
use ssh_key::rand_core::OsRng;
use ssh_key::{Algorithm, HashAlg, LineEnding, PrivateKey, PublicKey, SshSig};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// The namespace git uses for SSH commit signatures, so `git log --show-signature` verifies them.
const COMMIT_NAMESPACE: &str = "git";
/// A namespace of its own, so a commit signature never passes as a record signature.
const RECORD_NAMESPACE: &str = "tether-machine";

pub fn key_path() -> Result<PathBuf> {
    Ok(crate::home_dir()?.join(".tether").join("signing_key"))
}

/// Load this machine's signing key, or create it and trust it.
pub fn load_or_create(machine_id: &str) -> Result<PrivateKey> {
    let path = key_path()?;
    if path.exists() {
        return PrivateKey::from_openssh(std::fs::read(&path)?)
            .with_context(|| format!("Invalid signing key {}", path.display()));
    }
    let key = PrivateKey::random(&mut OsRng, Algorithm::Ed25519)?;
    std::fs::create_dir_all(path.parent().unwrap())?;
    crate::security::write_owner_only(&path, key.to_openssh(LineEnding::LF)?.as_bytes())?;
    crate::packages::inbox::Inbox::update(|_| {
        let mut store = TrustStore::load()?;
        store.trust(machine_id, key.public_key())?;
        store.save()
    })?;
    Ok(key)
}

fn existing_public_key() -> Option<PublicKey> {
    key_path()
        .ok()
        .and_then(|p| std::fs::read(p).ok())
        .and_then(|b| PrivateKey::from_openssh(b).ok())
        .map(|k| k.public_key().clone())
}

/// The fingerprint of this machine's signing key, read from the key file. None before
/// Tether made the key.
pub fn own_fingerprint() -> Option<String> {
    existing_public_key().map(|k| fingerprint(&k))
}

pub fn fingerprint(key: &PublicKey) -> String {
    key.fingerprint(HashAlg::Sha256).to_string()
}

/// An armored SSH signature over a commit buffer, as git stores it in `gpgsig`.
pub fn sign_commit(key: &PrivateKey, data: &[u8]) -> Result<String> {
    Ok(key
        .sign(COMMIT_NAMESPACE, HashAlg::Sha512, data)?
        .to_pem(LineEnding::LF)?)
}

fn record_path(sync_path: &Path, machine_id: &str) -> PathBuf {
    sync_path
        .join("machines")
        .join(format!("{}.json", machine_id))
}

pub fn record_sig_path(sync_path: &Path, machine_id: &str) -> PathBuf {
    sync_path
        .join("machines")
        .join(format!("{}.json.sig", machine_id))
}

/// What a record signature covers: the machine id and the record's exact bytes, so a
/// signed record cannot pose as another machine's record.
fn record_message(machine_id: &str, record: &[u8]) -> Vec<u8> {
    format!(
        "tether-machine-v1\n{}\n{}\n",
        machine_id,
        crate::sha256_hex(record)
    )
    .into_bytes()
}

/// Sign the bytes of `machines/<machine_id>.json` as they are on disk.
pub fn sign_record(sync_path: &Path, machine_id: &str, key: &PrivateKey) -> Result<()> {
    let record = std::fs::read(record_path(sync_path, machine_id))?;
    let signature = key
        .sign(
            RECORD_NAMESPACE,
            HashAlg::Sha512,
            &record_message(machine_id, &record),
        )?
        .to_pem(LineEnding::LF)?;
    let path = record_sig_path(sync_path, machine_id);
    if std::fs::read_to_string(&path).ok().as_deref() != Some(signature.as_str()) {
        crate::sync::atomic_write(&path, signature.as_bytes())?;
    }
    // Keys now travel inside record signatures, so the separate public key file is stale
    let _ = std::fs::remove_file(
        sync_path
            .join("machines")
            .join(format!("{}.pub", machine_id)),
    );
    Ok(())
}

/// Write this machine's record, sign it, and keep a local copy. The signature vouches for
/// the record's packages, so build the record with [`own_record`] and packages just read
/// from this machine's managers.
/// The caller holds the sync lock from its [`own_record`] read through this save, so no
/// other writer saves in between and each generation is signed once.
pub fn save_record(sync_path: &Path, record: &MachineState) -> Result<()> {
    let key = load_or_create(&record.machine_id)?;
    let mut generations = Generations::load()?;
    save_record_at(
        sync_path,
        record,
        &local_record_path()?,
        &key,
        &mut generations,
    )?;
    generations.save()
}

/// Move this machine's record from `old` to `new`: build it from [`own_record`], save and
/// sign it under the new id, and remove the old record and signature. A signature binds the
/// id, so a record is never moved without signing it again. The caller holds the sync lock.
pub fn rename_own_record(sync_path: &Path, old: &str, new: &str) -> Result<()> {
    let key = load_or_create(old)?;
    let mut generations = Generations::load()?;
    rename_record_at(
        sync_path,
        old,
        new,
        &local_record_path()?,
        &key,
        &mut generations,
    )?;
    generations.save()
}

fn rename_record_at(
    sync_path: &Path,
    old: &str,
    new: &str,
    local: &Path,
    key: &PrivateKey,
    generations: &mut Generations,
) -> Result<()> {
    let mut record = own_record_from(local, sync_path, old, key.public_key(), generations)?
        .unwrap_or_else(|| MachineState::new(new));
    record.machine_id = new.to_string();
    save_record_at(sync_path, &record, local, key, generations)?;
    for path in [
        record_path(sync_path, old),
        record_sig_path(sync_path, old),
        sync_path.join("machines").join(format!("{}.pub", old)),
    ] {
        if path.exists() {
            std::fs::remove_file(path)?;
        }
    }
    Ok(())
}

fn save_record_at(
    sync_path: &Path,
    record: &MachineState,
    local: &Path,
    key: &PrivateKey,
    generations: &mut Generations,
) -> Result<()> {
    // Other machines distrust a record that validation would change
    let mut record = record.clone();
    record.validate()?;
    let local_bytes = std::fs::read(local).ok();
    let previous = local_bytes
        .as_deref()
        .and_then(|bytes| serde_json::from_slice::<MachineState>(bytes).ok());
    if let (Some(bytes), Some(previous)) = (&local_bytes, &previous) {
        let published = std::fs::read(record_path(sync_path, &record.machine_id)).ok();
        if published.as_ref() == Some(bytes)
            && file_signer(sync_path, &record.machine_id)
                .is_some_and(|k| k.key_data() == key.public_key().key_data())
            && !record_due(previous, &record)
        {
            return Ok(());
        }
    }
    // Count from the newest saved generation, not the caller's copy, which can be older
    let saved = previous.map_or(0, |r| r.generation);
    record.generation = record
        .generation
        .max(saved)
        .max(generations.last(key.public_key()).unwrap_or(0))
        .saturating_add(1);
    record.save_to_repo(sync_path)?;
    sign_record(sync_path, &record.machine_id, key)?;
    let bytes = std::fs::read(record_path(sync_path, &record.machine_id))?;
    generations.accept(
        key.public_key(),
        record.generation,
        &crate::sha256_hex(&bytes),
    );
    crate::sync::atomic_write(local, &bytes)
}

/// Whether `record` must replace the published `previous` one: something besides
/// `last_sync` changed, or the heartbeat is due. Saving only then keeps an idle machine
/// from committing and signing a new record on every daemon tick.
fn record_due(previous: &MachineState, record: &MachineState) -> bool {
    let content = |r: &MachineState| {
        let mut value = serde_json::to_value(r).ok();
        if let Some(serde_json::Value::Object(map)) = &mut value {
            map.remove("last_sync");
            map.remove("generation");
        }
        value
    };
    content(previous) != content(record)
        || record.last_sync.signed_duration_since(previous.last_sync)
            >= chrono::Duration::minutes(crate::sync::state::RECORD_HEARTBEAT_MINUTES)
}

/// The newest record generation this machine has accepted from each machine key, in
/// `~/.tether/record_generations.json`, with the SHA-256 of the record it accepted at that
/// generation. It is never synced. A record signed by the same key with a lower generation,
/// or a different record with the same generation, is a replay and does not count.
#[derive(Debug, Default, Serialize, Deserialize)]
pub struct Generations {
    seen: BTreeMap<String, u64>,
    /// Earlier builds stored no digests; the next accepted record adds one
    #[serde(default)]
    digests: BTreeMap<String, String>,
}

impl Generations {
    fn path() -> Result<PathBuf> {
        Ok(crate::home_dir()?
            .join(".tether")
            .join("record_generations.json"))
    }

    pub fn load() -> Result<Self> {
        let path = Self::path()?;
        if !path.exists() {
            return Ok(Self::default());
        }
        serde_json::from_slice(&std::fs::read(&path)?)
            .with_context(|| format!("Invalid record generations {}", path.display()))
    }

    pub fn save(&self) -> Result<()> {
        crate::sync::atomic_write(&Self::path()?, &serde_json::to_vec_pretty(self)?)
    }

    /// The newest generation accepted from `key`.
    pub fn last(&self, key: &PublicKey) -> Option<u64> {
        self.seen.get(&fingerprint(key)).copied()
    }

    /// Whether a record `key` signed at `generation`, with SHA-256 `digest`, is newer than
    /// the one accepted before, or is that same record. A new key starts from its first
    /// record, so a reinstalled machine is not refused.
    pub fn current(&self, key: &PublicKey, generation: u64, digest: &str) -> bool {
        let fp = fingerprint(key);
        self.seen.get(&fp).is_none_or(|&seen| {
            generation > seen
                || (generation == seen && self.digests.get(&fp).is_none_or(|d| d == digest))
        })
    }

    /// Accept the record if it is [`Self::current`].
    pub fn accept(&mut self, key: &PublicKey, generation: u64, digest: &str) -> bool {
        if !self.current(key, generation, digest) {
            return false;
        }
        let fp = fingerprint(key);
        self.seen.insert(fp.clone(), generation);
        self.digests.insert(fp, digest.to_string());
        true
    }
}

/// This machine's copy of its last record. Anyone who can push writes the repo copy, so
/// removals and ignores this machine keeps come only from here.
fn local_record_path() -> Result<PathBuf> {
    Ok(crate::home_dir()?.join(".tether").join("machine.json"))
}

/// Save this machine's local copy of its record only. The next sync builds its record from
/// it, then signs and publishes that record.
pub fn save_local_record(record: &MachineState) -> Result<()> {
    let mut record = record.clone();
    record.validate()?;
    let content = serde_json::to_string_pretty(&serde_json::to_value(&record)?)?;
    crate::sync::atomic_write(&local_record_path()?, content.as_bytes())
}

/// This machine's last saved record, the base for its next one.
pub fn own_record(sync_path: &Path, machine_id: &str) -> Result<Option<MachineState>> {
    let key = load_or_create(machine_id)?;
    own_record_from(
        &local_record_path()?,
        sync_path,
        machine_id,
        key.public_key(),
        &Generations::load()?,
    )
}

/// The local copy when there is one. Without it, the repo copy counts when this machine's
/// key signed it and it is not older than the last record this machine saved or accepted.
/// Before this machine's first save, an unsigned repo copy counts too, because an earlier
/// build wrote it: that first upgrade trusts the repo as earlier builds did. A generation
/// accepted from this machine's key marks that the upgrade is done.
fn own_record_from(
    local: &Path,
    sync_path: &Path,
    machine_id: &str,
    own_key: &PublicKey,
    generations: &Generations,
) -> Result<Option<MachineState>> {
    if local.exists() {
        let mut record: MachineState = serde_json::from_slice(&std::fs::read(local)?)
            .with_context(|| format!("Invalid machine record {}", local.display()))?;
        record.validate()?;
        // A renamed machine keeps its record
        record.machine_id = machine_id.to_string();
        return Ok(Some(record));
    }
    let Some(found) = records(sync_path)
        .into_iter()
        .find(|r| r.record.machine_id == machine_id)
    else {
        return Ok(None);
    };
    let own = found
        .signer
        .is_some_and(|k| k.key_data() == own_key.key_data());
    if own && !generations.current(own_key, found.record.generation, &found.digest) {
        Output::warning(&format!(
            "machines/{}.json is older than this machine's last record. Tether rebuilds this \
             machine's record from local state",
            machine_id
        ));
        return Ok(None);
    }
    let legacy =
        generations.last(own_key).is_none() && !record_sig_path(sync_path, machine_id).exists();
    if !own && !legacy {
        Output::warning(&format!(
            "machines/{}.json is not signed by this machine. Tether rebuilds this machine's \
             record from local state",
            machine_id
        ));
    }
    Ok((own || legacy).then_some(found.record))
}

/// The ed25519 key whose signature over `record` as `machine_id`'s record verifies, or None.
fn record_signer(machine_id: &str, record: &[u8], signature: &str) -> Option<PublicKey> {
    let sig = SshSig::from_pem(signature).ok()?;
    let key = PublicKey::from(sig.public_key().clone());
    if key.algorithm() != Algorithm::Ed25519 {
        return None;
    }
    key.verify(RECORD_NAMESPACE, &record_message(machine_id, record), &sig)
        .ok()?;
    Some(key)
}

/// A machine record from the sync repo, and the key whose signature over it verifies.
/// The signer is not trusted yet: compare it with the trust store.
#[derive(Debug, Clone)]
pub struct SignedRecord {
    pub record: MachineState,
    pub signer: Option<PublicKey>,
    /// SHA-256 of the record file
    pub digest: String,
}

/// Every machine record whose file name is a valid machine id and equals the id inside it.
/// The signature is checked over the same bytes that are parsed.
pub fn records(sync_path: &Path) -> Vec<SignedRecord> {
    let Ok(entries) = std::fs::read_dir(sync_path.join("machines")) else {
        return Vec::new();
    };
    let mut records: Vec<SignedRecord> = entries
        .flatten()
        .filter_map(|entry| {
            let name = entry.file_name().into_string().ok()?;
            let id = name.strip_suffix(".json")?;
            if !valid_machine_id(id) {
                Output::warning(&format!("Ignoring machines/{}: invalid machine id", name));
                return None;
            }
            let bytes = std::fs::read(entry.path()).ok()?;
            let signed: MachineState = serde_json::from_slice(&bytes).ok()?;
            let mut record = signed.clone();
            if record.machine_id != id || record.validate().is_err() {
                return None;
            }
            // The signature covers the record as written. Validation drops invalid entries,
            // so a record it changed would vouch for something nobody signed, such as an
            // unpinned name where the signer wrote a range.
            let signer = if serde_json::to_value(&record).ok() == serde_json::to_value(&signed).ok()
            {
                std::fs::read_to_string(record_sig_path(sync_path, id))
                    .ok()
                    .and_then(|sig| record_signer(id, &bytes, &sig))
            } else {
                Output::warning(&format!(
                    "Not trusting machines/{}.json: it has entries Tether drops",
                    name
                ));
                None
            };
            Some(SignedRecord {
                record,
                signer,
                digest: crate::sha256_hex(&bytes),
            })
        })
        .collect();
    records.sort_by(|a, b| a.record.machine_id.cmp(&b.record.machine_id));
    records
}

/// How this machine reads a machine record from the sync repo.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecordStatus {
    /// Signed by the key trusted for its machine id, and the newest record that key signed
    Trusted,
    /// Its machine id has no trusted key
    Untrusted,
    /// Signed by the trusted key, but that key signed a newer or different record before
    Replayed,
    /// Its machine id has a trusted key, and no signature by that key verifies
    SignatureFailed,
}

impl RecordStatus {
    pub fn label(self) -> &'static str {
        match self {
            RecordStatus::Trusted => "trusted",
            RecordStatus::Untrusted => "untrusted",
            RecordStatus::Replayed => "replayed (ignored)",
            RecordStatus::SignatureFailed => "signature failed (ignored)",
        }
    }
}

/// Each record's machine id, status as a sync reads it, and the fingerprint of the key whose
/// signature over it verifies.
pub fn record_statuses(
    sync_path: &Path,
    this_machine: &str,
) -> Result<Vec<(String, RecordStatus, Option<String>)>> {
    // Listing must not create a key; a machine without one has no signed record yet
    let own_key = existing_public_key();
    let store = TrustStore::load()?;
    let generations = Generations::load()?;
    Ok(records(sync_path)
        .iter()
        .map(|r| {
            (
                r.record.machine_id.clone(),
                record_status(r, this_machine, own_key.as_ref(), &store, &generations),
                r.signer.as_ref().map(fingerprint),
            )
        })
        .collect())
}

/// What a machine on 1.x means during a rolling upgrade, in one line.
pub const OLD_BUILD_NOTE: &str = "on 1.x: installs without the new checks; its packages wait in \
                                  the inbox until it upgrades and you trust it";

/// Other machines whose record names a build before 2.0, or no version. Such a machine
/// installs synced packages without the trust, release-age and OSV checks. The signature
/// does not count here: a 2.x record that fails its signature is not a 1.x machine, and
/// `record_statuses` reports its signature state.
pub fn old_builds(machines: &[MachineState], this_machine: &str) -> Vec<String> {
    machines
        .iter()
        .filter(|m| m.machine_id != this_machine)
        .filter(|m| {
            m.cli_version
                .split('.')
                .next()
                .and_then(|v| v.parse::<u64>().ok())
                .is_none_or(|major| major < 2)
        })
        .map(|m| m.machine_id.clone())
        .collect()
}

/// The status of `record`. This machine's own id is trusted with `own_key` only.
pub fn record_status(
    record: &SignedRecord,
    this_machine: &str,
    own_key: Option<&PublicKey>,
    store: &TrustStore,
    generations: &Generations,
) -> RecordStatus {
    let id = &record.record.machine_id;
    let trusted = if id == this_machine {
        match own_key {
            Some(key) => key,
            None => return RecordStatus::Untrusted,
        }
    } else {
        match store.key_for(id) {
            Some(key) => key,
            None => return RecordStatus::Untrusted,
        }
    };
    match &record.signer {
        Some(signer) if signer.key_data() == trusted.key_data() => {
            if generations.current(signer, record.record.generation, &record.digest) {
                RecordStatus::Trusted
            } else {
                RecordStatus::Replayed
            }
        }
        _ => RecordStatus::SignatureFailed,
    }
}

/// The keys that validly signed each machine record, trusted or not.
pub fn record_signers(sync_path: &Path) -> Vec<(String, PublicKey)> {
    records(sync_path)
        .into_iter()
        .filter_map(|r| Some((r.record.machine_id, r.signer?)))
        .collect()
}

/// A record that may be an earlier id of this machine, and the SHA-256 of its file.
#[derive(Debug, Clone, PartialEq)]
pub struct OldId {
    pub machine_id: String,
    pub digest: String,
}

/// The records in `machines` that may be an earlier id of this machine. Reading them must
/// not create a signing key, so without one every signed record counts as another machine's.
pub fn old_ids_of_this_machine(
    sync_path: &Path,
    machines: &[MachineState],
    this_id: &str,
) -> Vec<OldId> {
    let own = existing_public_key();
    old_ids_at(
        sync_path,
        machines,
        this_id,
        &crate::sync::local_hostname(),
        own.as_ref(),
        chrono::Utc::now(),
    )
}

fn old_ids_at(
    sync_path: &Path,
    machines: &[MachineState],
    this_id: &str,
    host: &str,
    own: Option<&PublicKey>,
    now: chrono::DateTime<chrono::Utc>,
) -> Vec<OldId> {
    // Who signed the bytes decides, not whether validation keeps the signer: a record that
    // another key signed belongs to that machine even when it has entries Tether drops.
    let signed_by_other_keys: Vec<String> =
        MachineState::old_ids_of_this_machine(machines, this_id, host, &[], now)
            .into_iter()
            .filter(|m| {
                file_signer(sync_path, &m.machine_id)
                    .is_some_and(|k| own.map(|o| o.key_data()) != Some(k.key_data()))
            })
            .map(|m| m.machine_id.clone())
            .collect();
    // The digest must cover the record as `machines` shows it, so a file that changed since
    // the list was read is left out rather than vouched for
    MachineState::old_ids_of_this_machine(machines, this_id, host, &signed_by_other_keys, now)
        .into_iter()
        .filter_map(|m| {
            let bytes = std::fs::read(record_path(sync_path, &m.machine_id)).ok()?;
            let mut on_disk: MachineState = serde_json::from_slice(&bytes).ok()?;
            on_disk.validate().ok()?;
            (serde_json::to_value(&on_disk).ok()? == serde_json::to_value(m).ok()?).then(|| OldId {
                machine_id: m.machine_id.clone(),
                digest: crate::sha256_hex(&bytes),
            })
        })
        .collect()
}

/// The key whose signature over `machine_id`'s record file verifies, whatever the record holds.
fn file_signer(sync_path: &Path, machine_id: &str) -> Option<PublicKey> {
    let bytes = std::fs::read(record_path(sync_path, machine_id)).ok()?;
    let signature = std::fs::read_to_string(record_sig_path(sync_path, machine_id)).ok()?;
    record_signer(machine_id, &bytes, &signature)
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct TrustFile {
    version: u32,
    #[serde(default)]
    machines: BTreeMap<String, TrustEntry>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct TrustEntry {
    /// OpenSSH public key line, without a comment
    public_key: String,
    /// `SHA256:...` of `public_key`, checked on load
    fingerprint: String,
}

/// Machine keys this machine trusts, in `~/.tether/trusted_keys` as TOML. It is never
/// synced: trust is a decision about this machine. Only Tether writes it, so any entry
/// that does not parse makes loading fail.
/// Change it only inside [`crate::packages::inbox::Inbox::update`], which holds the lock.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct TrustStore {
    pub keys: Vec<(String, PublicKey)>,
}

impl TrustStore {
    pub fn path() -> Result<PathBuf> {
        Ok(crate::home_dir()?.join(".tether").join("trusted_keys"))
    }

    pub fn load() -> Result<Self> {
        let path = Self::path()?;
        if !path.exists() {
            return Ok(Self::default());
        }
        Self::parse(&std::fs::read_to_string(&path)?)
            .with_context(|| format!("Invalid trust store {}", path.display()))
    }

    fn parse(content: &str) -> Result<Self> {
        match toml::from_str::<TrustFile>(content) {
            Ok(file) => Self::from_file(file),
            // Earlier builds wrote git's allowed signers format; the next save rewrites it as TOML
            Err(_) if Self::is_allowed_signers(content) => Ok(Self::from_allowed_signers(content)),
            Err(e) => Err(e.into()),
        }
    }

    fn from_file(file: TrustFile) -> Result<Self> {
        if file.version != 1 {
            bail!("unsupported version {}", file.version);
        }
        let mut store = Self::default();
        for (id, entry) in file.machines {
            let key = PublicKey::from_openssh(&entry.public_key)
                .map_err(|e| anyhow!("machine {}: {}", id, e))?;
            if fingerprint(&key) != entry.fingerprint {
                bail!("machine {}: fingerprint does not match its key", id);
            }
            store.trust(&id, &key)?;
        }
        Ok(store)
    }

    fn is_allowed_signers(content: &str) -> bool {
        content.lines().filter(|l| !l.trim().is_empty()).all(|l| {
            l.split_whitespace()
                .nth(1)
                .is_some_and(|t| t.starts_with("ssh-"))
        })
    }

    /// Only lines exactly as earlier builds wrote them: `<machine id> ssh-ed25519 <key>`.
    fn from_allowed_signers(content: &str) -> Self {
        let mut store = Self::default();
        for line in content.lines().filter(|l| !l.trim().is_empty()) {
            let fields: Vec<&str> = line.split_whitespace().collect();
            let migrated = match fields.as_slice() {
                [id, "ssh-ed25519", data] => {
                    PublicKey::from_openssh(&format!("ssh-ed25519 {}", data))
                        .ok()
                        .is_some_and(|key| store.trust(id, &key).is_ok())
                }
                _ => false,
            };
            if !migrated {
                Output::warning(&format!("Dropping trusted key line '{}'", line));
            }
        }
        store
    }

    fn render(&self) -> Result<String> {
        let mut machines = BTreeMap::new();
        for (id, key) in &self.keys {
            machines.insert(
                id.clone(),
                TrustEntry {
                    public_key: key.to_openssh()?,
                    fingerprint: fingerprint(key),
                },
            );
        }
        Ok(toml::to_string(&TrustFile {
            version: 1,
            machines,
        })?)
    }

    pub fn save(&self) -> Result<()> {
        let path = Self::path()?;
        std::fs::create_dir_all(path.parent().unwrap())?;
        crate::security::write_owner_only(&path, self.render()?.as_bytes())
    }

    /// The machine id a trusted key belongs to.
    pub fn machine_for(&self, key: &PublicKey) -> Option<&str> {
        self.keys
            .iter()
            .find(|(_, k)| k.key_data() == key.key_data())
            .map(|(id, _)| id.as_str())
    }

    pub fn key_for(&self, machine_id: &str) -> Option<&PublicKey> {
        self.keys
            .iter()
            .find(|(id, _)| id == machine_id)
            .map(|(_, k)| k)
    }

    /// True when `key` is the key trusted for `machine_id`.
    pub fn trusts(&self, machine_id: &str, key: &PublicKey) -> bool {
        self.key_for(machine_id)
            .is_some_and(|k| k.key_data() == key.key_data())
    }

    /// Trust the ed25519 `key` as `machine_id`, replacing any other key that id had and
    /// any other id that key had.
    pub fn trust(&mut self, machine_id: &str, key: &PublicKey) -> Result<()> {
        if !valid_machine_id(machine_id) {
            bail!("Invalid machine id '{}'", machine_id);
        }
        if key.algorithm() != Algorithm::Ed25519 {
            bail!("Machine {} key is not ed25519", machine_id);
        }
        self.keys
            .retain(|(id, k)| id != machine_id && k.key_data() != key.key_data());
        let key = PublicKey::new(key.key_data().clone(), "");
        self.keys.push((machine_id.to_string(), key));
        Ok(())
    }

    /// Returns false when the machine had no trusted key.
    pub fn untrust(&mut self, machine_id: &str) -> bool {
        let before = self.keys.len();
        self.keys.retain(|(id, _)| id != machine_id);
        self.keys.len() != before
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sync::state::RECORD_HEARTBEAT_MINUTES;

    fn key() -> PrivateKey {
        PrivateKey::random(&mut OsRng, Algorithm::Ed25519).unwrap()
    }

    #[test]
    fn old_builds_go_by_version_not_signature() {
        let machine = |id: &str, version: &str| MachineState {
            cli_version: version.to_string(),
            ..MachineState::new(id)
        };
        // "unsigned" and "tampered" have no signature that verifies, and claim 2.x
        let machines = [
            machine("this", "1.13.1"),
            machine("old", "1.13.1"),
            machine("blank", ""),
            machine("beta", "2.0.0-beta.1"),
            machine("unsigned", "2.0.0"),
            machine("tampered", "2.1.0"),
        ];
        assert_eq!(old_builds(&machines, "this"), ["old", "blank"]);
    }

    fn write_record(dir: &Path, file_id: &str, record: &MachineState) {
        std::fs::create_dir_all(dir.join("machines")).unwrap();
        std::fs::write(
            record_path(dir, file_id),
            serde_json::to_string_pretty(record).unwrap(),
        )
        .unwrap();
    }

    fn signer_of(dir: &Path, id: &str) -> Option<PublicKey> {
        records(dir)
            .into_iter()
            .find(|r| r.record.machine_id == id)
            .and_then(|r| r.signer)
    }

    #[test]
    fn record_sign_verify_roundtrip() {
        let tmp = tempfile::TempDir::new().unwrap();
        let key = key();
        write_record(tmp.path(), "m1", &MachineState::new("m1"));
        std::fs::write(tmp.path().join("machines/m1.pub"), "stale").unwrap();
        sign_record(tmp.path(), "m1", &key).unwrap();
        let sig = std::fs::read_to_string(record_sig_path(tmp.path(), "m1")).unwrap();
        assert!(sig.starts_with("-----BEGIN SSH SIGNATURE-----"));
        assert!(!tmp.path().join("machines/m1.pub").exists());
        let signer = signer_of(tmp.path(), "m1").unwrap();
        assert_eq!(signer.key_data(), key.public_key().key_data());

        // A commit signature over the same message is in another namespace
        let bytes = std::fs::read(record_path(tmp.path(), "m1")).unwrap();
        let commit_sig = sign_commit(&key, &record_message("m1", &bytes)).unwrap();
        assert!(record_signer("m1", &bytes, &commit_sig).is_none());
    }

    #[test]
    fn record_status_reads_records_as_a_sync_does() {
        let tmp = tempfile::TempDir::new().unwrap();
        let dir = tmp.path();
        let (me, t, other) = (key(), key(), key());
        let mut store = TrustStore::default();
        store.trust("t", t.public_key()).unwrap();
        store.trust("u", other.public_key()).unwrap();
        let signed = |id: &str, generation: u64, key: &PrivateKey| {
            let mut record = MachineState::new(id);
            record.generation = generation;
            write_record(dir, id, &record);
            sign_record(dir, id, key).unwrap();
        };
        let status = |id: &str, generations: &Generations| {
            let r = records(dir)
                .into_iter()
                .find(|r| r.record.machine_id == id)
                .unwrap();
            record_status(&r, "me", Some(me.public_key()), &store, generations)
        };
        let mut generations = Generations::default();
        signed("me", 1, &me);
        signed("t", 5, &t);
        signed("u", 1, &t);
        signed("stranger", 1, &other);
        assert_eq!(status("me", &generations), RecordStatus::Trusted);
        assert_eq!(status("t", &generations), RecordStatus::Trusted);
        assert_eq!(status("u", &generations), RecordStatus::SignatureFailed);
        assert_eq!(status("stranger", &generations), RecordStatus::Untrusted);
        // An older record by the same key, restored from git history
        generations.accept(t.public_key(), 6, "newer");
        assert_eq!(status("t", &generations), RecordStatus::Replayed);
        std::fs::remove_file(record_sig_path(dir, "t")).unwrap();
        assert_eq!(status("t", &generations), RecordStatus::SignatureFailed);
    }

    #[test]
    fn tampered_record_is_unsigned() {
        let tmp = tempfile::TempDir::new().unwrap();
        let mut record = MachineState::new("m1");
        write_record(tmp.path(), "m1", &record);
        sign_record(tmp.path(), "m1", &key()).unwrap();
        record
            .packages
            .insert("npm".to_string(), vec!["evil".to_string()]);
        write_record(tmp.path(), "m1", &record);
        assert!(signer_of(tmp.path(), "m1").is_none());
    }

    #[test]
    fn older_generation_from_the_same_key_is_refused() {
        let (a, b) = (key(), key());
        let mut seen = Generations::default();
        assert!(seen.accept(a.public_key(), 5, "r5"));
        assert!(seen.accept(a.public_key(), 5, "r5"));
        assert!(seen.accept(a.public_key(), 7, "r7"));
        assert!(!seen.accept(a.public_key(), 6, "r6"));
        assert!(seen.accept(b.public_key(), 1, "b1"));
    }

    #[test]
    fn different_record_at_the_accepted_generation_is_refused() {
        let a = key();
        let mut seen = Generations::default();
        assert!(seen.accept(a.public_key(), 5, "first"));
        assert!(!seen.accept(a.public_key(), 5, "second"));
        assert!(seen.accept(a.public_key(), 5, "first"));

        // A generation from an earlier build has no digest; the next record sets one
        let mut legacy: Generations = serde_json::from_value(
            serde_json::json!({ "seen": { fingerprint(a.public_key()): 5 } }),
        )
        .unwrap();
        assert!(legacy.accept(a.public_key(), 5, "first"));
        assert!(!legacy.accept(a.public_key(), 5, "second"));
    }

    #[test]
    fn writers_from_a_stale_copy_never_reuse_a_generation() {
        let tmp = tempfile::TempDir::new().unwrap();
        let (dir, local) = (tmp.path(), tmp.path().join("machine.json"));
        std::fs::create_dir_all(dir.join("machines")).unwrap();
        let me = key();
        let mut seen = Generations::default();
        let stale = MachineState::new("me");
        let generation = |seen: &mut Generations, record: &MachineState| {
            save_record_at(dir, record, &local, &me, seen).unwrap();
            records(dir)[0].record.generation
        };
        assert_eq!(generation(&mut seen, &stale), 1);
        // A second writer that read its copy before the first saved
        let mut other = stale.clone();
        other.ignored_dotfiles.push(".zshrc".to_string());
        assert_eq!(generation(&mut seen, &other), 2);
        // Without the local copy, the accepted generation still counts
        std::fs::remove_file(&local).unwrap();
        assert_eq!(generation(&mut seen, &stale), 3);
    }

    #[test]
    fn a_record_that_only_synced_again_waits_for_the_heartbeat() {
        let tmp = tempfile::TempDir::new().unwrap();
        let (dir, local) = (tmp.path(), tmp.path().join("machine.json"));
        std::fs::create_dir_all(dir.join("machines")).unwrap();
        let me = key();
        let mut seen = Generations::default();
        let mut record = MachineState::new("me");
        record
            .packages
            .insert("npm".to_string(), vec!["a".to_string()]);
        record
            .packages
            .insert("brew_formulae".to_string(), vec!["b".to_string()]);
        let mut save = |record: &MachineState| {
            save_record_at(dir, record, &local, &me, &mut seen).unwrap();
            let r = &records(dir)[0];
            (r.record.generation, r.record.last_sync, r.digest.clone())
        };
        let first = save(&record);
        assert_eq!(first.0, 1);

        // A daemon tick: only last_sync moved, so nothing is written or signed again
        let mut tick = record.clone();
        tick.last_sync += chrono::Duration::minutes(5);
        assert_eq!(save(&tick), first);

        // The same content built in another map order writes the same bytes
        let mut reordered = MachineState::new("me");
        reordered.last_sync = record.last_sync;
        reordered
            .packages
            .insert("brew_formulae".to_string(), vec!["b".to_string()]);
        reordered
            .packages
            .insert("npm".to_string(), vec!["a".to_string()]);
        assert_eq!(save(&reordered), first);

        // A real change saves at once
        let mut changed = tick.clone();
        changed
            .packages
            .get_mut("npm")
            .unwrap()
            .push("c".to_string());
        let second = save(&changed);
        assert_eq!((second.0, second.1), (2, changed.last_sync));

        // The heartbeat saves an unchanged record once an hour
        let mut hour = changed.clone();
        hour.last_sync += chrono::Duration::minutes(RECORD_HEARTBEAT_MINUTES);
        assert_eq!(save(&hour).0, 3);

        // A published copy that no longer matches is written and signed again
        std::fs::remove_file(record_sig_path(dir, "me")).unwrap();
        let mut tick = hour.clone();
        tick.last_sync += chrono::Duration::minutes(5);
        assert_eq!(save(&tick).0, 4);
        assert!(records(dir)[0].signer.is_some());
    }

    #[test]
    fn own_record_comes_from_the_local_copy() {
        let tmp = tempfile::TempDir::new().unwrap();
        let (dir, local) = (tmp.path(), tmp.path().join("machine.json"));
        let me = key();
        let fresh = Generations::default();
        let mut mine = MachineState::new("me");
        mine.removed_packages
            .insert("npm".to_string(), vec!["old".to_string()]);
        std::fs::write(&local, serde_json::to_vec(&mine).unwrap()).unwrap();

        // A repo writer injects removals and ignores into the repo copy
        let mut injected = mine.clone();
        injected
            .removed_packages
            .insert("npm".to_string(), vec!["needed".to_string()]);
        injected.ignored_dotfiles.push(".zshrc".to_string());
        write_record(dir, "me", &injected);
        let own = own_record_from(&local, dir, "me", me.public_key(), &fresh)
            .unwrap()
            .unwrap();
        assert_eq!(own.removed_packages["npm"], vec!["old"]);
        assert!(own.ignored_dotfiles.is_empty());
        let renamed = own_record_from(&local, dir, "me2", me.public_key(), &fresh)
            .unwrap()
            .unwrap();
        assert_eq!(renamed.machine_id, "me2");

        // Without a local copy, a repo copy signed by another key is not used
        let none = tmp.path().join("none.json");
        sign_record(dir, "me", &key()).unwrap();
        assert!(own_record_from(&none, dir, "me", me.public_key(), &fresh)
            .unwrap()
            .is_none());
        sign_record(dir, "me", &me).unwrap();
        assert!(own_record_from(&none, dir, "me", me.public_key(), &fresh)
            .unwrap()
            .is_some());
        // An unsigned copy from an earlier build is used once, on the upgrade
        std::fs::remove_file(record_sig_path(dir, "me")).unwrap();
        assert!(own_record_from(&none, dir, "me", me.public_key(), &fresh)
            .unwrap()
            .is_some());
    }

    #[test]
    fn repo_own_record_after_the_upgrade_must_be_signed_and_current() {
        let tmp = tempfile::TempDir::new().unwrap();
        let dir = tmp.path();
        std::fs::create_dir_all(dir.join("machines")).unwrap();
        let (local, none) = (dir.join("machine.json"), dir.join("none.json"));
        let me = key();
        let mut seen = Generations::default();
        let record = MachineState::new("me");
        save_record_at(dir, &record, &local, &me, &mut seen).unwrap();
        let old = std::fs::read(record_path(dir, "me")).unwrap();
        let old_sig = std::fs::read(record_sig_path(dir, "me")).unwrap();
        let mut later = record.clone();
        later.last_sync += chrono::Duration::hours(2);
        save_record_at(dir, &later, &local, &me, &mut seen).unwrap();
        let recover = |seen: &Generations| {
            own_record_from(&none, dir, "me", me.public_key(), seen)
                .unwrap()
                .map(|r| r.generation)
        };
        assert_eq!(recover(&seen), Some(2));

        // An older copy that this machine's key signed, restored from git history
        std::fs::write(record_path(dir, "me"), &old).unwrap();
        std::fs::write(record_sig_path(dir, "me"), &old_sig).unwrap();
        assert_eq!(recover(&seen), None);
        // The same copy without its signature no longer passes as an earlier build's
        std::fs::remove_file(record_sig_path(dir, "me")).unwrap();
        assert_eq!(recover(&seen), None);
        assert_eq!(recover(&Generations::default()), Some(1));
    }

    #[test]
    fn rename_signs_this_machines_record_under_the_new_id() {
        let tmp = tempfile::TempDir::new().unwrap();
        let dir = tmp.path();
        std::fs::create_dir_all(dir.join("machines")).unwrap();
        let (local, none) = (dir.join("machine.json"), dir.join("none.json"));
        let me = key();
        let mut seen = Generations::default();
        let mut record = MachineState::new("me");
        record.ignored_dotfiles.push(".zshrc".to_string());
        save_record_at(dir, &record, &local, &me, &mut seen).unwrap();

        // A repo writer injects an ignore; the rename reads the local copy, not the repo copy
        let mut injected = record.clone();
        injected.ignored_dotfiles.push(".ssh/config".to_string());
        write_record(dir, "me", &injected);
        rename_record_at(dir, "me", "me2", &local, &me, &mut seen).unwrap();
        assert!(!record_path(dir, "me").exists());
        assert!(!record_sig_path(dir, "me").exists());
        let found = records(dir);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].record.machine_id, "me2");
        assert_eq!(found[0].record.ignored_dotfiles, vec![".zshrc"]);
        assert_eq!(found[0].record.generation, 2);
        assert_eq!(
            found[0].signer.as_ref().unwrap().key_data(),
            me.public_key().key_data()
        );
        let own = own_record_from(&none, dir, "me2", me.public_key(), &seen).unwrap();
        assert_eq!(own.unwrap().generation, 2);

        // A renamed copy that nobody signed, as earlier builds wrote, is not this machine's
        std::fs::remove_file(record_sig_path(dir, "me2")).unwrap();
        assert!(own_record_from(&none, dir, "me2", me.public_key(), &seen)
            .unwrap()
            .is_none());
    }

    #[test]
    fn signed_record_that_validation_changes_is_unsigned() {
        let tmp = tempfile::TempDir::new().unwrap();
        let mut record = MachineState::new("m1");
        record
            .packages
            .insert("npm".to_string(), vec!["widget".to_string()]);
        record.package_versions.insert(
            "npm".to_string(),
            std::collections::HashMap::from([("widget".to_string(), "^1.2.3".to_string())]),
        );
        write_record(tmp.path(), "m1", &record);
        sign_record(tmp.path(), "m1", &key()).unwrap();
        let found = records(tmp.path());
        assert!(found[0].record.package_versions["npm"].is_empty());
        assert!(found[0].signer.is_none());
    }

    #[test]
    fn old_id_signed_by_another_key_is_excluded_even_when_validation_drops_its_signer() {
        let tmp = tempfile::TempDir::new().unwrap();
        let own = key();
        let now = chrono::Utc::now();
        let mut this = MachineState::new("3f9a1c2b4d5e");
        this.hostname = "mac.local".to_string();
        let mut twin = MachineState::new("mac.local");
        twin.hostname = "mac.local".to_string();
        twin.cli_version = "1.11.10".to_string();
        twin.last_sync = now - chrono::Duration::days(30);
        twin.package_versions.insert(
            "npm".to_string(),
            std::collections::HashMap::from([("widget".to_string(), "^1.2.3".to_string())]),
        );
        write_record(tmp.path(), "mac.local", &twin);
        let machines = MachineState::list_all(tmp.path()).unwrap();
        let machines = [this, machines[0].clone()];
        let found = |own: &PrivateKey| {
            old_ids_at(
                tmp.path(),
                &machines,
                "3f9a1c2b4d5e",
                "mac.local",
                Some(own.public_key()),
                now,
            )
        };

        let ids = |own: &PrivateKey| -> Vec<String> {
            found(own).into_iter().map(|o| o.machine_id).collect()
        };
        assert_eq!(ids(&own), ["mac.local"]);
        sign_record(tmp.path(), "mac.local", &own).unwrap();
        assert_eq!(ids(&own), ["mac.local"]);
        sign_record(tmp.path(), "mac.local", &key()).unwrap();
        assert!(signer_of(tmp.path(), "mac.local").is_none());
        assert!(found(&own).is_empty());
    }

    #[test]
    fn old_id_digest_covers_the_record_as_listed() {
        let tmp = tempfile::TempDir::new().unwrap();
        let now = chrono::Utc::now();
        let mut this = MachineState::new("3f9a1c2b4d5e");
        this.hostname = "mac.local".to_string();
        let mut old = MachineState::new("mac.local");
        old.hostname = "mac.local".to_string();
        old.cli_version = "1.11.10".to_string();
        old.last_sync = now - chrono::Duration::days(30);
        write_record(tmp.path(), "mac.local", &old);
        let found = |machines: &[MachineState]| {
            old_ids_at(tmp.path(), machines, "3f9a1c2b4d5e", "mac.local", None, now)
        };

        let listed = [this.clone(), old.clone()];
        let shown = found(&listed);
        assert_eq!(
            shown,
            [OldId {
                machine_id: "mac.local".to_string(),
                digest: crate::sha256_hex(
                    &std::fs::read(record_path(tmp.path(), "mac.local")).unwrap()
                ),
            }]
        );

        // A pull rewrites the record: a list read before it no longer matches the file
        old.packages
            .insert("npm".to_string(), vec!["widget".to_string()]);
        write_record(tmp.path(), "mac.local", &old);
        assert!(found(&listed).is_empty());
        let fresh = found(&[this, old]);
        assert_eq!(fresh.len(), 1);
        assert_ne!(fresh[0].digest, shown[0].digest);
    }

    #[test]
    fn record_signed_for_one_machine_does_not_verify_as_another() {
        let tmp = tempfile::TempDir::new().unwrap();
        let x = key();
        write_record(tmp.path(), "x", &MachineState::new("x"));
        sign_record(tmp.path(), "x", &x).unwrap();

        // X's record and signature copied to Y's file names
        std::fs::copy(record_path(tmp.path(), "x"), record_path(tmp.path(), "y")).unwrap();
        std::fs::copy(
            record_sig_path(tmp.path(), "x"),
            record_sig_path(tmp.path(), "y"),
        )
        .unwrap();
        assert!(records(tmp.path())
            .iter()
            .all(|r| r.record.machine_id != "y"));

        // A record for Y with X's signature over the same bytes, bound to X's id
        let y = MachineState::new("y");
        write_record(tmp.path(), "y", &y);
        let bytes = std::fs::read(record_path(tmp.path(), "y")).unwrap();
        let forged = x
            .sign(
                RECORD_NAMESPACE,
                HashAlg::Sha512,
                &record_message("x", &bytes),
            )
            .unwrap()
            .to_pem(LineEnding::LF)
            .unwrap();
        std::fs::write(record_sig_path(tmp.path(), "y"), forged).unwrap();
        assert!(signer_of(tmp.path(), "y").is_none());

        // Y's record that X's key signs as Y verifies, but the key is X's
        sign_record(tmp.path(), "y", &x).unwrap();
        let mut store = TrustStore::default();
        store.trust("x", x.public_key()).unwrap();
        assert!(!store.trusts("y", &signer_of(tmp.path(), "y").unwrap()));
    }

    #[test]
    fn records_with_invalid_ids_are_ignored() {
        let tmp = tempfile::TempDir::new().unwrap();
        let id = "zz ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIB";
        write_record(tmp.path(), id, &MachineState::new(id));
        sign_record(tmp.path(), id, &key()).unwrap();
        write_record(tmp.path(), "ok", &MachineState::new("ok"));
        let found = records(tmp.path());
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].record.machine_id, "ok");
    }

    #[test]
    fn trust_store_roundtrips_and_replaces_rotated_keys() {
        let (a, a2, b) = (key(), key(), key());
        let mut store = TrustStore::default();
        store.trust("a", a.public_key()).unwrap();
        store.trust("b.local", b.public_key()).unwrap();
        let rendered = store.render().unwrap();
        assert!(rendered.starts_with("version = 1"));
        let parsed = TrustStore::parse(&rendered).unwrap();
        assert_eq!(parsed, store);
        assert_eq!(parsed.machine_for(b.public_key()), Some("b.local"));

        store.trust("a", a2.public_key()).unwrap();
        assert_eq!(store.keys.len(), 2);
        assert!(store.machine_for(a.public_key()).is_none());
        assert!(store.trusts("a", a2.public_key()));

        assert!(store.untrust("a"));
        assert!(!store.untrust("a"));
        assert!(store.key_for("a").is_none());
        assert!(store.trust("bad id", a.public_key()).is_err());
    }

    #[test]
    fn trust_store_rejects_bad_entries() {
        let a = key();
        let line = a.public_key().to_openssh().unwrap();
        let entry = |id: &str, fp: &str| {
            format!(
                "version = 1\n[machines.\"{}\"]\npublic_key = \"{}\"\nfingerprint = \"{}\"\n",
                id, line, fp
            )
        };
        let fp = fingerprint(a.public_key());
        assert!(TrustStore::parse(&entry("a", &fp)).is_ok());
        assert!(TrustStore::parse(&entry("a", "SHA256:other")).is_err());
        assert!(TrustStore::parse(&entry("a b", &fp)).is_err());
        assert!(TrustStore::parse(&format!("{}extra = 1\n", entry("a", &fp))).is_err());
        assert!(TrustStore::parse("version = 2\n").is_err());
        assert!(TrustStore::parse("not toml at all").is_err());
    }

    #[test]
    fn allowed_signers_file_migrates_and_drops_injected_lines() {
        let (a, attacker) = (key(), key());
        let a_line = a.public_key().to_openssh().unwrap();
        let evil = attacker.public_key().to_openssh().unwrap();
        // Attack D: a published key file named `zz <attacker key>.pub` made this line
        let legacy = format!("a {}\nzz {} {}\n", a_line, evil, a_line);
        let store = TrustStore::parse(&legacy).unwrap();
        assert_eq!(store.keys.len(), 1);
        assert!(store.trusts("a", a.public_key()));
        assert!(store.machine_for(attacker.public_key()).is_none());
        assert_eq!(TrustStore::parse(&store.render().unwrap()).unwrap(), store);
    }
}

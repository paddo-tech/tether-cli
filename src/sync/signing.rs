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
    // Count from the newest saved generation, not the caller's copy, which can be older
    let saved = std::fs::read(local)
        .ok()
        .and_then(|bytes| serde_json::from_slice::<MachineState>(&bytes).ok())
        .map_or(0, |r| r.generation);
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
    pub fn accept(&mut self, key: &PublicKey, generation: u64, digest: &str) -> bool {
        let fp = fingerprint(key);
        if let Some(&seen) = self.seen.get(&fp) {
            let same_record = self.digests.get(&fp).is_none_or(|d| d == digest);
            if generation < seen || (generation == seen && !same_record) {
                return false;
            }
        }
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

/// This machine's last saved record, the base for its next one.
pub fn own_record(sync_path: &Path, machine_id: &str) -> Result<Option<MachineState>> {
    let key = load_or_create(machine_id)?;
    own_record_from(
        &local_record_path()?,
        sync_path,
        machine_id,
        key.public_key(),
    )
}

/// The local copy when there is one. Before the first save that writes it, the repo copy
/// counts when this machine's key signed it, or when it has no signature because an
/// earlier build wrote it: that first upgrade trusts the repo as earlier builds did.
fn own_record_from(
    local: &Path,
    sync_path: &Path,
    machine_id: &str,
    own_key: &PublicKey,
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
    let legacy = !record_sig_path(sync_path, machine_id).exists();
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

/// The keys that validly signed each machine record, trusted or not.
pub fn record_signers(sync_path: &Path) -> Vec<(String, PublicKey)> {
    records(sync_path)
        .into_iter()
        .filter_map(|r| Some((r.record.machine_id, r.signer?)))
        .collect()
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

    fn key() -> PrivateKey {
        PrivateKey::random(&mut OsRng, Algorithm::Ed25519).unwrap()
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
    fn own_record_comes_from_the_local_copy() {
        let tmp = tempfile::TempDir::new().unwrap();
        let (dir, local) = (tmp.path(), tmp.path().join("machine.json"));
        let me = key();
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
        let own = own_record_from(&local, dir, "me", me.public_key())
            .unwrap()
            .unwrap();
        assert_eq!(own.removed_packages["npm"], vec!["old"]);
        assert!(own.ignored_dotfiles.is_empty());
        let renamed = own_record_from(&local, dir, "me2", me.public_key())
            .unwrap()
            .unwrap();
        assert_eq!(renamed.machine_id, "me2");

        // Without a local copy, a repo copy signed by another key is not used
        let none = tmp.path().join("none.json");
        sign_record(dir, "me", &key()).unwrap();
        assert!(own_record_from(&none, dir, "me", me.public_key())
            .unwrap()
            .is_none());
        sign_record(dir, "me", &me).unwrap();
        assert!(own_record_from(&none, dir, "me", me.public_key())
            .unwrap()
            .is_some());
        // An unsigned copy from an earlier build is used once, on the upgrade
        std::fs::remove_file(record_sig_path(dir, "me")).unwrap();
        assert!(own_record_from(&none, dir, "me", me.public_key())
            .unwrap()
            .is_some());
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

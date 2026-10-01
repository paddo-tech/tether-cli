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

/// Write this machine's record and sign it. The signature vouches for the record's
/// packages, so call it only with packages just read from this machine's managers.
pub fn save_record(sync_path: &Path, record: &MachineState) -> Result<()> {
    record.save_to_repo(sync_path)?;
    let key = load_or_create(&record.machine_id)?;
    sign_record(sync_path, &record.machine_id, &key)
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
            let mut record: MachineState = serde_json::from_slice(&bytes).ok()?;
            if record.machine_id != id || record.validate().is_err() {
                return None;
            }
            let signer = std::fs::read_to_string(record_sig_path(sync_path, id))
                .ok()
                .and_then(|sig| record_signer(id, &bytes, &sig));
            Some(SignedRecord { record, signer })
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

use anyhow::{Context, Result};
use git2::{Oid, Repository};
use ssh_key::rand_core::OsRng;
use ssh_key::{Algorithm, HashAlg, LineEnding, PrivateKey, PublicKey, SshSig};
use std::path::{Path, PathBuf};

/// The namespace git uses for SSH commit signatures, so `git log --show-signature` verifies them.
const NAMESPACE: &str = "git";

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
        store.trust(machine_id, key.public_key());
        store.save()
    })?;
    Ok(key)
}

/// Path of a machine's published public key in the sync repo.
pub fn published_key_path(sync_path: &Path, machine_id: &str) -> PathBuf {
    sync_path
        .join("machines")
        .join(format!("{}.pub", machine_id))
}

/// Write this machine's public key to the sync repo so other machines can trust it.
pub fn publish(sync_path: &Path, machine_id: &str, key: &PublicKey) -> Result<()> {
    let path = published_key_path(sync_path, machine_id);
    let line = format!("{}\n", key.to_openssh()?);
    if std::fs::read_to_string(&path).ok().as_deref() != Some(line.as_str()) {
        crate::sync::atomic_write(&path, line.as_bytes())?;
    }
    Ok(())
}

/// Public keys published in the sync repo, by machine id. Unparseable files are skipped.
pub fn published_keys(sync_path: &Path) -> Vec<(String, PublicKey)> {
    let Ok(entries) = std::fs::read_dir(sync_path.join("machines")) else {
        return Vec::new();
    };
    let mut keys: Vec<(String, PublicKey)> = entries
        .flatten()
        .filter_map(|entry| {
            let path = entry.path();
            if path.extension()? != "pub" {
                return None;
            }
            let id = path.file_stem()?.to_str()?.to_string();
            let key = PublicKey::from_openssh(std::fs::read_to_string(&path).ok()?.trim()).ok()?;
            Some((id, key))
        })
        .collect();
    keys.sort_by(|a, b| a.0.cmp(&b.0));
    keys
}

pub fn fingerprint(key: &PublicKey) -> String {
    key.fingerprint(HashAlg::Sha256).to_string()
}

/// An armored SSH signature over a commit buffer, as git stores it in `gpgsig`.
pub fn sign(key: &PrivateKey, data: &[u8]) -> Result<String> {
    Ok(key
        .sign(NAMESPACE, HashAlg::Sha512, data)?
        .to_pem(LineEnding::LF)?)
}

/// The key that made a valid signature over `data`, or None.
pub fn verify(signature: &str, data: &[u8]) -> Option<PublicKey> {
    let sig = SshSig::from_pem(signature).ok()?;
    let key = PublicKey::from(sig.public_key().clone());
    key.verify(NAMESPACE, data, &sig).ok()?;
    Some(key)
}

/// The key that validly signed a commit, or None for an unsigned or badly signed commit.
pub fn commit_signer(repo: &Repository, oid: Oid) -> Option<PublicKey> {
    let (signature, data) = repo.extract_signature(&oid, None).ok()?;
    verify(signature.as_str()?, &data)
}

/// Machine keys this machine trusts, in `~/.tether/trusted_keys`. It is never synced:
/// trust is a decision about this machine. The format is git's allowed signers format,
/// one `<machine id> <public key>` line per key, so git can verify with it too.
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
        Ok(Self::parse(&std::fs::read_to_string(path)?))
    }

    fn parse(content: &str) -> Self {
        let keys = content
            .lines()
            .filter_map(|line| {
                let (id, key) = line.trim().split_once(' ')?;
                Some((id.to_string(), PublicKey::from_openssh(key).ok()?))
            })
            .collect();
        Self { keys }
    }

    fn render(&self) -> Result<String> {
        let mut out = String::new();
        for (id, key) in &self.keys {
            out.push_str(&format!("{} {}\n", id, key.to_openssh()?));
        }
        Ok(out)
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

    /// Trust `key` as `machine_id`, replacing any other key that id had.
    pub fn trust(&mut self, machine_id: &str, key: &PublicKey) {
        self.keys
            .retain(|(id, k)| id != machine_id && k.key_data() != key.key_data());
        let key = PublicKey::new(key.key_data().clone(), "");
        self.keys.push((machine_id.to_string(), key));
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

    #[test]
    fn sign_verify_roundtrip() {
        let key = key();
        let signature = sign(&key, b"tree abc\n").unwrap();
        assert!(signature.starts_with("-----BEGIN SSH SIGNATURE-----"));
        let signer = verify(&signature, b"tree abc\n").unwrap();
        assert_eq!(signer.key_data(), key.public_key().key_data());
        assert!(verify(&signature, b"tree abd\n").is_none());
        assert!(verify("not a signature", b"tree abc\n").is_none());
    }

    #[test]
    fn trust_store_roundtrips_and_replaces_rotated_keys() {
        let (a, a2, b) = (key(), key(), key());
        let mut store = TrustStore::default();
        store.trust("a", a.public_key());
        store.trust("b", b.public_key());
        let parsed = TrustStore::parse(&store.render().unwrap());
        assert_eq!(parsed, store);
        assert_eq!(parsed.machine_for(b.public_key()), Some("b"));

        store.trust("a", a2.public_key());
        assert_eq!(store.keys.len(), 2);
        assert!(store.machine_for(a.public_key()).is_none());
        assert_eq!(store.machine_for(a2.public_key()), Some("a"));

        assert!(store.untrust("a"));
        assert!(!store.untrust("a"));
        assert!(store.key_for("a").is_none());
    }

    #[test]
    fn published_keys_skip_other_files() {
        let tmp = tempfile::TempDir::new().unwrap();
        let key = key();
        publish(tmp.path(), "m1", key.public_key()).unwrap();
        std::fs::write(tmp.path().join("machines/m1.json"), "{}").unwrap();
        std::fs::write(tmp.path().join("machines/bad.pub"), "junk").unwrap();
        let keys = published_keys(tmp.path());
        assert_eq!(keys.len(), 1);
        assert_eq!(keys[0].0, "m1");
        assert_eq!(keys[0].1.key_data(), key.public_key().key_data());
    }
}

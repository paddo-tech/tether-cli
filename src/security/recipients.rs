use age::secrecy::{ExposeSecret, Secret};
use anyhow::{Context, Result};
use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

#[cfg(unix)]
use std::os::unix::fs::OpenOptionsExt;

const IDENTITY_FILENAME: &str = "identity.age";
const PUBKEY_FILENAME: &str = "identity.pub";

/// Get path to user's encrypted identity file
fn identity_path() -> Result<PathBuf> {
    let home = crate::home_dir()?;
    Ok(home.join(".tether").join(IDENTITY_FILENAME))
}

/// Get path to user's public key file
fn pubkey_path() -> Result<PathBuf> {
    let home = crate::home_dir()?;
    Ok(home.join(".tether").join(PUBKEY_FILENAME))
}

/// Get path to cached decrypted identity (local only)
fn cached_identity_path() -> Result<PathBuf> {
    let home = crate::home_dir()?;
    Ok(home.join(".tether").join("identity.cache"))
}

/// Generate a new age X25519 identity
pub fn generate_identity() -> age::x25519::Identity {
    age::x25519::Identity::generate()
}

/// Get public key string from identity
pub fn get_public_key_from_identity(identity: &age::x25519::Identity) -> String {
    identity.to_public().to_string()
}

/// Store identity encrypted with passphrase
pub fn store_identity(identity: &age::x25519::Identity, passphrase: &str) -> Result<()> {
    let identity_str = identity.to_string();
    let encryptor = age::Encryptor::with_user_passphrase(Secret::new(passphrase.to_owned()));

    let mut encrypted = vec![];
    let mut writer = encryptor
        .wrap_output(&mut encrypted)
        .map_err(|e| anyhow::anyhow!("Failed to create encryptor: {}", e))?;
    writer.write_all(identity_str.expose_secret().as_bytes())?;
    writer
        .finish()
        .map_err(|e| anyhow::anyhow!("Failed to finish encryption: {}", e))?;

    let path = identity_path()?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }

    #[cfg(unix)]
    {
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&path)?;
        file.write_all(&encrypted)?;
    }
    #[cfg(not(unix))]
    fs::write(&path, &encrypted)?;

    // Also store public key for easy sharing
    let pubkey = identity.to_public().to_string();
    let pubkey_file = pubkey_path()?;
    fs::write(&pubkey_file, &pubkey)?;

    Ok(())
}

/// Load identity from cache or decrypt with passphrase
pub fn load_identity(passphrase: Option<&str>) -> Result<age::x25519::Identity> {
    // Try cache first
    let cache_path = cached_identity_path()?;
    if cache_path.exists() {
        let identity_str = fs::read_to_string(&cache_path)?;
        return identity_str
            .parse()
            .map_err(|e| anyhow::anyhow!("Invalid cached identity: {}", e));
    }

    // Need passphrase to decrypt
    let passphrase = passphrase.ok_or_else(|| {
        anyhow::anyhow!("Identity not cached. Provide passphrase or run 'tether identity unlock'")
    })?;

    let path = identity_path()?;
    if !path.exists() {
        return Err(anyhow::anyhow!(
            "No identity found. Run 'tether identity init' first."
        ));
    }

    let encrypted = fs::read(&path)?;
    let decryptor = match age::Decryptor::new(&encrypted[..])
        .map_err(|e| anyhow::anyhow!("Failed to create decryptor: {}", e))?
    {
        age::Decryptor::Passphrase(d) => d,
        _ => return Err(anyhow::anyhow!("Identity not encrypted with passphrase")),
    };

    let mut identity_str = String::new();
    let mut reader = decryptor
        .decrypt(&Secret::new(passphrase.to_owned()), None)
        .map_err(|_| anyhow::anyhow!("Wrong passphrase"))?;
    reader.read_to_string(&mut identity_str)?;

    let identity: age::x25519::Identity = identity_str
        .parse()
        .map_err(|e| anyhow::anyhow!("Invalid identity: {}", e))?;

    // Cache for future use
    cache_identity(&identity)?;

    Ok(identity)
}

/// Cache decrypted identity locally
fn cache_identity(identity: &age::x25519::Identity) -> Result<()> {
    let path = cached_identity_path()?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }

    let identity_str = identity.to_string();

    #[cfg(unix)]
    {
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&path)?;
        file.write_all(identity_str.expose_secret().as_bytes())?;
    }
    #[cfg(not(unix))]
    fs::write(&path, identity_str.expose_secret())?;

    Ok(())
}

/// Clear cached identity
pub fn clear_cached_identity() -> Result<()> {
    let path = cached_identity_path()?;
    if path.exists() {
        fs::remove_file(&path)?;
    }
    Ok(())
}

/// Check if identity exists
pub fn has_identity() -> bool {
    identity_path().map(|p| p.exists()).unwrap_or(false)
}

/// Check if identity is cached (unlocked)
pub fn is_identity_unlocked() -> bool {
    cached_identity_path().map(|p| p.exists()).unwrap_or(false)
}

/// Get user's public key string
pub fn get_public_key() -> Result<String> {
    let path = pubkey_path()?;
    if path.exists() {
        return fs::read_to_string(&path).context("Failed to read public key");
    }

    // Try to derive from cached identity
    if let Ok(identity) = load_identity(None) {
        return Ok(identity.to_public().to_string());
    }

    Err(anyhow::anyhow!(
        "No public key found. Run 'tether identity init' or 'tether identity unlock'"
    ))
}

/// Load recipients from a team's recipients directory
pub fn load_recipients(recipients_dir: &Path) -> Result<Vec<age::x25519::Recipient>> {
    let mut recipients = Vec::new();

    if !recipients_dir.exists() {
        return Ok(recipients);
    }

    for entry in fs::read_dir(recipients_dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.extension().is_some_and(|e| e == "pub") {
            let pubkey = fs::read_to_string(&path)?;
            let recipient: age::x25519::Recipient = pubkey
                .trim()
                .parse()
                .map_err(|_| anyhow::anyhow!("Invalid public key in {:?}", path))?;
            recipients.push(recipient);
        }
    }

    Ok(recipients)
}

/// Encrypt data to multiple recipients
pub fn encrypt_to_recipients(
    data: &[u8],
    recipients: &[age::x25519::Recipient],
) -> Result<Vec<u8>> {
    if recipients.is_empty() {
        return Err(anyhow::anyhow!("No recipients specified"));
    }

    let recipients_boxed: Vec<Box<dyn age::Recipient + Send>> = recipients
        .iter()
        .map(|r| Box::new(r.clone()) as Box<dyn age::Recipient + Send>)
        .collect();

    let encryptor = age::Encryptor::with_recipients(recipients_boxed)
        .ok_or_else(|| anyhow::anyhow!("Failed to create encryptor: no recipients"))?;

    let mut encrypted = vec![];
    let mut writer = encryptor
        .wrap_output(&mut encrypted)
        .map_err(|e| anyhow::anyhow!("Failed to wrap output: {}", e))?;
    writer.write_all(data)?;
    writer
        .finish()
        .map_err(|e| anyhow::anyhow!("Failed to finish encryption: {}", e))?;

    Ok(encrypted)
}

/// Decrypt data with user's identity
pub fn decrypt_with_identity(data: &[u8], identity: &age::x25519::Identity) -> Result<Vec<u8>> {
    let decryptor = match age::Decryptor::new(data)
        .map_err(|e| anyhow::anyhow!("Failed to create decryptor: {}", e))?
    {
        age::Decryptor::Recipients(d) => d,
        _ => return Err(anyhow::anyhow!("Data not encrypted with recipients")),
    };

    let mut decrypted = vec![];
    let mut reader = decryptor
        .decrypt(std::iter::once(identity as &dyn age::Identity))
        .map_err(|_| anyhow::anyhow!("Failed to decrypt - you may not be a recipient"))?;
    reader.read_to_end(&mut decrypted)?;

    Ok(decrypted)
}

/// Validate an age public key string
pub fn validate_pubkey(pubkey: &str) -> Result<age::x25519::Recipient> {
    pubkey
        .trim()
        .parse()
        .map_err(|_| anyhow::anyhow!("Invalid age public key format"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_generate_and_encrypt_decrypt() {
        let identity = generate_identity();
        let recipient = identity.to_public();
        let data = b"secret team data";

        let encrypted = encrypt_to_recipients(data, &[recipient]).unwrap();
        let decrypted = decrypt_with_identity(&encrypted, &identity).unwrap();

        assert_eq!(decrypted, data);
    }

    #[test]
    fn test_multi_recipient() {
        let identity1 = generate_identity();
        let identity2 = generate_identity();
        let recipients = vec![identity1.to_public(), identity2.to_public()];
        let data = b"shared secret";

        let encrypted = encrypt_to_recipients(data, &recipients).unwrap();

        // Both can decrypt
        let decrypted1 = decrypt_with_identity(&encrypted, &identity1).unwrap();
        let decrypted2 = decrypt_with_identity(&encrypted, &identity2).unwrap();

        assert_eq!(decrypted1, data);
        assert_eq!(decrypted2, data);
    }

    #[test]
    fn test_wrong_recipient_fails() {
        let identity1 = generate_identity();
        let identity2 = generate_identity();
        let data = b"secret";

        // Encrypt only to identity1
        let encrypted = encrypt_to_recipients(data, &[identity1.to_public()]).unwrap();

        // identity2 cannot decrypt
        let result = decrypt_with_identity(&encrypted, &identity2);
        assert!(result.is_err());
    }

    #[test]
    fn test_validate_pubkey() {
        let identity = generate_identity();
        let pubkey_str = identity.to_public().to_string();

        let result = validate_pubkey(&pubkey_str);
        assert!(result.is_ok());

        let invalid = validate_pubkey("not-a-valid-key");
        assert!(invalid.is_err());
    }
}

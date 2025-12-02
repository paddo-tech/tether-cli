use age::secrecy::Secret;
use anyhow::{Context, Result};
use std::fs;
use std::io::{Read, Write};
use std::path::PathBuf;

#[cfg(unix)]
use std::os::unix::fs::OpenOptionsExt;

const ENCRYPTED_KEY_FILENAME: &str = "encryption.key.age";

/// Get the path to the encrypted key in the sync repo
fn encrypted_key_path() -> Result<PathBuf> {
    let sync_path = crate::sync::SyncEngine::sync_path()?;
    Ok(sync_path.join(ENCRYPTED_KEY_FILENAME))
}

/// Get the path to the cached decrypted key (local only, not synced)
fn cached_key_path() -> Result<PathBuf> {
    let home = home::home_dir().context("Could not find home directory")?;
    Ok(home.join(".tether").join("key.cache"))
}

/// Store the encryption key encrypted with a passphrase
/// The encrypted key is stored in the sync repo (syncs via git)
pub fn store_encryption_key_with_passphrase(key: &[u8], passphrase: &str) -> Result<()> {
    let encryptor = age::Encryptor::with_user_passphrase(Secret::new(passphrase.to_owned()));

    let mut encrypted = vec![];
    let mut writer = encryptor
        .wrap_output(&mut encrypted)
        .map_err(|e| anyhow::anyhow!("Failed to create encryptor: {}", e))?;
    writer.write_all(key)?;
    writer
        .finish()
        .map_err(|e| anyhow::anyhow!("Failed to finish encryption: {}", e))?;

    let path = encrypted_key_path()?;
    fs::write(&path, &encrypted).context("Failed to write encrypted key")?;

    Ok(())
}

/// Cache the decrypted key locally for the session
/// This avoids prompting for passphrase on every operation
fn cache_key(key: &[u8]) -> Result<()> {
    let path = cached_key_path()?;
    if let Some(parent) = path.parent() {
        #[cfg(unix)]
        {
            fs::create_dir_all(parent)?;
            // Set directory permissions to 0o700
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(parent, fs::Permissions::from_mode(0o700))?;
        }
        #[cfg(not(unix))]
        fs::create_dir_all(parent)?;
    }

    // Write key with secure permissions (0o600 on Unix)
    #[cfg(unix)]
    {
        use std::fs::OpenOptions;
        let mut file = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&path)?;
        file.write_all(key)?;
    }
    #[cfg(not(unix))]
    fs::write(&path, key)?;

    Ok(())
}

/// Clear the cached key
pub fn clear_cached_key() -> Result<()> {
    let path = cached_key_path()?;
    if path.exists() {
        fs::remove_file(&path)?;
    }
    Ok(())
}

/// Get the encryption key, prompting for passphrase if needed
/// First checks cache, then decrypts from sync repo
pub fn get_encryption_key() -> Result<Vec<u8>> {
    // Try cached key first
    if let Ok(path) = cached_key_path() {
        if path.exists() {
            if let Ok(key) = fs::read(&path) {
                if key.len() == crate::security::encryption::KEY_SIZE {
                    return Ok(key);
                }
            }
        }
    }

    // No cache - need to decrypt with passphrase
    Err(anyhow::anyhow!(
        "Encryption key not cached. Run 'tether unlock' to decrypt with passphrase."
    ))
}

/// Decrypt and cache the key using a passphrase
pub fn unlock_with_passphrase(passphrase: &str) -> Result<Vec<u8>> {
    let path = encrypted_key_path()?;
    if !path.exists() {
        return Err(anyhow::anyhow!(
            "No encrypted key found. Run 'tether init' first."
        ));
    }

    let encrypted = fs::read(&path).context("Failed to read encrypted key")?;

    let decryptor = match age::Decryptor::new(&encrypted[..])
        .map_err(|e| anyhow::anyhow!("Failed to create decryptor: {}", e))?
    {
        age::Decryptor::Passphrase(d) => d,
        _ => return Err(anyhow::anyhow!("Key file not encrypted with passphrase")),
    };

    let mut key = vec![];
    let mut reader = decryptor
        .decrypt(&Secret::new(passphrase.to_owned()), None)
        .map_err(|_| anyhow::anyhow!("Wrong passphrase"))?;
    reader.read_to_end(&mut key)?;

    if key.len() != crate::security::encryption::KEY_SIZE {
        return Err(anyhow::anyhow!("Decrypted key has wrong size"));
    }

    // Cache for future use
    cache_key(&key)?;

    Ok(key)
}

/// Check if an encrypted key exists in the sync repo
pub fn has_encryption_key() -> bool {
    encrypted_key_path().map(|p| p.exists()).unwrap_or(false)
}

/// Check if the key is currently unlocked (cached)
pub fn is_unlocked() -> bool {
    cached_key_path().map(|p| p.exists()).unwrap_or(false)
}

/// Delete the encryption key (both encrypted and cached)
pub fn delete_encryption_key() -> Result<()> {
    if let Ok(path) = encrypted_key_path() {
        let _ = fs::remove_file(&path);
    }
    if let Ok(path) = cached_key_path() {
        let _ = fs::remove_file(&path);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};

    #[test]
    fn test_encrypt_decrypt_with_passphrase() {
        let key = crate::security::encryption::generate_key();
        let passphrase = "test-passphrase-123";

        // Encrypt
        let encryptor = age::Encryptor::with_user_passphrase(Secret::new(passphrase.to_owned()));
        let mut encrypted = vec![];
        let mut writer = encryptor.wrap_output(&mut encrypted).unwrap();
        writer.write_all(&key).unwrap();
        writer.finish().unwrap();

        // Decrypt
        let decryptor = match age::Decryptor::new(&encrypted[..]).unwrap() {
            age::Decryptor::Passphrase(d) => d,
            _ => panic!("Expected passphrase decryptor"),
        };
        let mut decrypted = vec![];
        let mut reader = decryptor
            .decrypt(&Secret::new(passphrase.to_owned()), None)
            .unwrap();
        reader.read_to_end(&mut decrypted).unwrap();

        assert_eq!(decrypted, key);
    }

    #[test]
    fn test_wrong_passphrase_fails() {
        let key = crate::security::encryption::generate_key();
        let passphrase = "correct";
        let wrong_passphrase = "wrong";

        // Encrypt
        let encryptor = age::Encryptor::with_user_passphrase(Secret::new(passphrase.to_owned()));
        let mut encrypted = vec![];
        let mut writer = encryptor.wrap_output(&mut encrypted).unwrap();
        writer.write_all(&key).unwrap();
        writer.finish().unwrap();

        // Decrypt with wrong passphrase
        let decryptor = match age::Decryptor::new(&encrypted[..]).unwrap() {
            age::Decryptor::Passphrase(d) => d,
            _ => panic!("Expected passphrase decryptor"),
        };
        let result = decryptor.decrypt(&Secret::new(wrong_passphrase.to_owned()), None);

        assert!(result.is_err());
    }
}

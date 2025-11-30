use anyhow::{Context, Result};
use base64::{engine::general_purpose::STANDARD, Engine};
use std::fs;
use std::path::PathBuf;
use std::process::Command;

const SERVICE_NAME: &str = "com.tether-cli";
const ACCOUNT_NAME: &str = "encryption-key";

/// Get the path to the key file in iCloud Drive (syncs automatically)
fn icloud_key_path() -> Option<PathBuf> {
    let home = home::home_dir()?;
    let icloud_path = home
        .join("Library/Mobile Documents/com~apple~CloudDocs")
        .join(".tether-key");

    // Only use if iCloud Drive exists
    if icloud_path.parent()?.exists() {
        Some(icloud_path)
    } else {
        None
    }
}

/// Get the local key file path (fallback)
fn local_key_path() -> Result<PathBuf> {
    let home = home::home_dir().context("Could not find home directory")?;
    Ok(home.join(".tether").join("encryption.key"))
}

/// Store the encryption key
/// Stores in iCloud Drive if available, otherwise local file + keychain
pub fn store_encryption_key(key: &[u8]) -> Result<()> {
    let encoded = STANDARD.encode(key);

    // Try iCloud Drive first (syncs automatically between Macs)
    if let Some(icloud_path) = icloud_key_path() {
        fs::write(&icloud_path, &encoded)
            .context("Failed to write key to iCloud Drive")?;

        // Also store in local keychain as backup
        let _ = store_in_keychain(&encoded);

        return Ok(());
    }

    // Fallback: store locally and in keychain
    let local_path = local_key_path()?;
    if let Some(parent) = local_path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(&local_path, &encoded)
        .context("Failed to write key to local file")?;

    // Also store in keychain
    store_in_keychain(&encoded)?;

    Ok(())
}

fn store_in_keychain(encoded: &str) -> Result<()> {
    // Delete existing entry first
    let _ = Command::new("security")
        .args([
            "delete-generic-password",
            "-a", ACCOUNT_NAME,
            "-s", SERVICE_NAME,
        ])
        .output();

    let output = Command::new("security")
        .args([
            "add-generic-password",
            "-a", ACCOUNT_NAME,
            "-s", SERVICE_NAME,
            "-w", encoded,
            "-U",
        ])
        .output()
        .context("Failed to run security command")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(anyhow::anyhow!("Failed to store key in Keychain: {}", stderr));
    }

    Ok(())
}

/// Retrieve the encryption key
/// Tries iCloud Drive first, then local file, then keychain
pub fn get_encryption_key() -> Result<Vec<u8>> {
    // Try iCloud Drive first
    if let Some(icloud_path) = icloud_key_path() {
        if icloud_path.exists() {
            let encoded = fs::read_to_string(&icloud_path)
                .context("Failed to read key from iCloud Drive")?;
            if let Ok(key) = STANDARD.decode(encoded.trim()) {
                return Ok(key);
            }
        }
    }

    // Try local file
    if let Ok(local_path) = local_key_path() {
        if local_path.exists() {
            let encoded = fs::read_to_string(&local_path)
                .context("Failed to read key from local file")?;
            if let Ok(key) = STANDARD.decode(encoded.trim()) {
                return Ok(key);
            }
        }
    }

    // Try keychain (for backwards compatibility)
    get_from_keychain()
}

fn get_from_keychain() -> Result<Vec<u8>> {
    let output = Command::new("security")
        .args([
            "find-generic-password",
            "-a", ACCOUNT_NAME,
            "-s", SERVICE_NAME,
            "-w",
        ])
        .output()
        .context("Failed to run security command")?;

    if !output.status.success() {
        return Err(anyhow::anyhow!(
            "Encryption key not found. Run 'tether init' first."
        ));
    }

    let stored = String::from_utf8(output.stdout)
        .context("Invalid UTF-8")?
        .trim()
        .to_string();

    // Try base64
    if let Ok(key) = STANDARD.decode(&stored) {
        return Ok(key);
    }

    // Try hex (old format)
    if stored.len() == 64 && stored.chars().all(|c| c.is_ascii_hexdigit()) {
        if let Ok(key) = hex::decode(&stored) {
            return Ok(key);
        }
    }

    Err(anyhow::anyhow!("Failed to decode encryption key"))
}

/// Check if an encryption key exists
pub fn has_encryption_key() -> bool {
    // Check iCloud Drive
    if let Some(icloud_path) = icloud_key_path() {
        if icloud_path.exists() {
            return true;
        }
    }

    // Check local file
    if let Ok(local_path) = local_key_path() {
        if local_path.exists() {
            return true;
        }
    }

    // Check keychain
    Command::new("security")
        .args([
            "find-generic-password",
            "-a", ACCOUNT_NAME,
            "-s", SERVICE_NAME,
        ])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// Delete the encryption key
pub fn delete_encryption_key() -> Result<()> {
    // Delete from iCloud Drive
    if let Some(icloud_path) = icloud_key_path() {
        let _ = fs::remove_file(&icloud_path);
    }

    // Delete local file
    if let Ok(local_path) = local_key_path() {
        let _ = fs::remove_file(&local_path);
    }

    // Delete from keychain
    let _ = Command::new("security")
        .args([
            "delete-generic-password",
            "-a", ACCOUNT_NAME,
            "-s", SERVICE_NAME,
        ])
        .output();

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[ignore] // This test requires macOS Keychain access
    fn test_store_and_retrieve_key() {
        let test_key = b"test_encryption_key_32_bytes!!!";

        // Clean up any existing test key
        let _ = delete_encryption_key();

        // Store key
        store_encryption_key(test_key).unwrap();

        // Retrieve key
        let retrieved = get_encryption_key().unwrap();
        assert_eq!(retrieved, test_key);

        // Check existence
        assert!(has_encryption_key());

        // Clean up
        delete_encryption_key().unwrap();
        assert!(!has_encryption_key());
    }
}

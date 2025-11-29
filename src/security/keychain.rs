use anyhow::{Context, Result};
use base64::{engine::general_purpose::STANDARD, Engine};
use std::process::Command;

const SERVICE_NAME: &str = "com.tether-cli";
const ACCOUNT_NAME: &str = "encryption-key";

/// Store the encryption key in Keychain using security CLI
pub fn store_encryption_key(key: &[u8]) -> Result<()> {
    // Delete existing key if present (ignore errors)
    let _ = delete_encryption_key();

    // Encode key as base64 for safe storage
    let encoded = STANDARD.encode(key);

    let output = Command::new("security")
        .args([
            "add-generic-password",
            "-a", ACCOUNT_NAME,
            "-s", SERVICE_NAME,
            "-w", &encoded,
            "-U", // Update if exists
        ])
        .output()
        .context("Failed to run security command")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(anyhow::anyhow!("Failed to store key in Keychain: {}", stderr));
    }

    Ok(())
}

/// Retrieve the encryption key from Keychain using security CLI
pub fn get_encryption_key() -> Result<Vec<u8>> {
    let output = Command::new("security")
        .args([
            "find-generic-password",
            "-a", ACCOUNT_NAME,
            "-s", SERVICE_NAME,
            "-w", // Print password only
        ])
        .output()
        .context("Failed to run security command")?;

    if !output.status.success() {
        return Err(anyhow::anyhow!(
            "Encryption key not found in Keychain. Run 'tether init' first."
        ));
    }

    let encoded = String::from_utf8(output.stdout)
        .context("Invalid UTF-8 in keychain data")?
        .trim()
        .to_string();

    STANDARD
        .decode(&encoded)
        .context("Failed to decode encryption key from Keychain")
}

/// Check if an encryption key exists in the Keychain
pub fn has_encryption_key() -> bool {
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

/// Delete the encryption key from Keychain
pub fn delete_encryption_key() -> Result<()> {
    let output = Command::new("security")
        .args([
            "delete-generic-password",
            "-a", ACCOUNT_NAME,
            "-s", SERVICE_NAME,
        ])
        .output()
        .context("Failed to run security command")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        // Ignore "item not found" errors
        if !stderr.contains("could not be found") {
            return Err(anyhow::anyhow!("Failed to delete key from Keychain: {}", stderr));
        }
    }

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

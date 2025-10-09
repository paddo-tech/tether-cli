use anyhow::{Context, Result};
use security_framework::passwords::{
    delete_generic_password, get_generic_password, set_generic_password,
};

const SERVICE_NAME: &str = "com.tether-cli";
const ACCOUNT_NAME: &str = "encryption-key";

/// Store the encryption key in iCloud Keychain
/// This key will automatically sync across all Macs with the same iCloud account
pub fn store_encryption_key(key: &[u8]) -> Result<()> {
    // Delete existing key if present
    let _ = delete_generic_password(SERVICE_NAME, ACCOUNT_NAME);

    // Store new key in Keychain
    // By default, this goes to the user's login keychain which syncs via iCloud
    set_generic_password(SERVICE_NAME, ACCOUNT_NAME, key)
        .context("Failed to store encryption key in Keychain")?;

    Ok(())
}

/// Retrieve the encryption key from iCloud Keychain
/// This will work across all Macs where the key has synced
pub fn get_encryption_key() -> Result<Vec<u8>> {
    get_generic_password(SERVICE_NAME, ACCOUNT_NAME)
        .context("Failed to retrieve encryption key from Keychain. Make sure Tether is initialized with encryption enabled.")?
        .to_vec()
        .pipe(Ok)
}

/// Check if an encryption key exists in the Keychain
pub fn has_encryption_key() -> bool {
    get_generic_password(SERVICE_NAME, ACCOUNT_NAME).is_ok()
}

/// Delete the encryption key from Keychain
/// This is useful for testing or when reinitializing
pub fn delete_encryption_key() -> Result<()> {
    delete_generic_password(SERVICE_NAME, ACCOUNT_NAME)
        .context("Failed to delete encryption key from Keychain")?;
    Ok(())
}

// Helper trait for pipe syntax
trait Pipe: Sized {
    fn pipe<F, R>(self, f: F) -> R
    where
        F: FnOnce(Self) -> R,
    {
        f(self)
    }
}

impl<T> Pipe for T {}

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

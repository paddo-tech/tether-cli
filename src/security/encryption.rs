use aes_gcm::{
    aead::{Aead, KeyInit, OsRng},
    Aes256Gcm,
};
use anyhow::{Context, Result};
use rand::RngCore;

const NONCE_SIZE: usize = 12; // 96 bits for GCM
pub const KEY_SIZE: usize = 32; // 256 bits for AES-256

/// Generate a new random encryption key (32 bytes for AES-256)
pub fn generate_key() -> [u8; KEY_SIZE] {
    let mut key = [0u8; KEY_SIZE];
    OsRng.fill_bytes(&mut key);
    key
}

/// Encrypt data using AES-256-GCM
/// Format: [nonce (12 bytes)][ciphertext + auth tag]
pub fn encrypt(plaintext: &[u8], key: &[u8]) -> Result<Vec<u8>> {
    if key.len() != KEY_SIZE {
        return Err(anyhow::anyhow!(
            "Invalid key size: expected {} bytes, got {}",
            KEY_SIZE,
            key.len()
        ));
    }

    // Create cipher
    let cipher = Aes256Gcm::new_from_slice(key).context("Failed to create cipher from key")?;

    // Generate random nonce
    let mut nonce_bytes = [0u8; NONCE_SIZE];
    OsRng.fill_bytes(&mut nonce_bytes);

    // Encrypt
    let ciphertext = cipher
        .encrypt((&nonce_bytes).into(), plaintext)
        .map_err(|e| anyhow::anyhow!("Encryption failed: {}", e))?;

    // Combine nonce + ciphertext
    let mut result = Vec::with_capacity(NONCE_SIZE + ciphertext.len());
    result.extend_from_slice(&nonce_bytes);
    result.extend_from_slice(&ciphertext);

    Ok(result)
}

/// Decrypt data using AES-256-GCM
/// Expects format: [nonce (12 bytes)][ciphertext + auth tag]
pub fn decrypt(encrypted_data: &[u8], key: &[u8]) -> Result<Vec<u8>> {
    if key.len() != KEY_SIZE {
        return Err(anyhow::anyhow!(
            "Invalid key size: expected {} bytes, got {}",
            KEY_SIZE,
            key.len()
        ));
    }

    if encrypted_data.len() < NONCE_SIZE {
        return Err(anyhow::anyhow!(
            "Encrypted data too short: must be at least {} bytes",
            NONCE_SIZE
        ));
    }

    // Split nonce and ciphertext
    let (nonce_bytes, ciphertext) = encrypted_data.split_at(NONCE_SIZE);

    // Create cipher
    let cipher = Aes256Gcm::new_from_slice(key).context("Failed to create cipher from key")?;

    // Decrypt
    let plaintext = cipher
        .decrypt(nonce_bytes.into(), ciphertext)
        .map_err(|e| {
            anyhow::anyhow!(
            "Decryption failed: {}. The file may be corrupted or encrypted with a different key.",
            e
        )
        })?;

    Ok(plaintext)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_key_generation() {
        let key1 = generate_key();
        let key2 = generate_key();

        assert_eq!(key1.len(), KEY_SIZE);
        assert_eq!(key2.len(), KEY_SIZE);
        assert_ne!(key1, key2); // Keys should be random
    }

    #[test]
    fn test_encrypt_decrypt_roundtrip() {
        let key = generate_key();
        let plaintext = b"Hello, this is a secret message!";

        let encrypted = encrypt(plaintext, &key).unwrap();
        assert_ne!(encrypted.as_slice(), plaintext); // Should be different
        assert!(encrypted.len() > plaintext.len()); // Overhead from nonce + auth tag

        let decrypted = decrypt(&encrypted, &key).unwrap();
        assert_eq!(decrypted, plaintext);
    }

    #[test]
    fn test_encrypt_twice_different_results() {
        let key = generate_key();
        let plaintext = b"Same message";

        let encrypted1 = encrypt(plaintext, &key).unwrap();
        let encrypted2 = encrypt(plaintext, &key).unwrap();

        // Should be different due to random nonce
        assert_ne!(encrypted1, encrypted2);

        // But both should decrypt to same plaintext
        assert_eq!(decrypt(&encrypted1, &key).unwrap(), plaintext);
        assert_eq!(decrypt(&encrypted2, &key).unwrap(), plaintext);
    }

    #[test]
    fn test_wrong_key_fails() {
        let key1 = generate_key();
        let key2 = generate_key();
        let plaintext = b"Secret data";

        let encrypted = encrypt(plaintext, &key1).unwrap();
        let result = decrypt(&encrypted, &key2);

        assert!(result.is_err()); // Should fail with wrong key
    }

    #[test]
    fn test_corrupted_data_fails() {
        let key = generate_key();
        let plaintext = b"Data";

        let mut encrypted = encrypt(plaintext, &key).unwrap();
        // Corrupt a byte in the ciphertext
        if let Some(byte) = encrypted.get_mut(NONCE_SIZE + 5) {
            *byte ^= 0xFF;
        }

        let result = decrypt(&encrypted, &key);
        assert!(result.is_err()); // Should fail due to auth tag mismatch
    }

    #[test]
    fn test_empty_data() {
        let key = generate_key();
        let plaintext = b"";

        let encrypted = encrypt(plaintext, &key).unwrap();
        let decrypted = decrypt(&encrypted, &key).unwrap();

        assert_eq!(decrypted, plaintext);
    }

    #[test]
    fn test_large_data() {
        let key = generate_key();
        let plaintext = vec![42u8; 1024 * 1024]; // 1MB

        let encrypted = encrypt(&plaintext, &key).unwrap();
        let decrypted = decrypt(&encrypted, &key).unwrap();

        assert_eq!(decrypted, plaintext);
    }
}

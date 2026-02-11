pub mod encryption;
pub mod keychain;
pub mod recipients;
pub mod secrets;

pub use encryption::{decrypt, encrypt, generate_key};
pub use keychain::{
    clear_cached_key, get_encryption_key, has_encryption_key, is_unlocked,
    store_encryption_key_with_passphrase, unlock_with_passphrase,
};
pub use recipients::{
    clear_cached_identity, decrypt_with_identity, encrypt_to_recipients, generate_identity,
    get_public_key, get_public_key_from_identity, has_identity, is_identity_unlocked,
    load_identity, load_recipients, store_identity, validate_pubkey,
};
pub use secrets::{scan_for_secrets, SecretFinding, SecretType};

/// Restrict file permissions to current user only (Windows equivalent of chmod 600)
#[cfg(windows)]
pub(crate) fn restrict_file_permissions(path: &std::path::Path) -> anyhow::Result<()> {
    let path_str = path.to_string_lossy();
    let username = std::env::var("USERNAME").unwrap_or_default();
    if username.is_empty() {
        anyhow::bail!(
            "USERNAME not set, cannot restrict permissions on {}",
            path_str
        );
    }
    // Remove all ACEs then grant only current user — ensures no other accounts have access
    let output = std::process::Command::new("icacls")
        .args([
            &*path_str,
            "/inheritance:r",
            "/remove:g",
            "*S-1-1-0",
            "/remove:g",
            "BUILTIN\\Administrators",
            "/grant:r",
            &format!("{username}:F"),
        ])
        .output()?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("icacls failed on {}: {}", path_str, stderr.trim());
    }
    Ok(())
}

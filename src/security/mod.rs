pub mod encryption;
pub mod keychain;
pub mod recipients;
pub mod secrets;

use anyhow::Result;
use std::path::Path;

/// Write data to a file readable only by the current user.
/// Unix: mode 0o600. Windows: inherits parent ACL then restricts to current user via icacls.
pub fn write_owner_only(path: &Path, data: &[u8]) -> Result<()> {
    #[cfg(unix)]
    {
        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;
        use std::os::unix::fs::PermissionsExt;
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(path)?;
        file.write_all(data)?;
        // mode() only applies on creation; fix permissions for pre-existing files
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    }
    #[cfg(windows)]
    {
        // TOCTOU: file is briefly world-readable before icacls runs.
        // Fixing requires CreateFile with SECURITY_ATTRIBUTES via Win32 API.
        std::fs::write(path, data)?;
        let username = std::env::var("USERNAME")
            .map_err(|_| anyhow::anyhow!("USERNAME not set; cannot secure file permissions"))?;
        let path_str = path.to_string_lossy();
        // Strip inherited ACLs, then grant full control to current user only
        let status = std::process::Command::new("icacls")
            .args([
                &*path_str,
                "/inheritance:r",
                "/grant",
                &format!("{}:(F)", username),
            ])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status();
        match status {
            Ok(s) if s.success() => {}
            _ => {
                std::fs::remove_file(path).ok();
                anyhow::bail!("Failed to restrict file permissions on {}", path.display());
            }
        }
    }
    #[cfg(not(any(unix, windows)))]
    {
        std::fs::write(path, data)?;
    }
    Ok(())
}

pub use encryption::{decrypt, encrypt, generate_key, random_hex_id};
pub use keychain::{
    clear_cached_key, get_encryption_key, has_encryption_key, is_unlocked,
    store_encryption_key_with_passphrase, unlock_with_passphrase,
};
pub use recipients::{
    clear_cached_identity, decrypt_with_identity, encrypt_to_recipients, generate_identity,
    get_public_key, get_public_key_from_identity, has_identity, is_identity_unlocked,
    load_identity, load_recipients, load_recipients_authorized, store_identity, validate_pubkey,
};
pub use secrets::{scan_for_secrets, SecretFinding, SecretType};

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

/// Write sensitive data to a file with restricted permissions, avoiding TOCTOU.
/// On Unix: opens with mode 0o600 atomically.
/// On Windows: writes to a temp file, restricts ACLs, then renames into place.
#[cfg(windows)]
pub(crate) fn write_file_secure(
    path: &std::path::Path,
    contents: &[u8],
) -> anyhow::Result<()> {
    let dir = path.parent().unwrap_or(path);
    std::fs::create_dir_all(dir)?;
    let tmp = tempfile::NamedTempFile::new_in(dir)?;
    std::io::Write::write_all(&mut tmp.as_file(), contents)?;
    tmp.as_file().sync_all()?;
    restrict_file_permissions(tmp.path())?;
    tmp.persist(path)?;
    Ok(())
}

/// Restrict file permissions to current user only (Windows equivalent of chmod 600).
/// Uses the current user's SID from `whoami /user` to avoid env var spoofing.
#[cfg(windows)]
pub(crate) fn restrict_file_permissions(path: &std::path::Path) -> anyhow::Result<()> {
    let path_str = path.to_string_lossy();
    let sid = current_user_sid()?;
    // Remove inherited ACEs, strip Everyone and Admins, grant only current user's SID
    let output = std::process::Command::new("icacls")
        .args([
            &*path_str,
            "/inheritance:r",
            "/remove:g",
            "*S-1-1-0",
            "/remove:g",
            "*S-1-5-32-544",
            "/grant:r",
            &format!("*{sid}:F"),
        ])
        .output()?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("icacls failed on {}: {}", path_str, stderr.trim());
    }
    Ok(())
}

/// Get the current user's SID via `whoami /user /fo csv /nh`.
/// Returns a SID string like "S-1-5-21-...". Not spoofable via env vars.
#[cfg(windows)]
fn current_user_sid() -> anyhow::Result<String> {
    let output = std::process::Command::new("whoami")
        .args(["/user", "/fo", "csv", "/nh"])
        .output()?;
    if !output.status.success() {
        anyhow::bail!("whoami /user failed");
    }
    // Output format: "DOMAIN\user","S-1-5-21-..."
    let stdout = String::from_utf8_lossy(&output.stdout);
    let sid = stdout
        .trim()
        .rsplit(',')
        .next()
        .and_then(|s| s.trim().strip_prefix('"'))
        .and_then(|s| s.strip_suffix('"'))
        .ok_or_else(|| anyhow::anyhow!("Failed to parse SID from whoami output: {}", stdout))?;
    if !sid.starts_with("S-") {
        anyhow::bail!("Invalid SID format: {}", sid);
    }
    Ok(sid.to_string())
}

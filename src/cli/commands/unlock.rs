use crate::cli::{Output, Prompt};
use anyhow::Result;

pub async fn run() -> Result<()> {
    if !crate::security::has_encryption_key() {
        anyhow::bail!("No encrypted key found. Run 'tether init' first.");
    }

    if crate::security::is_unlocked() {
        Output::success("Key is already unlocked");
        return Ok(());
    }

    let passphrase = Prompt::password("Passphrase")?;
    crate::security::unlock_with_passphrase(&passphrase)?;

    Output::success("Key unlocked and cached");
    Ok(())
}

pub async fn lock() -> Result<()> {
    crate::security::clear_cached_key()?;
    Output::success("Key cache cleared");
    Ok(())
}

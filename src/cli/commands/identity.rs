use anyhow::Result;
use owo_colors::OwoColorize;

use crate::cli::output::Output;
use crate::cli::prompts::Prompt;
use crate::security::recipients;

/// Initialize a new age identity
pub async fn init() -> Result<()> {
    if recipients::has_identity() {
        Output::warning("Identity already exists");
        println!("Run 'tether identity show' to see your public key");
        println!("Run 'tether identity reset' to generate a new identity");
        return Ok(());
    }

    Output::info("Generating age identity...");

    let passphrase = Prompt::password_with_confirm(
        "Enter passphrase to protect your identity:",
        "Confirm passphrase:",
    )?;

    let identity = recipients::generate_identity();
    recipients::store_identity(&identity, &passphrase)?;

    let pubkey = recipients::get_public_key_from_identity(&identity);

    Output::success("Identity created");
    println!();
    println!("{}", "Your public key (share with team admins):".cyan());
    println!("{}", pubkey.green().bold());
    println!();
    println!(
        "{}",
        "This key is also saved to ~/.tether/identity.pub".dimmed()
    );

    Ok(())
}

/// Show public key
pub async fn show() -> Result<()> {
    let pubkey = recipients::get_public_key()?;
    println!("{}", pubkey);
    Ok(())
}

/// Unlock identity with passphrase
pub async fn unlock() -> Result<()> {
    if !recipients::has_identity() {
        Output::error("No identity found. Run 'tether identity init' first.");
        return Ok(());
    }

    if recipients::is_identity_unlocked() {
        Output::info("Identity already unlocked");
        return Ok(());
    }

    let passphrase = Prompt::password("Enter passphrase:")?;
    recipients::load_identity(Some(&passphrase))?;

    Output::success("Identity unlocked");
    Ok(())
}

/// Lock identity (clear cache)
pub async fn lock() -> Result<()> {
    recipients::clear_cached_identity()?;
    Output::success("Identity locked");
    Ok(())
}

/// Reset identity (generate new)
pub async fn reset() -> Result<()> {
    if recipients::has_identity() {
        let confirm = Prompt::confirm(
            "This will delete your existing identity. You will lose access to any team secrets encrypted to your current key. Continue?",
            false,
        )?;
        if !confirm {
            Output::info("Aborted");
            return Ok(());
        }

        // Clear existing
        let home = home::home_dir().ok_or_else(|| anyhow::anyhow!("No home directory"))?;
        let _ = std::fs::remove_file(home.join(".tether").join("identity.age"));
        let _ = std::fs::remove_file(home.join(".tether").join("identity.pub"));
        let _ = std::fs::remove_file(home.join(".tether").join("identity.cache"));
    }

    init().await
}

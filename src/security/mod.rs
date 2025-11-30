pub mod encryption;
pub mod keychain;
pub mod secrets;

pub use encryption::{decrypt_file, encrypt_file, generate_key};
pub use keychain::{
    clear_cached_key, get_encryption_key, has_encryption_key, is_unlocked,
    store_encryption_key_with_passphrase, unlock_with_passphrase,
};
pub use secrets::{scan_for_secrets, SecretFinding, SecretType};

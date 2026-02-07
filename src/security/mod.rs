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

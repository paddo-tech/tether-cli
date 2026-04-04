pub mod cli;
pub mod config;
pub mod daemon;
pub mod dashboard;
pub mod github;
pub mod packages;
pub mod security;
pub mod sync;

pub use config::Config;

pub fn home_dir() -> anyhow::Result<std::path::PathBuf> {
    home::home_dir().ok_or_else(|| anyhow::anyhow!("Could not find home directory"))
}

pub fn sha256_hex(data: &[u8]) -> String {
    use sha2::Digest;
    format!("{:x}", sha2::Sha256::digest(data))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_sha256_hex_known_value() {
        // SHA-256("abc") is a well-known test vector
        assert_eq!(
            sha256_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }
}

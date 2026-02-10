pub mod cli;
pub mod config;
pub mod daemon;
pub mod github;
pub mod packages;
pub mod security;
pub mod sync;

pub use config::Config;

pub fn home_dir() -> anyhow::Result<std::path::PathBuf> {
    home::home_dir().ok_or_else(|| anyhow::anyhow!("Could not find home directory"))
}

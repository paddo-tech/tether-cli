use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    pub sync: SyncConfig,
    pub backend: BackendConfig,
    pub packages: PackagesConfig,
    pub dotfiles: DotfilesConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SyncConfig {
    pub interval: String,
    pub strategy: ConflictStrategy,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ConflictStrategy {
    #[serde(rename = "last-write-wins")]
    LastWriteWins,
    #[serde(rename = "manual")]
    Manual,
    #[serde(rename = "machine-priority")]
    MachinePriority,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BackendConfig {
    #[serde(rename = "type")]
    pub backend_type: BackendType,
    pub url: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum BackendType {
    #[serde(rename = "git")]
    Git,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PackagesConfig {
    pub brew: BrewConfig,
    pub npm: NpmConfig,
    pub pnpm: PnpmConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BrewConfig {
    pub enabled: bool,
    pub sync_casks: bool,
    pub sync_taps: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NpmConfig {
    pub enabled: bool,
    pub sync_versions: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PnpmConfig {
    pub enabled: bool,
    pub sync_versions: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DotfilesConfig {
    pub files: Vec<String>,
}

impl Config {
    pub fn config_dir() -> Result<PathBuf> {
        let home =
            home::home_dir().ok_or_else(|| anyhow::anyhow!("Could not find home directory"))?;
        Ok(home.join(".tether"))
    }

    pub fn config_path() -> Result<PathBuf> {
        Ok(Self::config_dir()?.join("config.toml"))
    }

    pub fn load() -> Result<Self> {
        let path = Self::config_path()?;
        let content = std::fs::read_to_string(path)?;
        Ok(toml::from_str(&content)?)
    }

    pub fn save(&self) -> Result<()> {
        let dir = Self::config_dir()?;
        std::fs::create_dir_all(&dir)?;

        let path = Self::config_path()?;
        let content = toml::to_string_pretty(self)?;
        std::fs::write(path, content)?;
        Ok(())
    }
}

impl Default for Config {
    fn default() -> Self {
        Self {
            sync: SyncConfig {
                interval: "5m".to_string(),
                strategy: ConflictStrategy::LastWriteWins,
            },
            backend: BackendConfig {
                backend_type: BackendType::Git,
                url: String::new(),
            },
            packages: PackagesConfig {
                brew: BrewConfig {
                    enabled: true,
                    sync_casks: true,
                    sync_taps: true,
                },
                npm: NpmConfig {
                    enabled: true,
                    sync_versions: false,
                },
                pnpm: PnpmConfig {
                    enabled: true,
                    sync_versions: false,
                },
            },
            dotfiles: DotfilesConfig {
                files: vec![
                    ".zshrc".to_string(),
                    ".gitconfig".to_string(),
                    ".zprofile".to_string(),
                ],
            },
        }
    }
}

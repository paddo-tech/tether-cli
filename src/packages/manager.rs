use anyhow::Result;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PackageInfo {
    pub name: String,
    pub version: Option<String>,
}

#[async_trait]
pub trait PackageManager: Send + Sync {
    async fn list_installed(&self) -> Result<Vec<PackageInfo>>;
    async fn install(&self, package: &PackageInfo) -> Result<()>;
    async fn is_available(&self) -> bool;
    fn name(&self) -> &str;
}

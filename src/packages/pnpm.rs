use super::{PackageInfo, PackageManager};
use anyhow::Result;
use async_trait::async_trait;
use serde_json::Value;
use tokio::process::Command;

pub struct PnpmManager;

impl PnpmManager {
    pub fn new() -> Self {
        Self
    }

    async fn run_pnpm(&self, args: &[&str]) -> Result<String> {
        let output = Command::new("pnpm").args(args).output().await?;

        if !output.status.success() {
            return Err(anyhow::anyhow!(
                "pnpm command failed: {}",
                pnpm_error_message(&output.stderr, &output.stdout)
            ));
        }

        Ok(String::from_utf8(output.stdout)?)
    }
}

impl Default for PnpmManager {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl PackageManager for PnpmManager {
    async fn list_installed(&self) -> Result<Vec<PackageInfo>> {
        let output = self
            .run_pnpm(&["list", "-g", "--depth=0", "--json"])
            .await?;

        let json: Value = serde_json::from_str(&output)?;
        let mut packages = Vec::new();

        if let Value::Array(entries) = json {
            for entry in entries {
                if let Some(deps) = entry.get("dependencies").and_then(Value::as_object) {
                    for (name, dep_info) in deps {
                        if name == "pnpm" {
                            continue;
                        }

                        let version = dep_info
                            .get("version")
                            .and_then(Value::as_str)
                            .map(|s| s.to_string());

                        packages.push(PackageInfo {
                            name: name.to_string(),
                            version,
                        });
                    }
                }
            }
        }

        packages.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(packages)
    }

    async fn install(&self, package: &PackageInfo) -> Result<()> {
        let pkg_spec = if let Some(version) = &package.version {
            format!("{}@{}", package.name, version)
        } else {
            package.name.clone()
        };

        self.run_pnpm(&["add", "-g", &pkg_spec]).await?;
        Ok(())
    }

    async fn is_available(&self) -> bool {
        which::which("pnpm").is_ok()
    }

    fn name(&self) -> &str {
        "pnpm"
    }

    async fn update_all(&self) -> Result<()> {
        let packages = self.list_installed().await?;
        if packages.is_empty() {
            return Ok(());
        }

        let output = Command::new("pnpm").args(["update", "-g"]).output().await?;

        if !output.status.success() {
            return Err(anyhow::anyhow!(
                "pnpm update failed: {}",
                pnpm_error_message(&output.stderr, &output.stdout)
            ));
        }

        Ok(())
    }

    async fn uninstall(&self, package: &str) -> Result<()> {
        let output = Command::new("pnpm")
            .args(["remove", "-g", package])
            .output()
            .await?;

        if !output.status.success() {
            return Err(anyhow::anyhow!(
                "pnpm remove failed: {}",
                pnpm_error_message(&output.stderr, &output.stdout)
            ));
        }

        Ok(())
    }
}

/// pnpm writes failures to stdout, not stderr — fall back when stderr is empty.
fn pnpm_error_message(stderr: &[u8], stdout: &[u8]) -> String {
    let err = String::from_utf8_lossy(stderr);
    let trimmed = err.trim();
    if !trimmed.is_empty() {
        return trimmed.to_string();
    }
    String::from_utf8_lossy(stdout).trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prefers_trimmed_stderr() {
        assert_eq!(pnpm_error_message(b"  boom  ", b"ignored"), "boom");
    }

    #[test]
    fn falls_back_to_stdout_when_stderr_blank() {
        assert_eq!(pnpm_error_message(b"   \n", b"  ENOENT  "), "ENOENT");
    }

    #[test]
    fn empty_when_both_blank() {
        assert_eq!(pnpm_error_message(b"", b"  "), "");
    }
}

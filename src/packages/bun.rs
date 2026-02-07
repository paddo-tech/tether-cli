use super::{PackageInfo, PackageManager};
use anyhow::Result;
use async_trait::async_trait;
use tokio::process::Command;

/// Parse a package@version string, handling scoped packages like @scope/pkg@version
fn parse_package_version(s: &str) -> (String, Option<String>) {
    if let Some(last_at) = s.rfind('@') {
        // Check it's not the @ in a scoped package name (e.g., @google/pkg)
        if last_at > 0 && !s[..last_at].ends_with('/') {
            return (s[..last_at].to_string(), Some(s[last_at + 1..].to_string()));
        }
    }
    (s.to_string(), None)
}

pub struct BunManager;

impl BunManager {
    pub fn new() -> Self {
        Self
    }

    async fn run_bun(&self, args: &[&str]) -> Result<String> {
        let output = Command::new("bun").args(args).output().await?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(anyhow::anyhow!("bun command failed: {}", stderr));
        }

        Ok(String::from_utf8(output.stdout)?)
    }
}

impl Default for BunManager {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl PackageManager for BunManager {
    async fn list_installed(&self) -> Result<Vec<PackageInfo>> {
        let output = match self.run_bun(&["pm", "ls", "-g"]).await {
            Ok(out) => out,
            Err(err) => {
                let message = err.to_string();
                if message.contains("No package.json was found") {
                    // Bun hasn't created the global install metadata yet.
                    // Treat this as "no global packages" instead of failing the sync.
                    return Ok(Vec::new());
                }
                return Err(err);
            }
        };

        let mut packages = Vec::new();

        // Parse tree output from `bun pm ls -g`:
        // /Users/paddo/.bun/install/global node_modules (535)
        // └── @google/gemini-cli@0.18.4
        for line in output.lines() {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }

            // Skip header line (contains "node_modules")
            if line.contains("node_modules") {
                continue;
            }

            // Remove tree prefixes (├── └── │)
            let cleaned = line
                .trim_start_matches("├──")
                .trim_start_matches("└──")
                .trim_start_matches("│")
                .trim();

            if cleaned.is_empty() {
                continue;
            }

            // Parse package@version format
            // Handle scoped packages like @google/gemini-cli@0.18.4
            let (name, version) = parse_package_version(cleaned);

            // Skip invalid entries (e.g. bare "@" from malformed output)
            if name.is_empty() || name == "@" {
                continue;
            }

            packages.push(PackageInfo { name, version });
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

        self.run_bun(&["add", "-g", &pkg_spec]).await?;
        Ok(())
    }

    async fn is_available(&self) -> bool {
        which::which("bun").is_ok()
    }

    fn name(&self) -> &str {
        "bun"
    }

    async fn update_all(&self) -> Result<()> {
        let packages = self.list_installed().await?;
        if packages.is_empty() {
            return Ok(());
        }

        // bun update -g is broken (only updates first package)
        // Workaround: reinstall each package to get latest version
        for pkg in packages {
            let output = Command::new("bun")
                .args(["add", "-g", &pkg.name])
                .output()
                .await?;

            if !output.status.success() {
                let stderr = String::from_utf8_lossy(&output.stderr);
                eprintln!("Warning: Failed to update {}: {}", pkg.name, stderr);
            }
        }

        Ok(())
    }

    async fn uninstall(&self, package: &str) -> Result<()> {
        let output = Command::new("bun")
            .args(["remove", "-g", package])
            .output()
            .await?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(anyhow::anyhow!("bun remove failed: {}", stderr));
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_package_version_simple() {
        let (name, version) = parse_package_version("typescript@5.3.3");
        assert_eq!(name, "typescript");
        assert_eq!(version, Some("5.3.3".to_string()));
    }

    #[test]
    fn test_parse_package_version_scoped() {
        let (name, version) = parse_package_version("@google/gemini-cli@0.18.4");
        assert_eq!(name, "@google/gemini-cli");
        assert_eq!(version, Some("0.18.4".to_string()));
    }

    #[test]
    fn test_parse_package_version_scoped_deep() {
        let (name, version) = parse_package_version("@angular/cli@17.0.0");
        assert_eq!(name, "@angular/cli");
        assert_eq!(version, Some("17.0.0".to_string()));
    }

    #[test]
    fn test_parse_package_version_no_version() {
        let (name, version) = parse_package_version("typescript");
        assert_eq!(name, "typescript");
        assert_eq!(version, None);
    }

    #[test]
    fn test_parse_package_version_scoped_no_version() {
        let (name, version) = parse_package_version("@types/node");
        assert_eq!(name, "@types/node");
        assert_eq!(version, None);
    }
}

use anyhow::{Context, Result};
use tokio::process::Command;

/// GitHub CLI integration for automatic repository setup
pub struct GitHubCli;

impl GitHubCli {
    /// Check if gh CLI is installed
    pub fn is_installed() -> bool {
        which::which("gh").is_ok()
    }

    /// Install gh CLI via Homebrew
    pub async fn install() -> Result<()> {
        let output = Command::new("brew")
            .args(["install", "gh"])
            .output()
            .await
            .context("Failed to run brew install gh")?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(anyhow::anyhow!("Failed to install gh: {}", stderr));
        }

        Ok(())
    }

    /// Check if user is authenticated with GitHub
    pub async fn is_authenticated() -> Result<bool> {
        let output = Command::new("gh")
            .args(["auth", "status"])
            .output()
            .await
            .context("Failed to check gh auth status")?;

        Ok(output.status.success())
    }

    /// Authenticate with GitHub (opens browser)
    pub async fn authenticate() -> Result<()> {
        let status = Command::new("gh")
            .args(["auth", "login", "--web"])
            .status()
            .await
            .context("Failed to run gh auth login")?;

        if !status.success() {
            return Err(anyhow::anyhow!("GitHub authentication failed"));
        }

        Ok(())
    }

    /// Get authenticated GitHub username
    pub async fn get_username() -> Result<String> {
        let output = Command::new("gh")
            .args(["api", "user", "--jq", ".login"])
            .output()
            .await
            .context("Failed to get GitHub username")?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(anyhow::anyhow!("Failed to get username: {}", stderr));
        }

        Ok(String::from_utf8(output.stdout)?.trim().to_string())
    }

    /// Check if a repository exists
    pub async fn repo_exists(owner: &str, repo: &str) -> Result<bool> {
        let repo_spec = format!("{}/{}", owner, repo);
        let output = Command::new("gh")
            .args(["repo", "view", &repo_spec])
            .output()
            .await?;

        Ok(output.status.success())
    }

    /// Create a new private GitHub repository
    pub async fn create_repo(name: &str, private: bool) -> Result<String> {
        let mut args = vec!["repo", "create", name, "--clone=false"];

        if private {
            args.push("--private");
        } else {
            args.push("--public");
        }

        let output = Command::new("gh")
            .args(&args)
            .output()
            .await
            .context("Failed to create GitHub repository")?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(anyhow::anyhow!("Failed to create repo: {}", stderr));
        }

        // Get the repo URL - use SSH format for authentication
        let username = Self::get_username().await?;
        Ok(format!("git@github.com:{}/{}.git", username, name))
    }

    /// Suggest alternative repository names if the desired name is taken
    pub async fn suggest_repo_name(base_name: &str, owner: &str) -> Result<String> {
        let mut name = base_name.to_string();
        let mut counter = 1;

        while Self::repo_exists(owner, &name).await? {
            counter += 1;
            name = format!("{}-{}", base_name, counter);
        }

        Ok(name)
    }

    /// Check if SSH key is configured with GitHub
    pub async fn check_ssh_access() -> Result<bool> {
        let output = Command::new("ssh")
            .args(["-T", "git@github.com"])
            .output()
            .await?;

        // GitHub returns exit code 1 even on success with a message
        // "Hi username! You've successfully authenticated..."
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);

        Ok(stdout.contains("successfully authenticated")
            || stderr.contains("successfully authenticated"))
    }

    /// Setup SSH key with GitHub using gh CLI
    pub async fn setup_ssh_key() -> Result<()> {
        // Use gh CLI to add SSH key
        let status = Command::new("gh").args(["ssh-key", "add"]).status().await?;

        if !status.success() {
            return Err(anyhow::anyhow!("Failed to add SSH key"));
        }

        Ok(())
    }
}

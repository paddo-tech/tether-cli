use anyhow::Result;
use regex::Regex;
use std::path::Path;
use std::sync::LazyLock;

#[derive(Debug, Clone)]
pub enum SecretType {
    AwsAccessKey,
    AwsSecretKey,
    GitHubToken,
    GitHubPat,
    ApiKey,
    PrivateKey,
    Password,
    DatabaseUrl,
    BearerToken,
    HighEntropy,
}

impl SecretType {
    pub fn description(&self) -> &str {
        match self {
            SecretType::AwsAccessKey => "AWS Access Key",
            SecretType::AwsSecretKey => "AWS Secret Key",
            SecretType::GitHubToken => "GitHub Token",
            SecretType::GitHubPat => "GitHub Personal Access Token",
            SecretType::ApiKey => "API Key",
            SecretType::PrivateKey => "Private Key",
            SecretType::Password => "Password",
            SecretType::DatabaseUrl => "Database URL with credentials",
            SecretType::BearerToken => "Bearer Token",
            SecretType::HighEntropy => "High-entropy string (possible secret)",
        }
    }
}

#[derive(Debug, Clone)]
pub struct SecretFinding {
    pub line_number: usize,
    pub secret_type: SecretType,
    pub context: String, // Redacted line preview
}

pub struct SecretScanner {
    patterns: Vec<(Regex, SecretType)>,
}

impl SecretScanner {
    pub fn new() -> Result<Self> {
        let patterns = vec![
            // AWS keys
            (Regex::new(r"AKIA[0-9A-Z]{16}")?, SecretType::AwsAccessKey),
            (
                Regex::new(r#"(?i)aws_secret_access_key\s*[=:]\s*['"]?([A-Za-z0-9/+=]{40})['"]?"#)?,
                SecretType::AwsSecretKey,
            ),
            // GitHub tokens
            (Regex::new(r"ghp_[a-zA-Z0-9]{36}")?, SecretType::GitHubToken),
            (Regex::new(r"gho_[a-zA-Z0-9]{36}")?, SecretType::GitHubPat),
            (
                Regex::new(r"github_pat_[a-zA-Z0-9]{22}_[a-zA-Z0-9]{59}")?,
                SecretType::GitHubPat,
            ),
            // API keys (generic patterns)
            (
                Regex::new(r#"(?i)(api[_-]?key|apikey)\s*[=:]\s*['"]([a-zA-Z0-9_\-]{20,})['"]"#)?,
                SecretType::ApiKey,
            ),
            // Private keys
            (
                Regex::new(r"-----BEGIN\s+(RSA\s+)?PRIVATE KEY-----")?,
                SecretType::PrivateKey,
            ),
            (
                Regex::new(r"-----BEGIN\s+OPENSSH PRIVATE KEY-----")?,
                SecretType::PrivateKey,
            ),
            // Passwords
            (
                Regex::new(r#"(?i)(password|passwd|pwd)\s*[=:]\s*['"]([^'"]{8,})['"]"#)?,
                SecretType::Password,
            ),
            // Database URLs with credentials
            (
                Regex::new(r"(?i)(postgres|mysql|mongodb)://[^:]+:[^@]+@")?,
                SecretType::DatabaseUrl,
            ),
            // Bearer tokens
            (
                Regex::new(r"Bearer\s+[a-zA-Z0-9\-._~+/]+=*")?,
                SecretType::BearerToken,
            ),
            // High-entropy strings (potential secrets)
            (
                Regex::new(r#"['"]([a-zA-Z0-9+/]{32,}={0,2})['"]"#)?,
                SecretType::HighEntropy,
            ),
        ];

        Ok(Self { patterns })
    }

    /// Findings in `content`. A line that holds one of the `ignored` patterns is skipped.
    pub fn scan_content(&self, content: &str, ignored: &[String]) -> Vec<SecretFinding> {
        let mut findings = Vec::new();

        for (line_num, line) in content.lines().enumerate() {
            if ignored.iter().any(|p| line.contains(p.as_str())) {
                continue;
            }
            for (pattern, secret_type) in &self.patterns {
                if pattern.is_match(line) {
                    // Redact the actual secret value for display
                    let redacted = Self::redact_line(line);

                    findings.push(SecretFinding {
                        line_number: line_num + 1,
                        secret_type: secret_type.clone(),
                        context: redacted,
                    });

                    // Only report one finding per line
                    break;
                }
            }
        }

        findings
    }

    fn redact_line(line: &str) -> String {
        static REDACT_RE: LazyLock<Regex> =
            LazyLock::new(|| Regex::new(r#"[=:]\s*['"]?([a-zA-Z0-9+/=_\-]{8,})['"]?"#).unwrap());

        let redacted = REDACT_RE.replace_all(line, "=***REDACTED***");

        // Truncate if too long
        if redacted.len() > 80 {
            format!("{}...", &redacted[..77])
        } else {
            redacted.to_string()
        }
    }
}

impl Default for SecretScanner {
    fn default() -> Self {
        Self::new().expect("Failed to create secret scanner")
    }
}

static GLOBAL_SCANNER: LazyLock<SecretScanner> = LazyLock::new(SecretScanner::default);

/// Patterns that `tether ignore secrets` keeps, one per line.
pub fn ignore_file_path() -> Result<std::path::PathBuf> {
    Ok(crate::config::Config::config_dir()?.join("ignore"))
}

/// The patterns in the ignore file. Without the file there are none.
pub fn ignored_patterns() -> Result<Vec<String>> {
    match std::fs::read_to_string(ignore_file_path()?) {
        Ok(text) => Ok(text
            .lines()
            .filter(|l| !l.is_empty())
            .map(str::to_string)
            .collect()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(e) => Err(e.into()),
    }
}

/// Findings in a team dotfile, which the team scan checks before a push. Personal dotfiles
/// reach Git only encrypted, so only team files are scanned. None when
/// `security.scan_secrets` is off.
pub fn scan_team_file(
    config: &crate::config::Config,
    ignored: &[String],
    file_path: &Path,
) -> Result<Vec<SecretFinding>> {
    if !config.security.scan_secrets {
        return Ok(Vec::new());
    }
    let content = std::fs::read_to_string(file_path)?;
    Ok(GLOBAL_SCANNER.scan_content(&content, ignored))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_aws_key_detection() {
        let scanner = SecretScanner::new().unwrap();
        let content = "export AWS_ACCESS_KEY_ID=AKIAIOSFODNN7EXAMPLE";
        let findings = scanner.scan_content(content, &[]);
        assert!(!findings.is_empty());
        assert!(matches!(findings[0].secret_type, SecretType::AwsAccessKey));
    }

    #[test]
    fn test_github_token_detection() {
        let scanner = SecretScanner::new().unwrap();
        let content = "GITHUB_TOKEN=ghp_123456789012345678901234567890123456";
        let findings = scanner.scan_content(content, &[]);
        assert!(!findings.is_empty());
        assert!(matches!(findings[0].secret_type, SecretType::GitHubToken));
    }

    #[test]
    fn test_private_key_detection() {
        let scanner = SecretScanner::new().unwrap();
        let content = "-----BEGIN RSA PRIVATE KEY-----\nMIIEpAIBAAKCAQEA...";
        let findings = scanner.scan_content(content, &[]);
        assert!(!findings.is_empty());
        assert!(matches!(findings[0].secret_type, SecretType::PrivateKey));
    }

    #[test]
    fn an_ignored_pattern_skips_its_line() {
        let scanner = SecretScanner::new().unwrap();
        let content =
            "GITHUB_TOKEN=ghp_123456789012345678901234567890123456\nAWS=AKIAIOSFODNN7EXAMPLE";
        let findings = scanner.scan_content(content, &["GITHUB_TOKEN=".to_string()]);
        assert_eq!(findings.len(), 1);
        assert!(matches!(findings[0].secret_type, SecretType::AwsAccessKey));
    }

    #[test]
    fn test_no_false_positives() {
        let scanner = SecretScanner::new().unwrap();
        let content = "export PATH=/usr/local/bin:$PATH";
        let findings = scanner.scan_content(content, &[]);
        assert!(findings.is_empty());
    }
}

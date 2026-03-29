use super::{PackageInfo, PackageManager};
use anyhow::Result;
use async_trait::async_trait;
use tokio::process::Command;

pub struct WingetManager;

impl WingetManager {
    pub fn new() -> Self {
        Self
    }

    fn winget_cmd(args: &[&str]) -> Command {
        let mut cmd = Command::new(super::resolve_program("winget"));
        cmd.args(args);
        cmd
    }

    async fn run_winget(&self, args: &[&str]) -> Result<String> {
        let output = Self::winget_cmd(args).output().await?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(anyhow::anyhow!("winget command failed: {}", stderr));
        }

        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    }
}

impl Default for WingetManager {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl PackageManager for WingetManager {
    async fn list_installed(&self) -> Result<Vec<PackageInfo>> {
        let output = self
            .run_winget(&["list", "--source", "winget", "--disable-interactivity"])
            .await?;

        let packages = parse_winget_list(&output);
        Ok(packages)
    }

    async fn install(&self, package: &PackageInfo) -> Result<()> {
        let mut args = vec![
            "install",
            "--id",
            &package.name,
            "-e",
            "--disable-interactivity",
            "--accept-source-agreements",
            "--accept-package-agreements",
        ];
        let version_str;
        if let Some(version) = &package.version {
            version_str = version.clone();
            args.extend(["--version", &version_str]);
        }
        self.run_winget(&args).await?;
        Ok(())
    }

    async fn is_available(&self) -> bool {
        which::which("winget").is_ok()
    }

    fn name(&self) -> &str {
        "winget"
    }

    async fn export_manifest(&self) -> Result<String> {
        let packages = self.list_installed().await?;
        let manifest = packages
            .iter()
            .map(|p| p.name.as_str())
            .collect::<Vec<_>>()
            .join("\n");
        Ok(manifest)
    }

    async fn import_manifest(&self, manifest_content: &str) -> Result<()> {
        let package_ids: Vec<&str> = manifest_content
            .lines()
            .map(|line| line.trim())
            .filter(|line| !line.is_empty())
            .collect();

        if package_ids.is_empty() {
            return Ok(());
        }

        let installed = self.list_installed().await?;
        let installed_ids: std::collections::HashSet<_> =
            installed.iter().map(|p| p.name.to_lowercase()).collect();

        for id in package_ids {
            if !installed_ids.contains(&id.to_lowercase()) {
                let output = Self::winget_cmd(&[
                    "install",
                    "--id",
                    id,
                    "-e",
                    "--disable-interactivity",
                    "--accept-source-agreements",
                    "--accept-package-agreements",
                ])
                .output()
                .await?;

                if !output.status.success() {
                    let stderr = String::from_utf8_lossy(&output.stderr);
                    eprintln!("Warning: Failed to install {}: {}", id, stderr);
                }
            }
        }

        Ok(())
    }

    async fn remove_unlisted(&self, manifest_content: &str) -> Result<()> {
        let desired: std::collections::HashSet<String> = manifest_content
            .lines()
            .map(|l| l.trim().to_lowercase())
            .filter(|l| !l.is_empty())
            .collect();

        if desired.is_empty() {
            return Ok(());
        }

        let installed = self.list_installed().await?;

        for pkg in installed {
            if !desired.contains(&pkg.name.to_lowercase()) {
                let output = Self::winget_cmd(&[
                    "uninstall",
                    "--id",
                    &pkg.name,
                    "-e",
                    "--disable-interactivity",
                ])
                .output()
                .await?;

                if !output.status.success() {
                    let stderr = String::from_utf8_lossy(&output.stderr);
                    eprintln!("Warning: Failed to uninstall {}: {}", pkg.name, stderr);
                }
            }
        }

        Ok(())
    }

    async fn update_all(&self) -> Result<()> {
        let output = Self::winget_cmd(&[
            "upgrade",
            "--all",
            "--disable-interactivity",
            "--accept-source-agreements",
            "--accept-package-agreements",
        ])
        .output()
        .await?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(anyhow::anyhow!("winget upgrade failed: {}", stderr));
        }

        Ok(())
    }

    async fn uninstall(&self, package: &str) -> Result<()> {
        let output = Self::winget_cmd(&[
            "uninstall",
            "--id",
            package,
            "-e",
            "--disable-interactivity",
        ])
        .output()
        .await?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(anyhow::anyhow!("winget uninstall failed: {}", stderr));
        }

        Ok(())
    }
}

/// Approximate display width of a char. CJK ideographs and fullwidth forms are 2 columns.
fn display_width(c: char) -> usize {
    let cp = c as u32;
    if (0x1100..=0x115F).contains(&cp)    // Hangul Jamo
        || (0x2E80..=0x303E).contains(&cp)  // CJK radicals, symbols
        || (0x3040..=0x33BF).contains(&cp)  // Hiragana, Katakana, CJK compat
        || (0x3400..=0x4DBF).contains(&cp)  // CJK Extension A
        || (0x4E00..=0x9FFF).contains(&cp)  // CJK Unified Ideographs
        || (0xA960..=0xA97C).contains(&cp)  // Hangul Jamo Extended-A
        || (0xAC00..=0xD7A3).contains(&cp)  // Hangul Syllables
        || (0xF900..=0xFAFF).contains(&cp)  // CJK Compatibility Ideographs
        || (0xFE10..=0xFE6F).contains(&cp)  // CJK compatibility forms, small forms
        || (0xFF01..=0xFF60).contains(&cp)  // Fullwidth forms
        || (0xFFE0..=0xFFE6).contains(&cp)  // Fullwidth signs
        || (0x20000..=0x2FA1F).contains(&cp)
    // CJK extensions B-F, compat supplement
    {
        2
    } else {
        1
    }
}

/// Slice a string by display column position, returning the substring between [start, end).
fn slice_by_display_col(s: &str, start: usize, end: usize) -> &str {
    let mut col = 0;
    let mut byte_start = s.len();
    let mut byte_end = s.len();
    for (i, c) in s.char_indices() {
        if col >= end {
            byte_end = i;
            break;
        }
        if col >= start && byte_start == s.len() {
            byte_start = i;
        }
        col += display_width(c);
    }
    if byte_start > s.len() {
        return "";
    }
    &s[byte_start..byte_end]
}

/// Parse `winget list` fixed-width column output.
/// Column positions are derived from the separator line (dashes with spaces), which is
/// locale-independent. Data lines are sliced by display width to handle non-ASCII
/// package names (e.g., CJK double-width characters).
fn parse_winget_list(output: &str) -> Vec<PackageInfo> {
    // winget emits \r-based progress spinners; take only content after the last \r per line
    let lines: Vec<&str> = output
        .lines()
        .map(|l| l.rsplit('\r').next().unwrap_or(l))
        .collect();

    // Find the separator line (all dashes). This is locale-independent.
    let Some(sep_idx) = lines.iter().position(|l| {
        let trimmed = l.trim();
        !trimmed.is_empty() && trimmed.chars().all(|c| c == '-')
    }) else {
        return Vec::new();
    };

    // The header is the line before the separator. Derive column positions from it
    // using display-width columns (not byte offsets) to handle non-ASCII headers.
    // Column boundaries: a space followed by a non-space, preceded by at least 2 spaces.
    if sep_idx == 0 {
        return Vec::new();
    }
    let header = lines[sep_idx - 1];
    let col_starts: Vec<usize> = {
        let mut starts = vec![0usize]; // first column always starts at 0
        let chars: Vec<char> = header.chars().collect();
        let mut col = 0;
        let mut prev_was_space = false;
        let mut prev2_was_space = false;
        for &c in &chars {
            if c != ' ' && prev_was_space && prev2_was_space {
                starts.push(col);
            }
            prev2_was_space = prev_was_space;
            prev_was_space = c == ' ';
            col += display_width(c);
        }
        starts
    };

    // Need at least 3 columns: Name, Id, Version
    if col_starts.len() < 3 {
        return Vec::new();
    }
    let id_col = col_starts[1];
    let version_col = col_starts[2];
    let data_start = sep_idx + 1;

    let mut packages = Vec::new();
    for line in lines.iter().skip(data_start) {
        if line.trim().is_empty() {
            continue;
        }
        let id = slice_by_display_col(line, id_col, version_col).trim();
        let version = {
            let rest = slice_by_display_col(line, version_col, usize::MAX).trim();
            let v = rest.split_whitespace().next().unwrap_or("");
            if v.is_empty() {
                None
            } else {
                Some(v.to_string())
            }
        };
        if !id.is_empty() {
            packages.push(PackageInfo {
                name: id.to_string(),
                version,
            });
        }
    }

    packages.sort_by(|a, b| a.name.cmp(&b.name));
    packages
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_winget_list_basic() {
        let output = "\
Name                            Id                          Version   Available Source
-----------------------------------------------------------------------------------------------
Git                             Git.Git                     2.43.0    2.44.0    winget
Visual Studio Code              Microsoft.VisualStudioCode  1.87.0              winget
Microsoft Edge                  Microsoft.Edge              122.0     123.0     winget";

        let packages = parse_winget_list(output);
        assert_eq!(packages.len(), 3);
        assert_eq!(packages[0].name, "Git.Git");
        assert_eq!(packages[0].version, Some("2.43.0".to_string()));
        assert_eq!(packages[1].name, "Microsoft.Edge");
        assert_eq!(packages[2].name, "Microsoft.VisualStudioCode");
        assert_eq!(packages[2].version, Some("1.87.0".to_string()));
    }

    #[test]
    fn test_parse_winget_list_with_preamble() {
        let output = "\
Some winget preamble text
Another line of output
Name                Id                Version  Source
-----------------------------------------------------
Git                 Git.Git           2.43.0   winget";

        let packages = parse_winget_list(output);
        assert_eq!(packages.len(), 1);
        assert_eq!(packages[0].name, "Git.Git");
    }

    #[test]
    fn test_parse_winget_list_empty() {
        let packages = parse_winget_list("");
        assert!(packages.is_empty());
    }

    #[test]
    fn test_parse_winget_list_no_header() {
        let packages = parse_winget_list("some random output\nwith no header");
        assert!(packages.is_empty());
    }

    #[test]
    fn test_parse_winget_list_localized_headers() {
        // Japanese locale: headers are translated but separator line is universal
        let output = "\
名前                            ID                          バージョン 利用可能  ソース
-----------------------------------------------------------------------------------------------
Git                             Git.Git                     2.43.0    2.44.0    winget
Visual Studio Code              Microsoft.VisualStudioCode  1.87.0              winget";

        let packages = parse_winget_list(output);
        assert_eq!(packages.len(), 2);
        assert_eq!(packages[0].name, "Git.Git");
        assert_eq!(packages[1].name, "Microsoft.VisualStudioCode");
    }

    #[test]
    fn test_parse_winget_list_non_ascii_names() {
        // CJK chars are double-width in terminal display; winget aligns by display columns.
        // "日本語App" = 3 CJK (6 cols) + 3 ASCII (3 cols) = 9 display cols
        // Header "Id" starts at display column 20, so pad to 20.
        let output = "\
Name                Id                  Version
-------------------------------------------------
日本語App           Editor.Japanese     2.1.0
Блокнот             Notepad.App         1.0.0";

        let packages = parse_winget_list(output);
        assert_eq!(packages.len(), 2);
        assert_eq!(packages[0].name, "Editor.Japanese");
        assert_eq!(packages[1].name, "Notepad.App");
    }
}

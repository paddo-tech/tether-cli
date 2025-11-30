use regex::Regex;
use std::collections::HashSet;
use std::path::{Path, PathBuf};

/// Discover directories that should be synced based on shell config files.
/// Parses .zshrc, .bashrc, etc. for `source` or `.` commands that reference directories.
pub fn discover_sourced_dirs(home: &Path, dotfiles: &[String]) -> Vec<String> {
    let mut discovered = HashSet::new();

    for dotfile in dotfiles {
        let path = home.join(dotfile);
        if !path.exists() {
            continue;
        }

        if let Ok(content) = std::fs::read_to_string(&path) {
            for dir in parse_sourced_dirs(&content, home) {
                // Convert to relative path from home with ~/ prefix (e.g., "~/.config/zsh")
                if let Ok(rel) = dir.strip_prefix(home) {
                    let rel_str = format!("~/{}", rel.to_string_lossy());
                    // Only include if it's a directory that exists
                    if dir.is_dir() {
                        discovered.insert(rel_str);
                    }
                }
            }
        }
    }

    let mut result: Vec<_> = discovered.into_iter().collect();
    result.sort();
    result
}

/// Parse a shell config file and extract directories being sourced.
fn parse_sourced_dirs(content: &str, home: &Path) -> Vec<PathBuf> {
    let mut dirs = Vec::new();

    // Patterns to match:
    // - source ~/.config/zsh/*.zsh
    // - . ~/.config/zsh/*.zsh
    // - for script in ~/.config/zsh/*.zsh(N); do source "$script"; done
    // - [ -f ~/.fzf.zsh ] && source ~/.fzf.zsh (single file, skip)

    // Match glob patterns like ~/.config/zsh/*.zsh or $HOME/.config/zsh/*.zsh
    let glob_pattern = Regex::new(
        r#"(?:source|\.)\s+["']?((?:~|\$HOME)/[^\s"'*]+)/\*\.[a-z]+"#
    ).unwrap();

    // Match for loops: for x in ~/.config/zsh/*.zsh or ~/.config/zsh/*.zsh(N)
    // The (N) is a zsh glob qualifier
    let for_loop_pattern = Regex::new(
        r#"for\s+\w+\s+in\s+["']?((?:~|\$HOME)/[^\s"'*(]+)/\*\.[a-z]+(?:\([A-Z]+\))?"#
    ).unwrap();

    for cap in glob_pattern.captures_iter(content) {
        if let Some(dir_match) = cap.get(1) {
            if let Some(expanded) = expand_path(dir_match.as_str(), home) {
                dirs.push(expanded);
            }
        }
    }

    for cap in for_loop_pattern.captures_iter(content) {
        if let Some(dir_match) = cap.get(1) {
            if let Some(expanded) = expand_path(dir_match.as_str(), home) {
                dirs.push(expanded);
            }
        }
    }

    dirs
}

/// Expand ~ or $HOME to actual home directory
fn expand_path(path: &str, home: &Path) -> Option<PathBuf> {
    path.strip_prefix("~/")
        .or_else(|| path.strip_prefix("$HOME/"))
        .map(|stripped| home.join(stripped))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_sourced_dirs() {
        let home = PathBuf::from("/Users/test");

        let content = r#"
# Source all zsh scripts
for script in ~/.config/zsh/*.zsh(N); do
  source "$script"
done

# Also source bash completions
source ~/.config/bash/*.sh
"#;

        let dirs = parse_sourced_dirs(content, &home);
        assert!(dirs.contains(&PathBuf::from("/Users/test/.config/zsh")));
        assert!(dirs.contains(&PathBuf::from("/Users/test/.config/bash")));
    }

    #[test]
    fn test_expand_path() {
        let home = PathBuf::from("/Users/test");

        assert_eq!(
            expand_path("~/.config/zsh", &home),
            Some(PathBuf::from("/Users/test/.config/zsh"))
        );
        assert_eq!(
            expand_path("$HOME/.config/zsh", &home),
            Some(PathBuf::from("/Users/test/.config/zsh"))
        );
        assert_eq!(expand_path("/absolute/path", &home), None);
    }
}

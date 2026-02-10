use std::collections::HashMap;

pub struct PackageMapping {
    pub brew_formula: Option<&'static str>,
    pub brew_cask: Option<&'static str>,
    pub winget: Option<&'static str>,
}

/// Built-in cross-platform package mappings.
fn default_mappings() -> Vec<PackageMapping> {
    vec![
        // CLI tools
        PackageMapping {
            brew_formula: Some("git"),
            brew_cask: None,
            winget: Some("Git.Git"),
        },
        PackageMapping {
            brew_formula: Some("curl"),
            brew_cask: None,
            winget: Some("cURL.cURL"),
        },
        PackageMapping {
            brew_formula: Some("wget"),
            brew_cask: None,
            winget: Some("JernejSimoncic.Wget"),
        },
        PackageMapping {
            brew_formula: Some("jq"),
            brew_cask: None,
            winget: Some("jqlang.jq"),
        },
        PackageMapping {
            brew_formula: Some("gh"),
            brew_cask: None,
            winget: Some("GitHub.cli"),
        },
        PackageMapping {
            brew_formula: Some("ripgrep"),
            brew_cask: None,
            winget: Some("BurntSushi.ripgrep.MSVC"),
        },
        PackageMapping {
            brew_formula: Some("fd"),
            brew_cask: None,
            winget: Some("sharkdp.fd"),
        },
        PackageMapping {
            brew_formula: Some("bat"),
            brew_cask: None,
            winget: Some("sharkdp.bat"),
        },
        PackageMapping {
            brew_formula: Some("fzf"),
            brew_cask: None,
            winget: Some("junegunn.fzf"),
        },
        PackageMapping {
            brew_formula: Some("tree"),
            brew_cask: None,
            winget: Some("IDRIX.Tree"),
        },
        PackageMapping {
            brew_formula: Some("cmake"),
            brew_cask: None,
            winget: Some("Kitware.CMake"),
        },
        // Languages & runtimes
        PackageMapping {
            brew_formula: Some("node"),
            brew_cask: None,
            winget: Some("OpenJS.NodeJS.LTS"),
        },
        PackageMapping {
            brew_formula: Some("python"),
            brew_cask: None,
            winget: Some("Python.Python.3"),
        },
        PackageMapping {
            brew_formula: Some("go"),
            brew_cask: None,
            winget: Some("GoLang.Go"),
        },
        PackageMapping {
            brew_formula: Some("rustup"),
            brew_cask: None,
            winget: Some("Rustlang.Rustup"),
        },
        // Cask ↔ winget (GUI apps)
        PackageMapping {
            brew_formula: None,
            brew_cask: Some("docker"),
            winget: Some("Docker.DockerDesktop"),
        },
        PackageMapping {
            brew_formula: None,
            brew_cask: Some("firefox"),
            winget: Some("Mozilla.Firefox"),
        },
        PackageMapping {
            brew_formula: None,
            brew_cask: Some("google-chrome"),
            winget: Some("Google.Chrome"),
        },
        PackageMapping {
            brew_formula: None,
            brew_cask: Some("visual-studio-code"),
            winget: Some("Microsoft.VisualStudioCode"),
        },
        PackageMapping {
            brew_formula: None,
            brew_cask: Some("slack"),
            winget: Some("SlackTechnologies.Slack"),
        },
        PackageMapping {
            brew_formula: None,
            brew_cask: Some("discord"),
            winget: Some("Discord.Discord"),
        },
        PackageMapping {
            brew_formula: None,
            brew_cask: Some("spotify"),
            winget: Some("Spotify.Spotify"),
        },
        PackageMapping {
            brew_formula: None,
            brew_cask: Some("1password"),
            winget: Some("AgileBits.1Password"),
        },
        PackageMapping {
            brew_formula: None,
            brew_cask: Some("notion"),
            winget: Some("Notion.Notion"),
        },
        PackageMapping {
            brew_formula: None,
            brew_cask: Some("obsidian"),
            winget: Some("Obsidian.Obsidian"),
        },
        PackageMapping {
            brew_formula: None,
            brew_cask: Some("figma"),
            winget: Some("Figma.Figma"),
        },
        PackageMapping {
            brew_formula: None,
            brew_cask: Some("postman"),
            winget: Some("Postman.Postman"),
        },
    ]
}

/// Config-level mapping entry (user overrides).
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct MappingEntry {
    pub brew: Option<String>,
    pub cask: Option<String>,
    pub winget: Option<String>,
}

/// Resolved lookup tables built from defaults + config overrides.
pub struct MappingTable {
    brew_formula_to_winget: HashMap<String, String>,
    brew_cask_to_winget: HashMap<String, String>,
    winget_to_brew_formula: HashMap<String, String>,
    winget_to_brew_cask: HashMap<String, String>,
}

impl MappingTable {
    pub fn build(config_overrides: &[MappingEntry]) -> Self {
        let mut table = Self {
            brew_formula_to_winget: HashMap::new(),
            brew_cask_to_winget: HashMap::new(),
            winget_to_brew_formula: HashMap::new(),
            winget_to_brew_cask: HashMap::new(),
        };

        // Load built-in defaults
        for m in default_mappings() {
            table.insert_mapping(
                m.brew_formula.map(|s| s.to_string()),
                m.brew_cask.map(|s| s.to_string()),
                m.winget.map(|s| s.to_string()),
            );
        }

        // Apply config overrides (overwrite existing entries)
        for entry in config_overrides {
            table.insert_mapping(entry.brew.clone(), entry.cask.clone(), entry.winget.clone());
        }

        table
    }

    fn insert_mapping(
        &mut self,
        brew_formula: Option<String>,
        brew_cask: Option<String>,
        winget: Option<String>,
    ) {
        if let (Some(formula), Some(wg)) = (&brew_formula, &winget) {
            // Remove stale reverse entry if this formula previously mapped to a different winget ID
            if let Some(old_wg) = self.brew_formula_to_winget.get(formula) {
                if *old_wg != *wg {
                    self.winget_to_brew_formula.remove(&old_wg.to_lowercase());
                }
            }
            self.brew_formula_to_winget
                .insert(formula.clone(), wg.clone());
            self.winget_to_brew_formula
                .insert(wg.to_lowercase(), formula.clone());
        }
        if let (Some(cask), Some(wg)) = (&brew_cask, &winget) {
            if let Some(old_wg) = self.brew_cask_to_winget.get(cask) {
                if *old_wg != *wg {
                    self.winget_to_brew_cask.remove(&old_wg.to_lowercase());
                }
            }
            self.brew_cask_to_winget.insert(cask.clone(), wg.clone());
            self.winget_to_brew_cask
                .insert(wg.to_lowercase(), cask.clone());
        }
    }

    /// Map brew formula names to winget IDs.
    pub fn formulae_to_winget<'a>(&self, formulae: &'a [String]) -> Vec<(&'a str, &str)> {
        formulae
            .iter()
            .filter_map(|f| {
                self.brew_formula_to_winget
                    .get(f.as_str())
                    .map(|wg| (f.as_str(), wg.as_str()))
            })
            .collect()
    }

    /// Map brew cask names to winget IDs.
    pub fn casks_to_winget<'a>(&self, casks: &'a [String]) -> Vec<(&'a str, &str)> {
        casks
            .iter()
            .filter_map(|c| {
                self.brew_cask_to_winget
                    .get(c.as_str())
                    .map(|wg| (c.as_str(), wg.as_str()))
            })
            .collect()
    }

    /// Map winget IDs to brew formula names.
    pub fn winget_to_formulae<'a>(&self, ids: &'a [String]) -> Vec<(&'a str, &str)> {
        ids.iter()
            .filter_map(|id| {
                self.winget_to_brew_formula
                    .get(&id.to_lowercase())
                    .map(|f| (id.as_str(), f.as_str()))
            })
            .collect()
    }

    /// Map winget IDs to brew cask names.
    pub fn winget_to_casks<'a>(&self, ids: &'a [String]) -> Vec<(&'a str, &str)> {
        ids.iter()
            .filter_map(|id| {
                self.winget_to_brew_cask
                    .get(&id.to_lowercase())
                    .map(|c| (id.as_str(), c.as_str()))
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_builtin_formula_to_winget() {
        let table = MappingTable::build(&[]);
        let formulae = vec!["git".to_string(), "unknown-pkg".to_string()];
        let mapped = table.formulae_to_winget(&formulae);
        assert_eq!(mapped.len(), 1);
        assert_eq!(mapped[0], ("git", "Git.Git"));
    }

    #[test]
    fn test_builtin_cask_to_winget() {
        let table = MappingTable::build(&[]);
        let casks = vec!["visual-studio-code".to_string()];
        let mapped = table.casks_to_winget(&casks);
        assert_eq!(mapped.len(), 1);
        assert_eq!(
            mapped[0],
            ("visual-studio-code", "Microsoft.VisualStudioCode")
        );
    }

    #[test]
    fn test_winget_to_formula() {
        let table = MappingTable::build(&[]);
        let ids = vec!["Git.Git".to_string(), "Unknown.Pkg".to_string()];
        let mapped = table.winget_to_formulae(&ids);
        assert_eq!(mapped.len(), 1);
        assert_eq!(mapped[0], ("Git.Git", "git"));
    }

    #[test]
    fn test_winget_to_cask() {
        let table = MappingTable::build(&[]);
        let ids = vec!["Microsoft.VisualStudioCode".to_string()];
        let mapped = table.winget_to_casks(&ids);
        assert_eq!(mapped.len(), 1);
        assert_eq!(
            mapped[0],
            ("Microsoft.VisualStudioCode", "visual-studio-code")
        );
    }

    #[test]
    fn test_config_override() {
        let overrides = vec![MappingEntry {
            brew: Some("my-tool".to_string()),
            cask: None,
            winget: Some("MyOrg.MyTool".to_string()),
        }];
        let table = MappingTable::build(&overrides);
        let formulae = vec!["my-tool".to_string()];
        let mapped = table.formulae_to_winget(&formulae);
        assert_eq!(mapped.len(), 1);
        assert_eq!(mapped[0], ("my-tool", "MyOrg.MyTool"));
    }

    #[test]
    fn test_config_override_replaces_builtin() {
        let overrides = vec![MappingEntry {
            brew: Some("git".to_string()),
            cask: None,
            winget: Some("Custom.Git".to_string()),
        }];
        let table = MappingTable::build(&overrides);
        let formulae = vec!["git".to_string()];
        let mapped = table.formulae_to_winget(&formulae);
        assert_eq!(mapped[0], ("git", "Custom.Git"));
    }

    #[test]
    fn test_override_removes_stale_reverse_entry() {
        // Builtin: git → Git.Git. Override: git → Custom.Git.
        // The old reverse entry Git.Git → git should be removed.
        let overrides = vec![MappingEntry {
            brew: Some("git".to_string()),
            cask: None,
            winget: Some("Custom.Git".to_string()),
        }];
        let table = MappingTable::build(&overrides);
        let ids = vec!["Git.Git".to_string()];
        let mapped = table.winget_to_formulae(&ids);
        assert!(mapped.is_empty());
    }

    #[test]
    fn test_winget_lookup_case_insensitive() {
        let table = MappingTable::build(&[]);
        let ids = vec!["git.git".to_string()];
        let mapped = table.winget_to_formulae(&ids);
        assert_eq!(mapped.len(), 1);
        assert_eq!(mapped[0], ("git.git", "git"));
    }
}

use crate::config::{Config, ConflictStrategy};
use std::sync::LazyLock;

#[derive(Clone, Copy)]
pub enum FieldKind {
    Bool,
    Text,
}

pub struct ConfigField {
    pub key: &'static str,
    pub label: &'static str,
    pub section: &'static str,
    pub kind: FieldKind,
}

static FIELDS: LazyLock<Vec<ConfigField>> = LazyLock::new(|| {
    vec![
        // Features
        ConfigField {
            key: "personal_dotfiles",
            label: "Personal dotfiles",
            section: "Features",
            kind: FieldKind::Bool,
        },
        ConfigField {
            key: "personal_packages",
            label: "Personal packages",
            section: "Features",
            kind: FieldKind::Bool,
        },
        ConfigField {
            key: "team_dotfiles",
            label: "Team dotfiles",
            section: "Features",
            kind: FieldKind::Bool,
        },
        ConfigField {
            key: "collab_secrets",
            label: "Collab secrets",
            section: "Features",
            kind: FieldKind::Bool,
        },
        ConfigField {
            key: "team_layering",
            label: "Team layering",
            section: "Features",
            kind: FieldKind::Bool,
        },
        // Sync
        ConfigField {
            key: "interval",
            label: "Sync interval",
            section: "Sync",
            kind: FieldKind::Text,
        },
        ConfigField {
            key: "strategy",
            label: "Conflict strategy",
            section: "Sync",
            kind: FieldKind::Text,
        },
        // Security
        ConfigField {
            key: "encrypt_dotfiles",
            label: "Encrypt dotfiles",
            section: "Security",
            kind: FieldKind::Bool,
        },
        ConfigField {
            key: "scan_secrets",
            label: "Scan secrets",
            section: "Security",
            kind: FieldKind::Bool,
        },
        // Packages
        ConfigField {
            key: "remove_unlisted",
            label: "Remove unlisted",
            section: "Packages",
            kind: FieldKind::Bool,
        },
        ConfigField {
            key: "brew.enabled",
            label: "Brew enabled",
            section: "Packages",
            kind: FieldKind::Bool,
        },
        ConfigField {
            key: "brew.sync_casks",
            label: "Brew sync casks",
            section: "Packages",
            kind: FieldKind::Bool,
        },
        ConfigField {
            key: "brew.sync_taps",
            label: "Brew sync taps",
            section: "Packages",
            kind: FieldKind::Bool,
        },
        ConfigField {
            key: "npm.enabled",
            label: "npm enabled",
            section: "Packages",
            kind: FieldKind::Bool,
        },
        ConfigField {
            key: "npm.sync_versions",
            label: "npm sync versions",
            section: "Packages",
            kind: FieldKind::Bool,
        },
        ConfigField {
            key: "pnpm.enabled",
            label: "pnpm enabled",
            section: "Packages",
            kind: FieldKind::Bool,
        },
        ConfigField {
            key: "pnpm.sync_versions",
            label: "pnpm sync versions",
            section: "Packages",
            kind: FieldKind::Bool,
        },
        ConfigField {
            key: "bun.enabled",
            label: "Bun enabled",
            section: "Packages",
            kind: FieldKind::Bool,
        },
        ConfigField {
            key: "bun.sync_versions",
            label: "Bun sync versions",
            section: "Packages",
            kind: FieldKind::Bool,
        },
        ConfigField {
            key: "gem.enabled",
            label: "Gem enabled",
            section: "Packages",
            kind: FieldKind::Bool,
        },
        ConfigField {
            key: "gem.sync_versions",
            label: "Gem sync versions",
            section: "Packages",
            kind: FieldKind::Bool,
        },
        ConfigField {
            key: "uv.enabled",
            label: "uv enabled",
            section: "Packages",
            kind: FieldKind::Bool,
        },
        ConfigField {
            key: "uv.sync_versions",
            label: "uv sync versions",
            section: "Packages",
            kind: FieldKind::Bool,
        },
        // Project
        ConfigField {
            key: "project_configs.enabled",
            label: "Project configs",
            section: "Project",
            kind: FieldKind::Bool,
        },
    ]
});

pub fn fields() -> &'static [ConfigField] {
    &FIELDS
}

pub fn get_value(config: &Config, idx: usize) -> String {
    let f = &fields()[idx];
    match f.key {
        // Features
        "personal_dotfiles" => config.features.personal_dotfiles.to_string(),
        "personal_packages" => config.features.personal_packages.to_string(),
        "team_dotfiles" => config.features.team_dotfiles.to_string(),
        "collab_secrets" => config.features.collab_secrets.to_string(),
        "team_layering" => config.features.team_layering.to_string(),
        // Sync
        "interval" => config.sync.interval.clone(),
        "strategy" => match config.sync.strategy {
            ConflictStrategy::LastWriteWins => "last-write-wins".into(),
            ConflictStrategy::Manual => "manual".into(),
            ConflictStrategy::MachinePriority => "machine-priority".into(),
        },
        // Security
        "encrypt_dotfiles" => config.security.encrypt_dotfiles.to_string(),
        "scan_secrets" => config.security.scan_secrets.to_string(),
        // Packages
        "remove_unlisted" => config.packages.remove_unlisted.to_string(),
        "brew.enabled" => config.packages.brew.enabled.to_string(),
        "brew.sync_casks" => config.packages.brew.sync_casks.to_string(),
        "brew.sync_taps" => config.packages.brew.sync_taps.to_string(),
        "npm.enabled" => config.packages.npm.enabled.to_string(),
        "npm.sync_versions" => config.packages.npm.sync_versions.to_string(),
        "pnpm.enabled" => config.packages.pnpm.enabled.to_string(),
        "pnpm.sync_versions" => config.packages.pnpm.sync_versions.to_string(),
        "bun.enabled" => config.packages.bun.enabled.to_string(),
        "bun.sync_versions" => config.packages.bun.sync_versions.to_string(),
        "gem.enabled" => config.packages.gem.enabled.to_string(),
        "gem.sync_versions" => config.packages.gem.sync_versions.to_string(),
        "uv.enabled" => config.packages.uv.enabled.to_string(),
        "uv.sync_versions" => config.packages.uv.sync_versions.to_string(),
        // Project
        "project_configs.enabled" => config.project_configs.enabled.to_string(),
        _ => String::new(),
    }
}

/// Validate and set a text field. Returns false if validation fails or save errors.
pub fn set_value(config: &mut Config, idx: usize, val: &str) -> bool {
    let f = &fields()[idx];
    match f.key {
        "interval" => {
            if !is_valid_interval(val) {
                return false;
            }
            config.sync.interval = val.to_string();
        }
        "strategy" => {
            config.sync.strategy = match val {
                "last-write-wins" => ConflictStrategy::LastWriteWins,
                "manual" => ConflictStrategy::Manual,
                "machine-priority" => ConflictStrategy::MachinePriority,
                _ => return false,
            };
        }
        _ => return false,
    }
    config.save().is_ok()
}

/// Toggle a bool field. Returns false if save errors.
pub fn toggle(config: &mut Config, idx: usize) -> bool {
    let f = &fields()[idx];
    match f.key {
        "personal_dotfiles" => {
            config.features.personal_dotfiles = !config.features.personal_dotfiles
        }
        "personal_packages" => {
            config.features.personal_packages = !config.features.personal_packages
        }
        "team_dotfiles" => config.features.team_dotfiles = !config.features.team_dotfiles,
        "collab_secrets" => config.features.collab_secrets = !config.features.collab_secrets,
        "team_layering" => config.features.team_layering = !config.features.team_layering,
        "encrypt_dotfiles" => config.security.encrypt_dotfiles = !config.security.encrypt_dotfiles,
        "scan_secrets" => config.security.scan_secrets = !config.security.scan_secrets,
        "remove_unlisted" => config.packages.remove_unlisted = !config.packages.remove_unlisted,
        "brew.enabled" => config.packages.brew.enabled = !config.packages.brew.enabled,
        "brew.sync_casks" => config.packages.brew.sync_casks = !config.packages.brew.sync_casks,
        "brew.sync_taps" => config.packages.brew.sync_taps = !config.packages.brew.sync_taps,
        "npm.enabled" => config.packages.npm.enabled = !config.packages.npm.enabled,
        "npm.sync_versions" => {
            config.packages.npm.sync_versions = !config.packages.npm.sync_versions
        }
        "pnpm.enabled" => config.packages.pnpm.enabled = !config.packages.pnpm.enabled,
        "pnpm.sync_versions" => {
            config.packages.pnpm.sync_versions = !config.packages.pnpm.sync_versions
        }
        "bun.enabled" => config.packages.bun.enabled = !config.packages.bun.enabled,
        "bun.sync_versions" => {
            config.packages.bun.sync_versions = !config.packages.bun.sync_versions
        }
        "gem.enabled" => config.packages.gem.enabled = !config.packages.gem.enabled,
        "gem.sync_versions" => {
            config.packages.gem.sync_versions = !config.packages.gem.sync_versions
        }
        "uv.enabled" => config.packages.uv.enabled = !config.packages.uv.enabled,
        "uv.sync_versions" => config.packages.uv.sync_versions = !config.packages.uv.sync_versions,
        "project_configs.enabled" => {
            config.project_configs.enabled = !config.project_configs.enabled
        }
        _ => return true,
    }
    config.save().is_ok()
}

/// Validate interval format: number followed by s/m/h (e.g. "5m", "30s", "1h")
fn is_valid_interval(val: &str) -> bool {
    if val.len() < 2 {
        return false;
    }
    let (num, unit) = val.split_at(val.len() - 1);
    matches!(unit, "s" | "m" | "h") && num.parse::<u32>().is_ok()
}

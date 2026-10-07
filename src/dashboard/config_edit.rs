use crate::config::{is_safe_dotfile_path, Config, ConflictStrategy, DotfileEntry};
use std::sync::LazyLock;

#[derive(Clone, Copy, PartialEq)]
pub enum FieldKind {
    Bool,
    Text,
    List,
    DotfileList,
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
        // Dotfiles
        ConfigField {
            key: "dotfiles.files",
            label: "Dotfiles",
            section: "Dotfiles",
            kind: FieldKind::DotfileList,
        },
        ConfigField {
            key: "dotfiles.dirs",
            label: "Dotfile folders",
            section: "Dotfiles",
            kind: FieldKind::List,
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
            key: "pnpm.enabled",
            label: "pnpm enabled",
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
            key: "gem.enabled",
            label: "Gem enabled",
            section: "Packages",
            kind: FieldKind::Bool,
        },
        ConfigField {
            key: "uv.enabled",
            label: "uv enabled",
            section: "Packages",
            kind: FieldKind::Bool,
        },
        // Package security
        ConfigField {
            key: "min_release_age_days",
            label: "Min release age (days)",
            section: "Package security",
            kind: FieldKind::Text,
        },
        ConfigField {
            key: "auto_install_from_trusted",
            label: "Auto-install from trusted machines",
            section: "Package security",
            kind: FieldKind::Bool,
        },
        ConfigField {
            key: "brew.trusted_taps",
            label: "Trusted Homebrew taps",
            section: "Package security",
            kind: FieldKind::List,
        },
        ConfigField {
            key: "allow_scripts",
            label: "Allow install scripts",
            section: "Package security",
            kind: FieldKind::List,
        },
        // Project
        ConfigField {
            key: "project_configs.enabled",
            label: "Project configs",
            section: "Project",
            kind: FieldKind::Bool,
        },
        ConfigField {
            key: "project_configs.search_paths",
            label: "Search paths",
            section: "Project",
            kind: FieldKind::List,
        },
        ConfigField {
            key: "project_configs.patterns",
            label: "File patterns",
            section: "Project",
            kind: FieldKind::List,
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
        // Dotfiles
        "dotfiles.files" => format!("{} items", config.dotfiles.files.len()),
        "dotfiles.dirs" => format!("{} items", config.dotfiles.dirs.len()),
        // Packages
        "remove_unlisted" => config.packages.remove_unlisted.to_string(),
        "brew.enabled" => config.packages.brew.enabled.to_string(),
        "brew.sync_casks" => config.packages.brew.sync_casks.to_string(),
        "brew.sync_taps" => config.packages.brew.sync_taps.to_string(),
        "npm.enabled" => config.packages.npm.enabled.to_string(),
        "pnpm.enabled" => config.packages.pnpm.enabled.to_string(),
        "bun.enabled" => config.packages.bun.enabled.to_string(),
        "gem.enabled" => config.packages.gem.enabled.to_string(),
        "uv.enabled" => config.packages.uv.enabled.to_string(),
        // Package security
        "min_release_age_days" => config.packages.min_release_age_days.to_string(),
        "auto_install_from_trusted" => config.packages.auto_install_from_trusted.to_string(),
        "brew.trusted_taps" => format!("{} items", config.packages.brew.trusted_taps.len()),
        "allow_scripts" => format!("{} items", config.packages.allow_scripts.len()),
        // Project
        "project_configs.enabled" => config.project_configs.enabled.to_string(),
        "project_configs.search_paths" => {
            format!("{} items", config.project_configs.search_paths.len())
        }
        "project_configs.patterns" => format!("{} items", config.project_configs.patterns.len()),
        _ => String::new(),
    }
}

/// Why an edit was refused or did not save, as the dashboard shows it.
pub type EditResult = Result<(), String>;

fn save(config: &Config) -> EditResult {
    config
        .save()
        .map_err(|e| format!("Config not saved: {}", e))
}

/// Validate and set a text field.
pub fn set_value(config: &mut Config, idx: usize, val: &str) -> EditResult {
    let f = &fields()[idx];
    match f.key {
        "interval" => {
            if !is_valid_interval(val) {
                return Err(format!(
                    "{} needs a number and s, m or h, such as 5m",
                    f.label
                ));
            }
            config.sync.interval = val.to_string();
        }
        "strategy" => {
            config.sync.strategy = match val {
                "last-write-wins" => ConflictStrategy::LastWriteWins,
                "manual" => ConflictStrategy::Manual,
                "machine-priority" => ConflictStrategy::MachinePriority,
                _ => {
                    return Err(format!(
                        "{} is last-write-wins, manual or machine-priority",
                        f.label
                    ))
                }
            };
        }
        "min_release_age_days" => match val.trim().parse() {
            Ok(days) => config.packages.min_release_age_days = days,
            Err(_) => return Err(format!("{} needs a whole number", f.label)),
        },
        _ => return Err(format!("{} is not a text field", f.label)),
    }
    save(config)
}

/// Toggle a bool field.
pub fn toggle(config: &mut Config, idx: usize) -> EditResult {
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
        "pnpm.enabled" => config.packages.pnpm.enabled = !config.packages.pnpm.enabled,
        "bun.enabled" => config.packages.bun.enabled = !config.packages.bun.enabled,
        "gem.enabled" => config.packages.gem.enabled = !config.packages.gem.enabled,
        "uv.enabled" => config.packages.uv.enabled = !config.packages.uv.enabled,
        "auto_install_from_trusted" => {
            config.packages.auto_install_from_trusted = !config.packages.auto_install_from_trusted
        }
        "project_configs.enabled" => {
            config.project_configs.enabled = !config.project_configs.enabled
        }
        _ => return Err(format!("{} is not an on/off field", f.label)),
    }
    save(config)
}

/// Get items for a List field
pub fn get_list_items(config: &Config, key: &str) -> Vec<String> {
    match key {
        "dotfiles.dirs" => config.dotfiles.dirs.clone(),
        "brew.trusted_taps" => config.packages.brew.trusted_taps.clone(),
        "allow_scripts" => config.packages.allow_scripts.clone(),
        "project_configs.search_paths" => config.project_configs.search_paths.clone(),
        "project_configs.patterns" => config.project_configs.patterns.clone(),
        _ => Vec::new(),
    }
}

/// Get dotfile items as (path, create_if_missing) pairs
pub fn get_dotfile_items(config: &Config) -> Vec<(String, bool)> {
    config
        .dotfiles
        .files
        .iter()
        .map(|e| (e.path().to_string(), e.create_if_missing()))
        .collect()
}

fn list_mut<'a>(config: &'a mut Config, key: &str) -> Option<&'a mut Vec<String>> {
    Some(match key {
        "dotfiles.dirs" => &mut config.dotfiles.dirs,
        "brew.trusted_taps" => &mut config.packages.brew.trusted_taps,
        "allow_scripts" => &mut config.packages.allow_scripts,
        "project_configs.search_paths" => &mut config.project_configs.search_paths,
        "project_configs.patterns" => &mut config.project_configs.patterns,
        _ => return None,
    })
}

/// Add an item to a List field.
pub fn add_list_item(config: &mut Config, key: &str, value: &str) -> EditResult {
    let value = value.trim();
    if value.is_empty() {
        return Err("Type a value first".into());
    }
    let Some(list) = list_mut(config, key) else {
        return Err(format!("{} is not a list", key));
    };
    if list.iter().any(|v| v == value) {
        return Err(format!("{} is in the list already", value));
    }
    list.push(value.to_string());
    save(config)
}

/// Remove an item from a List field by index.
pub fn remove_list_item(config: &mut Config, key: &str, value: &str) -> EditResult {
    take_list_item(config, key, value)?;
    save(config)
}

/// Remove `value` from a List field. Another edit can change the list after the user chose
/// the value, so the value counts, not its position.
fn take_list_item(config: &mut Config, key: &str, value: &str) -> EditResult {
    let Some(list) = list_mut(config, key) else {
        return Err(format!("{} is not a list", key));
    };
    let Some(pos) = list.iter().position(|v| v == value) else {
        return Err(format!("{} is no longer in the list", value));
    };
    list.remove(pos);
    Ok(())
}

/// Add a dotfile entry.
pub fn add_dotfile(config: &mut Config, path: &str, create_if_missing: bool) -> EditResult {
    let path = path.trim();
    if path.is_empty() {
        return Err("Type a path first".into());
    }
    if !is_safe_dotfile_path(path) {
        return Err(format!(
            "{} must be a path in your home folder, without ..",
            path
        ));
    }
    if config.dotfiles.files.iter().any(|e| e.path() == path) {
        return Err(format!("{} is in the list already", path));
    }
    config.dotfiles.files.push(DotfileEntry::WithOptions {
        path: path.to_string(),
        create_if_missing,
        on_conflict: Default::default(),
    });
    save(config)
}

/// Remove a dotfile by index.
pub fn remove_dotfile(config: &mut Config, path: &str) -> EditResult {
    take_dotfile(config, path)?;
    save(config)
}

/// Remove the dotfile entry for `path`, by path like [`take_list_item`].
fn take_dotfile(config: &mut Config, path: &str) -> EditResult {
    let Some(pos) = config.dotfiles.files.iter().position(|e| e.path() == path) else {
        return Err(format!("{} is no longer in the list", path));
    };
    config.dotfiles.files.remove(pos);
    Ok(())
}

/// Toggle create_if_missing for a dotfile entry.
pub fn toggle_dotfile_create(config: &mut Config, index: usize) -> EditResult {
    if index >= config.dotfiles.files.len() {
        return Err("No dotfile selected".into());
    }
    let entry = &config.dotfiles.files[index];
    let path = entry.path().to_string();
    let new_create = !entry.create_if_missing();
    let on_conflict = entry.on_conflict();
    config.dotfiles.files[index] = DotfileEntry::WithOptions {
        path,
        create_if_missing: new_create,
        on_conflict,
    };
    save(config)
}

/// The profile this machine uses, for an edit to its dotfiles.
fn machine_profile<'a>(
    config: &'a mut Config,
    machine_id: &str,
) -> Result<&'a mut crate::config::ProfileConfig, String> {
    let name = config.profile_name(machine_id).to_string();
    config
        .profiles
        .get_mut(&name)
        .ok_or_else(|| format!("Profile {} is not in the config", name))
}

/// Toggle shared flag for a profile dotfile by path.
pub fn toggle_profile_dotfile_shared(
    config: &mut Config,
    machine_id: &str,
    path: &str,
) -> EditResult {
    use crate::config::ProfileDotfileEntry;

    let profile = machine_profile(config, machine_id)?;
    let Some(entry) = profile.dotfiles.iter_mut().find(|e| e.path() == path) else {
        return Err(format!("{} is not in this profile", path));
    };
    let new_shared = !entry.shared();
    let entry_path = entry.path().to_string();
    *entry = ProfileDotfileEntry::WithOptions {
        path: entry_path,
        shared: new_shared,
        create_if_missing: entry.create_if_missing(),
        on_conflict: entry.on_conflict(),
    };
    save(config)
}

/// Add a dotfile to the machine's profile.
pub fn add_profile_dotfile(config: &mut Config, machine_id: &str, path: &str) -> EditResult {
    use crate::config::ProfileDotfileEntry;

    let profile = machine_profile(config, machine_id)?;
    if profile.dotfiles.iter().any(|e| e.path() == path) {
        return Err(format!("{} is in this profile already", path));
    }
    profile
        .dotfiles
        .push(ProfileDotfileEntry::Simple(path.to_string()));
    save(config)
}

/// Remove a dotfile from the machine's profile by path.
pub fn remove_profile_dotfile(config: &mut Config, machine_id: &str, path: &str) -> EditResult {
    let profile = machine_profile(config, machine_id)?;
    let before = profile.dotfiles.len();
    profile.dotfiles.retain(|e| e.path() != path);
    if profile.dotfiles.len() == before {
        return Err(format!("{} is not in this profile", path));
    }
    save(config)
}

/// Validate interval format: number followed by s/m/h (e.g. "5m", "30s", "1h")
fn is_valid_interval(val: &str) -> bool {
    if val.len() < 2 {
        return false;
    }
    let (num, unit) = val.split_at(val.len() - 1);
    matches!(unit, "s" | "m" | "h") && num.parse::<u32>().is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn index(key: &str) -> usize {
        fields().iter().position(|f| f.key == key).unwrap()
    }

    #[test]
    fn list_items_are_removed_by_value_not_position() {
        let mut config = Config::default();
        config.packages.allow_scripts = vec!["a".into(), "b".into()];
        // Another edit put a new item first, after the user chose `b`
        config.packages.allow_scripts.insert(0, "new".into());
        assert!(take_list_item(&mut config, "allow_scripts", "b").is_ok());
        assert_eq!(config.packages.allow_scripts, ["new", "a"]);
        assert_eq!(
            take_list_item(&mut config, "allow_scripts", "b"),
            Err("b is no longer in the list".to_string())
        );
        assert_eq!(config.packages.allow_scripts, ["new", "a"]);

        let before = config.dotfiles.files.len();
        let path = config.dotfiles.files[0].path().to_string();
        assert!(take_dotfile(&mut config, &path).is_ok());
        assert_eq!(config.dotfiles.files.len(), before - 1);
        assert!(take_dotfile(&mut config, &path).is_err());
    }

    #[test]
    fn invalid_values_are_refused_with_the_reason() {
        let mut config = Config::default();
        let before = config.sync.interval.clone();
        assert_eq!(
            set_value(&mut config, index("interval"), "often"),
            Err("Sync interval needs a number and s, m or h, such as 5m".to_string())
        );
        assert_eq!(config.sync.interval, before);
        assert!(set_value(&mut config, index("strategy"), "newest")
            .unwrap_err()
            .contains("last-write-wins"));
        assert!(set_value(&mut config, index("min_release_age_days"), "-1")
            .unwrap_err()
            .contains("whole number"));
        assert!(add_dotfile(&mut config, "../etc/passwd", true)
            .unwrap_err()
            .contains("without .."));
        config.packages.allow_scripts.push("esbuild".into());
        assert_eq!(
            add_list_item(&mut config, "allow_scripts", "esbuild"),
            Err("esbuild is in the list already".to_string())
        );
    }
}

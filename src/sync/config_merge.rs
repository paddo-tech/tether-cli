//! Three-way merge of the synced config.toml. Each machine edits its own copy and exports it
//! as one file, so a plain replace loses the edits of whichever machine exported first.
//! The merge works on parsed values, so a file that only changed its format merges as
//! unchanged.

use crate::config::{Config, CURRENT_CONFIG_VERSION};
use anyhow::Result;
use toml::Value;

pub struct Merged {
    pub config: Config,
    /// The merged config differs from the local one
    pub changed: bool,
    /// Settings both sides changed to different values; the local value stays
    pub conflicts: Vec<String>,
}

/// The parsed form two configs compare by: defaults filled in, maps sorted, format gone.
pub fn normalize(text: &str) -> Result<Value> {
    let mut config = Config::parse(text)?;
    config.config_version = CURRENT_CONFIG_VERSION;
    Ok(Value::try_from(&config)?)
}

/// Whether two config.toml texts hold the same settings. Text that does not parse
/// compares by bytes.
pub fn same_settings(a: &[u8], b: &[u8]) -> bool {
    let parse = |t: &[u8]| std::str::from_utf8(t).ok().and_then(|t| normalize(t).ok());
    match (parse(a), parse(b)) {
        (Some(a), Some(b)) => a == b,
        _ => a == b,
    }
}

/// Merges `local` and `remote` against `base`, the remote config this machine last merged.
/// Without a base, every difference counts as a change on both sides.
pub fn merge(base: Option<&str>, local: &str, remote: &str, machine_id: &str) -> Result<Merged> {
    let local_value = normalize(local)?;
    let remote_value = normalize(remote)?;
    let base_value = base.and_then(|b| normalize(b).ok());
    let base = match &base_value {
        Some(b) => Base::Known(Some(b)),
        None => Base::Unknown,
    };
    let mut ctx = Ctx {
        machine_id,
        conflicts: Vec::new(),
    };
    let merged = merge_node(&mut ctx, &[], base, Some(&local_value), Some(&remote_value))
        .unwrap_or_else(|| local_value.clone());
    let changed = merged != local_value;
    Ok(Merged {
        config: merged.try_into()?,
        changed,
        conflicts: ctx.conflicts,
    })
}

#[derive(Clone, Copy)]
enum Base<'a> {
    /// The base value; None when the base lacks the key
    Known(Option<&'a Value>),
    Unknown,
}

struct Ctx<'a> {
    machine_id: &'a str,
    conflicts: Vec<String>,
}

fn merge_node(
    ctx: &mut Ctx,
    path: &[&str],
    base: Base,
    local: Option<&Value>,
    remote: Option<&Value>,
) -> Option<Value> {
    if local == remote {
        return local.cloned();
    }
    let profile_entry = path.len() == 2 && path[0] == "machine_profiles";
    // Only this machine assigns its own profile; another machine's export never drops it
    if profile_entry && path[1] == ctx.machine_id && local.is_some() {
        return local.cloned();
    }
    if let Base::Known(base) = base {
        if local == base {
            return remote.cloned();
        }
        if remote == base {
            return local.cloned();
        }
    }
    match (local, remote) {
        (Some(Value::Table(l)), Some(Value::Table(r))) => {
            let base_table = match base {
                Base::Known(Some(Value::Table(b))) => Some(b),
                _ => None,
            };
            let mut keys: Vec<&String> = l.keys().chain(r.keys()).collect();
            keys.sort();
            keys.dedup();
            let mut out = toml::Table::new();
            for key in keys {
                let child_base = match base {
                    Base::Unknown => Base::Unknown,
                    Base::Known(_) => Base::Known(base_table.and_then(|b| b.get(key))),
                };
                let mut child_path = path.to_vec();
                child_path.push(key);
                if let Some(v) = merge_node(ctx, &child_path, child_base, l.get(key), r.get(key)) {
                    out.insert(key.clone(), v);
                }
            }
            Some(Value::Table(out))
        }
        // Argument order matters, so the merge tool's arguments stay one value
        (Some(Value::Array(l)), Some(Value::Array(r))) if path != ["merge", "args"] => {
            let base_items = match base {
                Base::Known(Some(Value::Array(b))) => Some(b.as_slice()),
                Base::Known(_) => Some(&[][..]),
                Base::Unknown => None,
            };
            Some(Value::Array(merge_list(base_items, l, r)))
        }
        // Without a base, another machine's assignment is that machine's to make
        (_, Some(r)) if profile_entry && matches!(base, Base::Unknown) => Some(r.clone()),
        // Added on one side only
        (Some(v), None) | (None, Some(v)) if matches!(base, Base::Unknown) => Some(v.clone()),
        _ => {
            ctx.conflicts.push(path.join("."));
            local.cloned()
        }
    }
}

/// Lists merge as sets: an item either side added stays, and an item one side removed goes
/// unless the other side added it again. Without a base, nothing counts as removed. Two
/// items for one path, such as a dotfile with changed options, keep the local one.
fn merge_list(base: Option<&[Value]>, local: &[Value], remote: &[Value]) -> Vec<Value> {
    let in_base = |v: &Value| base.is_some_and(|b| b.contains(v));
    let mut out: Vec<Value> = local
        .iter()
        .filter(|v| !(in_base(v) && !remote.contains(v)))
        .cloned()
        .collect();
    out.extend(
        remote
            .iter()
            .filter(|v| !local.contains(v) && !in_base(v))
            .cloned(),
    );
    let mut seen = Vec::new();
    out.retain(|v| {
        let key = item_key(v);
        if seen.contains(&key) {
            return false;
        }
        seen.push(key);
        true
    });
    out
}

fn item_key(v: &Value) -> Value {
    match v {
        Value::Table(t) => t.get("path").cloned().unwrap_or_else(|| v.clone()),
        _ => v.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base_config() -> String {
        let mut c = Config::default();
        c.machine_profiles.insert("a".into(), "dev".into());
        c.machine_profiles.insert("b".into(), "dev".into());
        toml::to_string_pretty(&c).unwrap()
    }

    fn edit(text: &str, f: impl FnOnce(&mut Config)) -> String {
        let mut c = Config::parse(text).unwrap();
        f(&mut c);
        toml::to_string_pretty(&c).unwrap()
    }

    #[test]
    fn format_only_rewrite_takes_remote_changes() {
        let base = base_config();
        // Same settings, other format: a comment, other spacing, another key order
        let local = format!("# rewritten\n{}", base.replace(" = ", "="));
        assert!(same_settings(base.as_bytes(), local.as_bytes()));
        let remote = edit(&base, |c| {
            c.machine_profiles.insert("b".into(), "linux-server".into());
            c.profiles.insert(
                "linux-server".into(),
                crate::config::ProfileConfig::default(),
            );
        });
        let m = merge(Some(&base), &local, &remote, "a").unwrap();
        assert!(m.changed);
        assert!(m.conflicts.is_empty());
        assert_eq!(m.config.machine_profiles["b"], "linux-server");
        assert!(m.config.profiles.contains_key("linux-server"));
    }

    #[test]
    fn edits_to_different_settings_both_stay() {
        let base = base_config();
        let local = edit(&base, |c| c.dashboard.theme = Some("mocha".into()));
        let remote = edit(&base, |c| c.packages.brew.sync_casks = false);
        let m = merge(Some(&base), &local, &remote, "a").unwrap();
        assert_eq!(m.config.dashboard.theme.as_deref(), Some("mocha"));
        assert!(!m.config.packages.brew.sync_casks);
        assert!(m.conflicts.is_empty());
    }

    #[test]
    fn both_changed_keeps_local_and_reports() {
        let base = base_config();
        let local = edit(&base, |c| c.packages.min_release_age_days = 3);
        let remote = edit(&base, |c| c.packages.min_release_age_days = 14);
        let m = merge(Some(&base), &local, &remote, "a").unwrap();
        assert_eq!(m.config.packages.min_release_age_days, 3);
        assert_eq!(m.conflicts, vec!["packages.min_release_age_days"]);
    }

    #[test]
    fn own_assignment_stays_and_others_take_remote() {
        let base = base_config();
        let local = edit(&base, |c| {
            c.machine_profiles.insert("a".into(), "server".into());
        });
        // The remote drops a's entry and reassigns b
        let remote = edit(&base, |c| {
            c.machine_profiles.remove("a");
            c.machine_profiles.insert("b".into(), "server".into());
        });
        let m = merge(Some(&base), &local, &remote, "a").unwrap();
        assert_eq!(m.config.machine_profiles["a"], "server");
        assert_eq!(m.config.machine_profiles["b"], "server");
        // Without a base too
        let m = merge(None, &local, &remote, "a").unwrap();
        assert_eq!(m.config.machine_profiles["a"], "server");
        assert_eq!(m.config.machine_profiles["b"], "server");
    }

    #[test]
    fn profiles_merge_per_field() {
        let mut c = Config::parse(&base_config()).unwrap();
        c.profiles.insert(
            "dev".into(),
            crate::config::ProfileConfig {
                dirs: vec![".config/a".into()],
                packages: vec!["brew".into()],
                ..Default::default()
            },
        );
        let base = toml::to_string_pretty(&c).unwrap();
        let local = edit(&base, |c| {
            let p = c.profiles.get_mut("dev").unwrap();
            p.dirs.push(".config/local".into());
            p.packages.push("npm".into());
        });
        let remote = edit(&base, |c| {
            let p = c.profiles.get_mut("dev").unwrap();
            p.dirs.retain(|d| d != ".config/a");
            p.dirs.push(".config/remote".into());
        });
        let m = merge(Some(&base), &local, &remote, "a").unwrap();
        let dev = &m.config.profiles["dev"];
        assert_eq!(dev.dirs, vec![".config/local", ".config/remote"]);
        assert_eq!(dev.packages, vec!["brew", "npm"]);
    }

    #[test]
    fn dotfile_lists_union_and_remove_once() {
        let base = edit(&base_config(), |c| {
            c.dotfiles.files = vec![
                crate::config::DotfileEntry::Simple(".zshrc".into()),
                crate::config::DotfileEntry::Simple(".vimrc".into()),
            ];
        });
        let local = edit(&base, |c| {
            c.dotfiles
                .files
                .push(crate::config::DotfileEntry::Simple(".tmux.conf".into()));
        });
        let remote = edit(&base, |c| {
            c.dotfiles.files.retain(|f| f.path() != ".vimrc");
            c.dotfiles
                .files
                .push(crate::config::DotfileEntry::Simple(".gitconfig".into()));
        });
        let m = merge(Some(&base), &local, &remote, "a").unwrap();
        let paths: Vec<&str> = m.config.dotfiles.files.iter().map(|f| f.path()).collect();
        assert_eq!(paths, vec![".zshrc", ".tmux.conf", ".gitconfig"]);
    }

    #[test]
    fn unknown_base_keeps_additions_from_both() {
        let base = base_config();
        let local = edit(&base, |c| c.dashboard.theme = Some("mocha".into()));
        let remote = edit(&base, |c| {
            c.profiles.insert(
                "linux-server".into(),
                crate::config::ProfileConfig::default(),
            );
            c.machine_profiles.insert("b".into(), "linux-server".into());
        });
        let m = merge(None, &local, &remote, "a").unwrap();
        assert_eq!(m.config.dashboard.theme.as_deref(), Some("mocha"));
        assert_eq!(m.config.machine_profiles["b"], "linux-server");
        assert!(m.config.profiles.contains_key("linux-server"));
    }

    #[test]
    fn merged_config_keeps_1x_fields() {
        let base = base_config();
        let local = edit(&base, |c| c.dashboard.theme = Some("mocha".into()));
        let m = merge(Some(&base), &local, &base, "a").unwrap();
        let text = toml::to_string_pretty(&m.config).unwrap();
        assert!(text.contains("sync_versions = false"), "{text}");
    }
}

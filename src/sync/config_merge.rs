//! Three-way merge of the synced config.toml. Each machine edits its own copy and exports it
//! as one file, so a plain replace loses the edits of whichever machine exported first.
//!
//! The merge works on raw TOML values, never through `Config`, so keys from a newer Tether
//! stay. The rules:
//! - A missing key holds its default before values compare, so a file that only changed its
//!   format, or wrote a default out, has no changes.
//! - A setting that one side changed takes the value of that side. When both sides changed it
//!   to different values, the local value stays and the merge reports the setting. The
//!   export then makes the local value the repo value, so the fleet settles on the value of
//!   the machine that exports last.
//! - A synced config without `config_writer` came from 1.x, which drops keys it does not
//!   know when it saves. A key that it does not have is not a deletion.
//! - The `machine_profiles` entry of this machine keeps its local value. A profile that an
//!   assignment names is never deleted.
//! - The lists in SET_LISTS merge item by item and are written sorted. Every other list
//!   merges as one value.
//! - The merged settings go into the local file as edits, so its comments and layout stay.

use crate::config::{Config, CONFIG_WRITER, CURRENT_CONFIG_VERSION};
use anyhow::{Context, Result};
use toml::{Table, Value};
use toml_edit::{DocumentMut, Item, TableLike};

/// Lists whose order has no meaning. They merge as sets, keyed by `path` for dotfile entries,
/// and are written sorted, so two machines never write them in a different order. `*`
/// matches any key of a map. Every other list merges as one value: merge.args (argument
/// order), teams.active (the first team is the active team), project_configs.search_paths and
/// project_configs.patterns (search order), teams.collabs.*.members_cache (a refresh replaces
/// it), and lists that this build does not know.
const SET_LISTS: &[&[&str]] = &[
    &["dotfiles", "files"],
    &["dotfiles", "dirs"],
    &["packages", "allow_scripts"],
    &["packages", "brew", "trusted_taps"],
    &["profiles", "*", "dotfiles"],
    &["profiles", "*", "dirs"],
    &["profiles", "*", "packages"],
    &["team", "orgs"],
    &["teams", "allowed_orgs"],
    &["teams", "teams", "*", "orgs"],
    &["teams", "collabs", "*", "projects"],
];

/// Keys that each machine sets for its own file. They never merge.
const OWN_KEYS: [&str; 3] = ["config_version", "config_writer", "config_parent"];

pub struct Merged {
    /// The local config.toml with the merged settings written in
    pub text: String,
    /// The merged settings differ from the local ones
    pub changed: bool,
    /// Settings both sides changed to different values; the local value stays
    pub conflicts: Vec<String>,
    /// Profiles the remote deleted that an assignment still names; they stay
    pub kept_profiles: Vec<String>,
}

/// The `config_version` of a config that this build cannot read, if it is newer.
pub fn newer_version(text: &str) -> Option<i64> {
    let raw: Table = toml::from_str(text).ok()?;
    let version = raw.get("config_version")?.as_integer()?;
    (version > CURRENT_CONFIG_VERSION as i64).then_some(version)
}

/// The text an export pushes: the local config with `config_parent`, the SHA-256 of the synced
/// copy it replaces. So each export differs from every earlier synced copy, and a copy that a
/// 1.x machine exports again verbatim is known as stale. Text that does not parse stays.
pub fn with_parent(text: &str, parent: &str) -> String {
    let Ok(mut doc) = text.parse::<DocumentMut>() else {
        return text.to_string();
    };
    doc.as_table_mut()
        .insert("config_parent", toml_edit::value(parent));
    doc.to_string()
}

/// Whether the text reads as a config.toml.
pub fn reads(text: &[u8]) -> bool {
    std::str::from_utf8(text).is_ok_and(|t| settings(t, true).is_ok())
}

/// Whether two config.toml texts hold the same settings. Text that does not parse
/// compares by bytes.
pub fn same_settings(a: &[u8], b: &[u8]) -> bool {
    let parse = |t: &[u8]| {
        std::str::from_utf8(t)
            .ok()
            .and_then(|t| settings(t, true).ok())
    };
    match (parse(a), parse(b)) {
        (Some(a), Some(b)) => a == b,
        _ => a == b,
    }
}

/// The text `Config::save` writes: the settings that changed, written into the current file.
pub fn save_text(current: Option<&str>, config: &Config) -> Result<String> {
    let pretty = || Ok(toml::to_string_pretty(config)?);
    let Some(current) = current else {
        return pretty();
    };
    let (Ok(mut doc), Ok(old)) = (
        current.parse::<DocumentMut>(),
        toml::from_str::<Config>(current),
    ) else {
        return pretty();
    };
    apply(doc.as_table_mut(), &table_of(&old)?, &table_of(config)?);
    Ok(doc.to_string())
}

/// Merges `local` and `remote` against `base`, the synced config this machine last merged or
/// exported. Without a base, every difference counts as a change on both sides.
pub fn merge(base: Option<&str>, local: &str, remote: &str, machine_id: &str) -> Result<Merged> {
    let local_value = settings(local, true)?;
    let from_2x = has_writer(remote)?;
    let mut remote_value = settings(remote, from_2x)?;
    let base_value = base.map(|b| settings(b, true)).transpose()?;
    if !from_2x {
        fill_missing(
            &mut remote_value,
            base_value.as_ref().unwrap_or(&local_value),
        );
    }

    let (local_value, remote_value) = (Value::Table(local_value), Value::Table(remote_value));
    let base_value = base_value.map(Value::Table);
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
    let (Value::Table(mut merged), Value::Table(local_table)) = (merged, &local_value) else {
        unreachable!("a config is a table");
    };
    let base_table = base_value.as_ref().and_then(|b| b.as_table());
    let kept_profiles = keep_assigned_profiles(&mut merged, local_table, base_table);

    let changed = merged != *local_table;
    let text = if changed {
        let mut doc: DocumentMut = local.parse()?;
        apply(doc.as_table_mut(), local_table, &merged);
        doc.as_table_mut()
            .insert("config_writer", toml_edit::value(i64::from(CONFIG_WRITER)));
        doc.to_string()
    } else {
        local.to_string()
    };
    Config::parse(&text).context("The merged config.toml does not read")?;
    Ok(Merged {
        text,
        changed,
        conflicts: ctx.conflicts,
        kept_profiles,
    })
}

fn has_writer(text: &str) -> Result<bool> {
    let raw: Table = toml::from_str(text)?;
    Ok(raw.contains_key("config_writer"))
}

fn table_of(config: &Config) -> Result<Table> {
    match Value::try_from(config)? {
        Value::Table(t) => Ok(t),
        _ => unreachable!("a config is a table"),
    }
}

/// The settings of a config in the form two configs compare by: the raw keys, with the
/// values `Config` reads for the keys it knows, and set lists sorted. `filled`: a missing
/// key holds its default. Otherwise a missing key stays missing.
fn settings(text: &str, filled: bool) -> Result<Table> {
    let raw: Table = toml::from_str(text)?;
    let config: Config = toml::from_str(text)?;
    let mut known = table_of(&config)?;
    if filled {
        fill_empty(&mut known);
    }
    let mut out = overlay(known, &raw, filled);
    for key in OWN_KEYS {
        out.remove(key);
    }
    sort_set_lists(&mut out, &mut Vec::new());
    Ok(out)
}

/// `Config` leaves these out when they are empty, so a missing one is empty.
fn fill_empty(t: &mut Table) {
    let empty_list = || Value::Array(Vec::new());
    let empty_table = || Value::Table(Table::new());
    t.entry("team_only").or_insert(Value::Boolean(false));
    t.entry("machine_profiles").or_insert_with(empty_table);
    t.entry("profiles").or_insert_with(empty_table);
    t.entry("dashboard").or_insert_with(empty_table);
    if let Some(Value::Table(packages)) = t.get_mut("packages") {
        packages.entry("allow_scripts").or_insert_with(empty_list);
        if let Some(Value::Table(brew)) = packages.get_mut("brew") {
            brew.entry("trusted_taps").or_insert_with(empty_list);
        }
    }
    if let Some(Value::Table(profiles)) = t.get_mut("profiles") {
        for (_, profile) in profiles.iter_mut() {
            if let Value::Table(p) = profile {
                p.entry("dirs").or_insert_with(empty_list);
                p.entry("packages").or_insert_with(empty_list);
            }
        }
    }
}

/// Adds the raw keys that `Config` does not know to `known`. Unless `filled`, drops the keys
/// the raw text does not have.
fn overlay(mut known: Table, raw: &Table, filled: bool) -> Table {
    if !filled {
        known.retain(|k, _| raw.contains_key(k));
    }
    for (key, r) in raw {
        match (known.get_mut(key), r) {
            (Some(Value::Table(k)), Value::Table(r)) => {
                *k = overlay(std::mem::take(k), r, filled);
            }
            (Some(_), _) => {}
            (None, _) => {
                known.insert(key.clone(), r.clone());
            }
        }
    }
    known
}

/// A config from 1.x lacks the keys 1.x does not know. They keep the reference value.
fn fill_missing(remote: &mut Table, reference: &Table) {
    for (key, r) in reference {
        match (remote.get_mut(key), r) {
            (None, _) => {
                remote.insert(key.clone(), r.clone());
            }
            (Some(Value::Table(t)), Value::Table(r)) => fill_missing(t, r),
            _ => {}
        }
    }
}

fn is_set_list(path: &[&str]) -> bool {
    SET_LISTS.iter().any(|pattern| {
        pattern.len() == path.len() && pattern.iter().zip(path).all(|(p, k)| *p == "*" || p == k)
    })
}

fn sort_set_lists<'a>(t: &'a mut Table, path: &mut Vec<&'a str>) {
    for (key, value) in t.iter_mut() {
        path.push(key);
        match value {
            Value::Table(child) => sort_set_lists(child, path),
            Value::Array(items) if is_set_list(path) => {
                let mut seen = Vec::new();
                items.retain(|v| {
                    let k = item_key(v);
                    let new = !seen.contains(&k);
                    seen.push(k);
                    new
                });
                items.sort_by_key(item_key);
            }
            _ => {}
        }
        path.pop();
    }
}

/// Items of a set list are the same item when they name the same path.
fn item_key(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Table(t) => match t.get("path") {
            Some(Value::String(p)) => p.clone(),
            _ => v.to_string(),
        },
        _ => v.to_string(),
    }
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
    let profile_entry = path.len() == 2 && path[0] == "machine_profiles";
    // Only this machine assigns its own profile
    if profile_entry && path[1] == ctx.machine_id {
        return local.cloned();
    }
    if local == remote {
        return local.cloned();
    }
    // The rules for profiles apply per entry, so these tables never merge as one value
    let per_key = matches!(path, [] | ["machine_profiles"] | ["profiles"]);
    let tables = matches!(
        (local, remote),
        (Some(Value::Table(_)), Some(Value::Table(_)))
    );
    if let Base::Known(base) = base {
        if !(per_key && tables) {
            if local == base {
                return remote.cloned();
            }
            if remote == base {
                return local.cloned();
            }
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
            let mut out = Table::new();
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
        (Some(Value::Array(l)), Some(Value::Array(r))) if is_set_list(path) => {
            let base_items = match base {
                Base::Known(Some(Value::Array(b))) => Some(b.as_slice()),
                Base::Known(_) => Some(&[][..]),
                Base::Unknown => None,
            };
            Some(Value::Array(merge_set(ctx, path, base_items, l, r)))
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

/// Merges each item as a setting of its own: an item one side added or changed takes that
/// side, and an item one side removed goes. Without a base, nothing counts as removed.
fn merge_set(
    ctx: &mut Ctx,
    path: &[&str],
    base: Option<&[Value]>,
    local: &[Value],
    remote: &[Value],
) -> Vec<Value> {
    let find = |items: &[Value], key: &str| items.iter().find(|v| item_key(v) == key).cloned();
    let mut keys: Vec<String> = local.iter().chain(remote).map(item_key).collect();
    keys.sort();
    keys.dedup();
    let mut out = Vec::new();
    for key in keys {
        let (l, r) = (find(local, &key), find(remote, &key));
        let pick = match base {
            _ if l == r => l,
            Some(base) => {
                let b = find(base, &key);
                if l == b {
                    r
                } else if r == b {
                    l
                } else {
                    ctx.conflicts.push(format!("{} {}", path.join("."), key));
                    l
                }
            }
            None => l.or(r),
        };
        out.extend(pick);
    }
    out
}

/// A profile that an assignment names stays: the local or base definition comes back.
fn keep_assigned_profiles(merged: &mut Table, local: &Table, base: Option<&Table>) -> Vec<String> {
    let profiles_of = |t: &Table| t.get("profiles").and_then(|p| p.as_table()).cloned();
    let mut names: Vec<String> = merged
        .get("machine_profiles")
        .and_then(|m| m.as_table())
        .map(|m| {
            m.values()
                .filter_map(|v| v.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default();
    names.sort();
    names.dedup();
    let local_profiles = profiles_of(local).unwrap_or_default();
    let base_profiles = base.and_then(profiles_of).unwrap_or_default();
    let Some(Value::Table(profiles)) = merged.get_mut("profiles") else {
        return Vec::new();
    };
    let mut kept = Vec::new();
    for name in names {
        if profiles.contains_key(&name) {
            continue;
        }
        if let Some(def) = local_profiles.get(&name).or(base_profiles.get(&name)) {
            profiles.insert(name.clone(), def.clone());
            kept.push(name);
        }
    }
    kept
}

/// Edits `doc` from the settings `from` to the settings `to`. Keys that neither has, such as
/// keys from a newer Tether, and the comments around unchanged keys stay.
fn apply(doc: &mut dyn TableLike, from: &Table, to: &Table) {
    let mut keys: Vec<&String> = from.keys().chain(to.keys()).collect();
    keys.sort();
    keys.dedup();
    for key in keys {
        let (f, t) = (from.get(key), to.get(key));
        if f == t {
            continue;
        }
        match (f, t) {
            (_, None) => {
                doc.remove(key);
            }
            (Some(Value::Table(f)), Some(Value::Table(t)))
                if doc.get(key).is_some_and(|i| i.as_table_like().is_some()) =>
            {
                let child = doc.get_mut(key).and_then(|i| i.as_table_like_mut());
                apply(child.expect("checked above"), f, t);
            }
            (_, Some(t)) => {
                doc.insert(key, to_item(t));
            }
        }
    }
}

fn to_item(v: &Value) -> Item {
    match v {
        Value::Table(t) => {
            let mut table = toml_edit::Table::new();
            table.set_implicit(true);
            for (k, v) in t {
                table.insert(k, to_item(v));
            }
            Item::Table(table)
        }
        _ => Item::Value(to_value(v)),
    }
}

fn to_value(v: &Value) -> toml_edit::Value {
    match v {
        Value::String(s) => s.as_str().into(),
        Value::Integer(i) => (*i).into(),
        Value::Float(f) => (*f).into(),
        Value::Boolean(b) => (*b).into(),
        Value::Datetime(d) => (*d).into(),
        Value::Array(a) => toml_edit::Value::Array(a.iter().map(to_value).collect()),
        Value::Table(t) => toml_edit::Value::InlineTable(
            t.iter().map(|(k, v)| (k.as_str(), to_value(v))).collect(),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{DotfileEntry, ProfileConfig};

    fn base_config() -> String {
        let mut c = Config::default();
        c.machine_profiles.insert("a".into(), "dev".into());
        c.machine_profiles.insert("b".into(), "dev".into());
        c.profiles.insert("dev".into(), ProfileConfig::default());
        toml::to_string_pretty(&c).unwrap()
    }

    fn edit(text: &str, f: impl FnOnce(&mut Config)) -> String {
        let mut c = Config::parse(text).unwrap();
        f(&mut c);
        save_text(Some(text), &c).unwrap()
    }

    fn config(m: &Merged) -> Config {
        Config::parse(&m.text).unwrap()
    }

    /// A config as 1.x saves it: no keys 1.x does not know
    fn strip_2x(text: &str) -> String {
        let mut t: Table = toml::from_str(text).unwrap();
        for key in ["config_writer", "dashboard", "profiles", "machine_profiles"] {
            t.remove(key);
        }
        let packages = t["packages"].as_table_mut().unwrap();
        for key in [
            "allow_scripts",
            "min_release_age_days",
            "auto_install_from_trusted",
        ] {
            packages.remove(key);
        }
        toml::to_string_pretty(&t).unwrap()
    }

    #[test]
    fn format_only_rewrite_takes_remote_changes() {
        let base = base_config();
        // Same settings, other format: a comment, other spacing, another key order
        let local = format!("# rewritten\n{}", base.replace(" = ", "="));
        assert!(same_settings(base.as_bytes(), local.as_bytes()));
        let remote = edit(&base, |c| {
            c.machine_profiles.insert("b".into(), "linux-server".into());
            c.profiles
                .insert("linux-server".into(), ProfileConfig::default());
        });
        let m = merge(Some(&base), &local, &remote, "a").unwrap();
        assert!(m.changed);
        assert!(m.conflicts.is_empty());
        assert_eq!(config(&m).machine_profiles["b"], "linux-server");
        assert!(config(&m).profiles.contains_key("linux-server"));
        assert!(m.text.starts_with("# rewritten\n"), "{}", m.text);
    }

    #[test]
    fn a_written_out_default_is_no_change() {
        let base = base_config();
        let explicit = base.replace("[packages]\n", "[packages]\nallow_scripts = []\n");
        assert_ne!(explicit, base);
        assert!(same_settings(base.as_bytes(), explicit.as_bytes()));
        let m = merge(Some(&base), &base, &explicit, "a").unwrap();
        assert!(!m.changed);
    }

    #[test]
    fn edits_to_different_settings_both_stay() {
        let base = base_config();
        let local = edit(&base, |c| c.dashboard.theme = Some("mocha".into()));
        let remote = edit(&base, |c| c.packages.brew.sync_casks = false);
        let m = merge(Some(&base), &local, &remote, "a").unwrap();
        assert_eq!(config(&m).dashboard.theme.as_deref(), Some("mocha"));
        assert!(!config(&m).packages.brew.sync_casks);
        assert!(m.conflicts.is_empty());
    }

    #[test]
    fn both_changed_keeps_local_and_reports() {
        let base = base_config();
        let local = edit(&base, |c| c.packages.min_release_age_days = 3);
        let remote = edit(&base, |c| c.packages.min_release_age_days = 14);
        let m = merge(Some(&base), &local, &remote, "a").unwrap();
        assert_eq!(config(&m).packages.min_release_age_days, 3);
        assert_eq!(m.conflicts, vec!["packages.min_release_age_days"]);
    }

    #[test]
    fn unknown_keys_and_comments_survive() {
        let base = base_config();
        let local = base.replace(
            "[packages]\n",
            "# keep this comment\n[packages]\nfuture_local = \"x\"\n",
        );
        let remote = format!(
            "future_top = 1\n{}",
            edit(&base, |c| c.packages.npm.enabled = false)
        )
        .replace(
            "[packages.brew]\n",
            "[packages.brew]\nfuture_remote = [1, 2]\n",
        );
        let m = merge(Some(&base), &local, &remote, "a").unwrap();
        assert!(m.changed);
        assert!(m.text.contains("# keep this comment"), "{}", m.text);
        let t: Table = toml::from_str(&m.text).unwrap();
        assert_eq!(t["future_top"].as_integer(), Some(1));
        assert_eq!(t["packages"]["future_local"].as_str(), Some("x"));
        assert_eq!(
            t["packages"]["brew"]["future_remote"],
            Value::Array(vec![Value::Integer(1), Value::Integer(2)])
        );
        assert!(!config(&m).packages.npm.enabled);
        // A save keeps them too
        let saved = edit(&m.text, |c| c.dashboard.theme = Some("latte".into()));
        assert!(saved.contains("future_top = 1"), "{saved}");
        assert!(saved.contains("# keep this comment"), "{saved}");
    }

    #[test]
    fn stale_1x_export_keeps_2x_settings() {
        let base = edit(&base_config(), |c| {
            c.dashboard.theme = Some("mocha".into());
            c.packages.allow_scripts = vec!["esbuild".into()];
            c.packages.min_release_age_days = 3;
            c.machine_profiles.insert("b".into(), "server".into());
            c.profiles.insert("server".into(), ProfileConfig::default());
        });
        assert!(base.contains("config_writer"));
        // 1.x changed a setting it knows and dropped every key it does not
        let remote = strip_2x(&edit(&base, |c| c.packages.brew.sync_casks = false));
        assert!(!remote.contains("dashboard"));
        for (b, local) in [(Some(base.as_str()), base.clone()), (None, base.clone())] {
            let m = merge(b, &local, &remote, "a").unwrap();
            let c = config(&m);
            // Without a base, a setting that differs is a change on both sides
            assert_eq!(c.packages.brew.sync_casks, b.is_none());
            assert_eq!(c.dashboard.theme.as_deref(), Some("mocha"));
            assert_eq!(c.packages.allow_scripts, vec!["esbuild"]);
            assert_eq!(c.packages.min_release_age_days, 3);
            assert_eq!(c.machine_profiles["b"], "server");
            assert!(c.profiles.contains_key("server"));
            // The export pushes the merged config, not the stripped one
            assert!(!same_settings(m.text.as_bytes(), remote.as_bytes()));
        }
        // A 2.0 writer that drops a table deletes it
        let remote = edit(&base, |c| c.dashboard.theme = None);
        let m = merge(Some(&base), &base, &remote, "a").unwrap();
        assert_eq!(config(&m).dashboard.theme, None);
    }

    #[test]
    fn own_assignment_stays_and_others_take_remote() {
        let base = edit(&base_config(), |c| {
            c.profiles.insert("server".into(), ProfileConfig::default());
        });
        let local = edit(&base, |c| {
            c.machine_profiles.insert("a".into(), "server".into());
        });
        // The remote drops a's entry and reassigns b
        let remote = edit(&base, |c| {
            c.machine_profiles.remove("a");
            c.machine_profiles.insert("b".into(), "server".into());
        });
        let m = merge(Some(&base), &local, &remote, "a").unwrap();
        assert_eq!(config(&m).machine_profiles["a"], "server");
        assert_eq!(config(&m).machine_profiles["b"], "server");
        // Without a base too
        let m = merge(None, &local, &remote, "a").unwrap();
        assert_eq!(config(&m).machine_profiles["a"], "server");
        assert_eq!(config(&m).machine_profiles["b"], "server");
        // An unchanged local table still keeps the entry of this machine
        let m = merge(Some(&base), &base, &remote, "a").unwrap();
        assert_eq!(config(&m).machine_profiles["a"], "dev");
        assert_eq!(config(&m).machine_profiles["b"], "server");
    }

    #[test]
    fn assigned_profile_is_never_deleted() {
        let base = edit(&base_config(), |c| {
            c.machine_profiles.insert("a".into(), "server".into());
            c.profiles.insert(
                "server".into(),
                ProfileConfig {
                    packages: vec!["npm".into()],
                    ..Default::default()
                },
            );
        });
        let remote = edit(&base, |c| {
            c.profiles.remove("server");
        });
        let m = merge(Some(&base), &base, &remote, "a").unwrap();
        assert_eq!(config(&m).profiles["server"].packages, vec!["npm"]);
        assert_eq!(m.kept_profiles, vec!["server"]);
        assert!(!m.changed);
        // A profile no assignment names goes
        let unassigned = edit(&base, |c| {
            c.machine_profiles.insert("a".into(), "dev".into());
        });
        let remote = edit(&unassigned, |c| {
            c.profiles.remove("server");
        });
        let m = merge(Some(&unassigned), &unassigned, &remote, "a").unwrap();
        assert!(!config(&m).profiles.contains_key("server"));
    }

    #[test]
    fn profiles_merge_per_field() {
        let base = edit(&base_config(), |c| {
            c.profiles.insert(
                "dev".into(),
                ProfileConfig {
                    dirs: vec![".config/a".into()],
                    packages: vec!["brew".into()],
                    ..Default::default()
                },
            );
        });
        let local = edit(&base, |c| {
            let p = c.profiles.get_mut("dev").unwrap();
            p.dirs.push(".config/local".into());
            p.packages.push("npm".into());
        });
        let remote = edit(&base, |c| {
            let p = c.profiles.get_mut("dev").unwrap();
            p.dirs.retain(|d| d != ".config/a");
            p.dirs.insert(0, ".config/remote".into());
        });
        let m = merge(Some(&base), &local, &remote, "a").unwrap();
        let dev = &config(&m).profiles["dev"];
        assert_eq!(dev.dirs, vec![".config/local", ".config/remote"]);
        assert_eq!(dev.packages, vec!["brew", "npm"]);
        // The other machine writes the same order
        let m2 = merge(Some(&base), &remote, &local, "b").unwrap();
        assert_eq!(config(&m2).profiles["dev"].dirs, dev.dirs);
    }

    #[test]
    fn dotfile_lists_union_and_remove_once() {
        let base = edit(&base_config(), |c| {
            c.dotfiles.files = vec![
                DotfileEntry::Simple(".zshrc".into()),
                DotfileEntry::Simple(".vimrc".into()),
            ];
        });
        let local = edit(&base, |c| {
            c.dotfiles
                .files
                .push(DotfileEntry::Simple(".tmux.conf".into()));
        });
        let remote = edit(&base, |c| {
            c.dotfiles.files.retain(|f| f.path() != ".vimrc");
            c.dotfiles
                .files
                .push(DotfileEntry::Simple(".gitconfig".into()));
        });
        let m = merge(Some(&base), &local, &remote, "a").unwrap();
        let c = config(&m);
        let paths: Vec<&str> = c.dotfiles.files.iter().map(|f| f.path()).collect();
        assert_eq!(paths, vec![".gitconfig", ".tmux.conf", ".zshrc"]);
    }

    #[test]
    fn ordered_lists_merge_as_one_value() {
        let base = edit(&base_config(), |c| {
            c.merge.args = vec!["{local}".into(), "{remote}".into()];
        });
        let local = edit(&base, |c| {
            c.merge.args = vec!["-a".into(), "{local}".into()]
        });
        let remote = edit(&base, |c| {
            c.merge.args = vec!["{remote}".into(), "{local}".into()]
        });
        let m = merge(Some(&base), &local, &remote, "a").unwrap();
        assert_eq!(config(&m).merge.args, vec!["-a", "{local}"]);
        assert_eq!(m.conflicts, vec!["merge.args"]);
        // One side reorders: that order wins
        let m = merge(Some(&base), &base, &remote, "a").unwrap();
        assert_eq!(config(&m).merge.args, vec!["{remote}", "{local}"]);
        // A set list in another order holds the same settings
        let one = edit(&base, |c| {
            c.packages.allow_scripts = vec!["a".into(), "b".into()]
        });
        let two = edit(&base, |c| {
            c.packages.allow_scripts = vec!["b".into(), "a".into()]
        });
        assert!(same_settings(one.as_bytes(), two.as_bytes()));
        assert!(!merge(Some(&one), &one, &two, "a").unwrap().changed);
    }

    #[test]
    fn cleared_list_stays_cleared() {
        let base = edit(&base_config(), |c| {
            c.packages.allow_scripts = vec!["esbuild".into()];
        });
        let remote = edit(&base, |c| c.packages.allow_scripts.clear());
        assert!(!remote.contains("allow_scripts"), "{remote}");
        let m = merge(Some(&base), &base, &remote, "a").unwrap();
        assert!(config(&m).packages.allow_scripts.is_empty());
        // The next round, with the remote as base, keeps it empty
        let again = merge(Some(&remote), &m.text, &remote, "a").unwrap();
        assert!(!again.changed);
        assert!(config(&again).packages.allow_scripts.is_empty());
    }

    #[test]
    fn unknown_base_keeps_additions_from_both() {
        let base = base_config();
        let local = edit(&base, |c| c.dashboard.theme = Some("mocha".into()));
        let remote = edit(&base, |c| {
            c.profiles
                .insert("linux-server".into(), ProfileConfig::default());
            c.machine_profiles.insert("b".into(), "linux-server".into());
        });
        let m = merge(None, &local, &remote, "a").unwrap();
        assert_eq!(config(&m).dashboard.theme.as_deref(), Some("mocha"));
        assert_eq!(config(&m).machine_profiles["b"], "linux-server");
        assert!(config(&m).profiles.contains_key("linux-server"));
    }

    #[test]
    fn merged_config_keeps_1x_fields() {
        let base = base_config();
        let local = edit(&base, |c| c.dashboard.theme = Some("mocha".into()));
        let m = merge(Some(&base), &local, &base, "a").unwrap();
        assert!(m.text.contains("sync_versions = false"), "{}", m.text);
    }

    /// Two machines and a repo, as sync runs them: merge, then export when the settings
    /// differ. The base follows the merged remote, then the export.
    #[test]
    fn same_setting_changed_on_two_machines_converges() {
        struct M {
            id: &'static str,
            local: String,
            base: String,
        }
        let start = base_config();
        let mut repo = start.clone();
        let mut a = M {
            id: "a",
            local: edit(&start, |c| c.packages.min_release_age_days = 3),
            base: start.clone(),
        };
        let mut b = M {
            id: "b",
            local: edit(&start, |c| c.packages.min_release_age_days = 14),
            base: start.clone(),
        };
        let sync = |m: &mut M, repo: &mut String| -> bool {
            let merged = merge(Some(&m.base), &m.local, repo, m.id).unwrap();
            m.local = merged.text;
            m.base = repo.clone();
            if same_settings(repo.as_bytes(), m.local.as_bytes()) {
                return false;
            }
            *repo = m.local.clone();
            m.base = m.local.clone();
            true
        };
        let mut exports = Vec::new();
        for _ in 0..2 {
            exports.push(sync(&mut a, &mut repo));
            exports.push(sync(&mut b, &mut repo));
        }
        let days = |t: &str| Config::parse(t).unwrap().packages.min_release_age_days;
        assert_eq!(days(&a.local), days(&b.local));
        assert_eq!(days(&a.local), days(&repo));
        // Settled: a further round exports nothing
        assert!(
            !sync(&mut a, &mut repo) && !sync(&mut b, &mut repo),
            "{exports:?}"
        );
    }

    #[test]
    fn an_export_names_its_parent_and_compares_unchanged() {
        let local = base_config();
        let exported = with_parent(&local, "abc");
        assert!(exported.contains("config_parent = \"abc\""), "{exported}");
        assert_ne!(with_parent(&local, "abc"), with_parent(&local, "def"));
        assert!(same_settings(local.as_bytes(), exported.as_bytes()));
    }

    #[test]
    fn newer_config_version_is_detected() {
        assert_eq!(newer_version("config_version = 3\n"), Some(3));
        assert_eq!(newer_version(&base_config()), None);
    }
}

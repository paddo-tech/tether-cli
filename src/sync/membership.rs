//! The profiles each package belongs to. A machine installs a synced package only when its
//! profile is a member of that package.

use crate::config::{Config, DEFAULT_PROFILE};
use crate::packages::normalize_formula_name;
use crate::sync::signing::{self, RecordStatus};
use crate::sync::{GitBackend, MachineState, SyncEngine, SyncState};
use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::path::Path;

/// Package membership in the sync repo. A file of its own, not config.toml, because 1.x
/// rewrites config.toml and would drop it. A repo writer could widen membership, but a
/// package still installs only when a trusted signed record lists it.
pub const FILE: &str = "packages/profiles.toml";

/// `manager:name` to the profiles the package belongs to.
pub type Table = BTreeMap<String, Vec<String>>;

#[derive(Debug, Default, Serialize, Deserialize)]
struct ProfilesFile {
    #[serde(default)]
    profiles: Table,
}

/// The table in the sync repo. A missing or unreadable file is an empty table, so every
/// package falls back to implicit membership.
pub fn read_table(sync_path: &Path) -> Table {
    let Ok(text) = std::fs::read_to_string(sync_path.join(FILE)) else {
        return Table::new();
    };
    match toml::from_str::<ProfilesFile>(&text) {
        Ok(file) => file.profiles,
        Err(e) => {
            crate::cli::Output::warning(&format!("Cannot read {}: {}", FILE, e));
            Table::new()
        }
    }
}

/// Apply one package's new members to the file text. Returns the new text, and whether
/// another machine changed the entry since this machine read it as `base`.
fn merge_entry(
    text: &str,
    id: &str,
    base: &BTreeSet<String>,
    members: &BTreeSet<String>,
) -> Result<(String, bool)> {
    let mut file: ProfilesFile = toml::from_str(text)?;
    let conflict = file
        .profiles
        .get(id)
        .is_some_and(|now| now.iter().cloned().collect::<BTreeSet<_>>() != *base);
    file.profiles
        .insert(id.to_string(), members.iter().cloned().collect());
    Ok((toml::to_string_pretty(&file)?, conflict))
}

/// Set one package's members in the repo and push. Under the sync lock, each attempt pulls,
/// reads the file again and changes only this entry, so edits to other packages from other
/// machines are kept. An edit to the same package from another machine is replaced: the
/// last writer wins, with a warning.
pub fn save_members(
    sync_path: &Path,
    id: &str,
    base: &BTreeSet<String>,
    members: &BTreeSet<String>,
) -> Result<()> {
    let _sync_lock = crate::sync::acquire_sync_lock(true)?;
    let repo = GitBackend::open(sync_path)?;
    let path = sync_path.join(FILE);
    let mut last_error = None;
    for _ in 0..3 {
        repo.pull()?;
        let text = std::fs::read_to_string(&path).unwrap_or_default();
        let (text, conflict) = merge_entry(&text, id, base, members)?;
        if conflict {
            crate::cli::Output::warning(&format!(
                "Another machine changed the profiles of {} since this machine read them. \
                 This edit replaces that change",
                id
            ));
        }
        std::fs::create_dir_all(path.parent().expect("FILE has a directory"))?;
        crate::sync::atomic_write(&path, text.as_bytes())?;
        repo.commit(
            &format!("Set profiles of {}", id),
            &crate::sync::local_hostname(),
        )?;
        match repo.push() {
            Ok(()) => return Ok(()),
            Err(e) => last_error = Some(e),
        }
    }
    Err(last_error.expect("three attempts failed"))
}

/// Manager key and name.
type Key = (String, String);

/// A Homebrew formula or cask may be named with or without its tap.
fn key(manager: &str, name: &str) -> Key {
    let name = if manager == "brew_formulae" || manager == "brew_casks" {
        normalize_formula_name(name)
    } else {
        name
    };
    (manager.to_string(), name.to_string())
}

/// A machine without a profile counts as the default profile, as everywhere else in Tether.
fn record_profile(record: &MachineState) -> &str {
    record.profile.as_deref().unwrap_or(DEFAULT_PROFILE)
}

pub struct Membership {
    /// This machine's profile
    pub profile: String,
    /// Entries in the table. An entry is authoritative
    explicit: HashMap<Key, BTreeSet<String>>,
    /// Profiles of this machine and of each trusted machine whose record lists the package
    trusted: HashMap<Key, BTreeSet<String>>,
    /// Profiles of the other records that list the package. Their profile is not verified,
    /// so they count only for a package no trusted record lists, which can only wait in the
    /// inbox
    untrusted: HashMap<Key, BTreeSet<String>>,
    /// Listed by this machine's record
    own: HashSet<Key>,
}

impl Membership {
    /// `others` are the other machines' records, each with whether it is trusted.
    pub fn new(
        config: &Config,
        table: &Table,
        this: &MachineState,
        others: &[(&MachineState, bool)],
    ) -> Self {
        let profile = config.profile_name(&this.machine_id).to_string();
        let explicit = table
            .iter()
            .filter_map(|(id, members)| {
                let (manager, name) = id.split_once(':')?;
                Some((key(manager, name), members.iter().cloned().collect()))
            })
            .collect();
        let mut membership = Self {
            profile: profile.clone(),
            explicit,
            trusted: HashMap::new(),
            untrusted: HashMap::new(),
            own: HashSet::new(),
        };
        for (manager, names) in &this.packages {
            for name in names {
                membership.own.insert(key(manager, name));
                membership
                    .trusted
                    .entry(key(manager, name))
                    .or_default()
                    .insert(profile.clone());
            }
        }
        for (record, trusted) in others {
            let map = if *trusted {
                &mut membership.trusted
            } else {
                &mut membership.untrusted
            };
            for (manager, names) in &record.packages {
                for name in names {
                    map.entry(key(manager, name))
                        .or_default()
                        .insert(record_profile(record).to_string());
                }
            }
        }
        membership
    }

    /// Read the table and the records in the repo as a sync does. `this` is this machine's
    /// current record.
    pub fn load(config: &Config, sync_path: &Path, this: &MachineState) -> Result<Self> {
        let statuses = signing::record_statuses(sync_path, &this.machine_id)?;
        let records = signing::records(sync_path);
        let others: Vec<(&MachineState, bool)> = records
            .iter()
            .map(|r| &r.record)
            .filter(|r| r.machine_id != this.machine_id)
            .map(|r| {
                let trusted = statuses
                    .iter()
                    .any(|(id, status, _)| *id == r.machine_id && *status == RecordStatus::Trusted);
                (r, trusted)
            })
            .collect();
        Ok(Self::new(config, &read_table(sync_path), this, &others))
    }

    /// For commands outside a sync: this machine's last saved record, and the repo as it is.
    pub fn load_current(config: &Config) -> Result<Self> {
        let state = SyncState::load()?;
        let sync_path = SyncEngine::sync_path()?;
        let this = signing::own_record(&sync_path, &state.machine_id)?
            .unwrap_or_else(|| MachineState::new(&state.machine_id));
        Self::load(config, &sync_path, &this)
    }

    /// The profiles the package belongs to: its table entry, else the profiles of the
    /// trusted machines that list it.
    pub fn members(&self, manager: &str, name: &str) -> BTreeSet<String> {
        let k = key(manager, name);
        self.explicit
            .get(&k)
            .or_else(|| self.trusted.get(&k))
            .or_else(|| self.untrusted.get(&k))
            .cloned()
            .unwrap_or_default()
    }

    /// Whether this machine installs the package from other machines. A package this
    /// machine lists always counts.
    pub fn includes(&self, manager: &str, name: &str) -> bool {
        self.own.contains(&key(manager, name))
            || self.members(manager, name).contains(&self.profile)
    }

    /// Packages that trusted records list, that this machine does not list, and whose
    /// members do not include this machine's profile, as `manager:name`, sorted.
    pub fn excluded(&self) -> Vec<String> {
        let mut ids: Vec<String> = self
            .trusted
            .keys()
            .filter(|(m, n)| !self.includes(m, n))
            .map(|(m, n)| format!("{}:{}", m, n))
            .collect();
        ids.sort();
        ids
    }

    /// The members after adding `profiles`. The profiles that have the package keep it.
    pub fn shared(&self, manager: &str, name: &str, profiles: &[String]) -> BTreeSet<String> {
        let mut members = self.members(manager, name);
        members.extend(profiles.iter().cloned());
        members
    }

    /// The members after this machine's profile leaves, so other machines in it stop
    /// installing the package. None when this profile was its only member: removing it here
    /// is then a plain uninstall.
    pub fn left(&self, manager: &str, name: &str) -> Option<BTreeSet<String>> {
        let mut members = self.members(manager, name);
        members.remove(&self.profile);
        (!members.is_empty()).then_some(members)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(id: &str, profile: Option<&str>, packages: &[(&str, &str)]) -> MachineState {
        let mut m = MachineState::new(id);
        m.profile = profile.map(str::to_string);
        for (manager, name) in packages {
            m.packages
                .entry(manager.to_string())
                .or_default()
                .push(name.to_string());
        }
        m
    }

    fn config(assign: &[(&str, &str)]) -> Config {
        let mut c = Config::default();
        for (id, profile) in assign {
            c.machine_profiles
                .insert(id.to_string(), profile.to_string());
        }
        c
    }

    fn set(profiles: &[&str]) -> BTreeSet<String> {
        profiles.iter().map(|p| p.to_string()).collect()
    }

    fn entry(table: &mut Table, id: &str, members: BTreeSet<String>) {
        table.insert(id.to_string(), members.into_iter().collect());
    }

    fn fleet() -> (Config, MachineState, MachineState, MachineState) {
        (
            config(&[("mac1", "dev"), ("mac2", "dev"), ("server", "server")]),
            record(
                "mac1",
                Some("dev"),
                &[("npm", "typescript"), ("brew_casks", "zoom")],
            ),
            record(
                "mac2",
                Some("dev"),
                &[("npm", "typescript"), ("uv", "ruff")],
            ),
            record("server", Some("server"), &[("brew_formulae", "nginx")]),
        )
    }

    #[test]
    fn implicit_membership_keeps_packages_in_their_profile() {
        let (c, mac1, mac2, server) = fleet();
        let t = Table::new();

        let on_server = Membership::new(&c, &t, &server, &[(&mac1, true), (&mac2, true)]);
        assert!(!on_server.includes("brew_casks", "zoom"));
        assert!(!on_server.includes("uv", "ruff"));
        assert!(on_server.includes("brew_formulae", "nginx"));
        assert_eq!(
            on_server.excluded(),
            vec!["brew_casks:zoom", "npm:typescript", "uv:ruff"]
        );

        let on_mac1 = Membership::new(&c, &t, &mac1, &[(&mac2, true), (&server, true)]);
        assert!(on_mac1.includes("uv", "ruff"));
        assert!(!on_mac1.includes("brew_formulae", "nginx"));
    }

    #[test]
    fn explicit_entry_is_authoritative() {
        let (c, mac1, mac2, server) = fleet();
        let mut t = Table::new();
        entry(&mut t, "uv:ruff", set(&["server"]));

        let on_server = Membership::new(&c, &t, &server, &[(&mac1, true), (&mac2, true)]);
        assert!(on_server.includes("uv", "ruff"));
        // mac2 lists ruff, but the entry leaves dev out, so mac1 no longer installs it
        let on_mac1 = Membership::new(&c, &t, &mac1, &[(&mac2, true), (&server, true)]);
        assert!(!on_mac1.includes("uv", "ruff"));
        // A machine always keeps what it lists itself
        let on_mac2 = Membership::new(&c, &t, &mac2, &[(&mac1, true), (&server, true)]);
        assert!(on_mac2.includes("uv", "ruff"));
    }

    #[test]
    fn share_adds_profiles_to_the_implicit_members() {
        let (c, mac1, mac2, server) = fleet();
        let mut t = Table::new();
        let on_mac1 = Membership::new(&c, &t, &mac1, &[(&mac2, true), (&server, true)]);
        let members = on_mac1.shared("brew_casks", "zoom", &["server".to_string()]);
        assert_eq!(members, set(&["dev", "server"]));
        entry(&mut t, "brew_casks:zoom", members);

        let on_server = Membership::new(&c, &t, &server, &[(&mac1, true), (&mac2, true)]);
        assert!(on_server.includes("brew_casks", "zoom"));
        assert!(!on_server.includes("uv", "ruff"));
    }

    #[test]
    fn leave_keeps_the_other_profiles_enrolled() {
        let (c, mac1, mac2, mut server) = fleet();
        let mut t = Table::new();
        entry(&mut t, "brew_casks:zoom", set(&["dev", "server"]));
        server
            .packages
            .insert("brew_casks".to_string(), vec!["zoom".to_string()]);

        let on_server = Membership::new(&c, &t, &server, &[(&mac1, true), (&mac2, true)]);
        let members = on_server.left("brew_casks", "zoom");
        assert_eq!(members, Some(set(&["dev"])));
        entry(&mut t, "brew_casks:zoom", members.unwrap());

        // After the uninstall, the server's record no longer lists it
        server.packages.remove("brew_casks");
        let on_server = Membership::new(&c, &t, &server, &[(&mac1, true), (&mac2, true)]);
        assert!(!on_server.includes("brew_casks", "zoom"));
        let on_mac2 = Membership::new(&c, &t, &mac2, &[(&mac1, true), (&server, true)]);
        assert!(on_mac2.includes("brew_casks", "zoom"));
    }

    #[test]
    fn leave_by_the_only_member_is_a_plain_uninstall() {
        let (c, mac1, mac2, server) = fleet();
        let on_mac1 = Membership::new(&c, &Table::new(), &mac1, &[(&mac2, true), (&server, true)]);
        assert_eq!(on_mac1.left("brew_casks", "zoom"), None);
    }

    #[test]
    fn machine_without_profile_counts_as_default() {
        let (_, mac1, _, server) = fleet();
        let bare = record("bare", None, &[("npm", "left-pad")]);
        // The config assigns no profile to "bare", and its record names none
        let c = config(&[("mac1", "dev"), ("server", "server")]);
        let t = Table::new();

        let on_bare = Membership::new(&c, &t, &bare, &[(&mac1, true), (&server, true)]);
        assert_eq!(on_bare.profile, DEFAULT_PROFILE);
        assert!(on_bare.includes("brew_casks", "zoom"));
        assert!(!on_bare.includes("brew_formulae", "nginx"));

        let on_mac1 = Membership::new(&c, &t, &mac1, &[(&bare, true), (&server, true)]);
        assert!(on_mac1.includes("npm", "left-pad"));
    }

    #[test]
    fn own_record_always_counts() {
        let (c, mac1, mac2, server) = fleet();
        let mut t = Table::new();
        entry(&mut t, "brew_formulae:nginx", set(&["dev"]));
        let on_server = Membership::new(&c, &t, &server, &[(&mac1, true), (&mac2, true)]);
        assert!(on_server.includes("brew_formulae", "nginx"));
    }

    #[test]
    fn untrusted_records_never_widen_a_trusted_package() {
        let (c, mac1, _, server) = fleet();
        // An unsigned record that claims the server profile and lists a dev package
        let forged = record(
            "forged",
            Some("server"),
            &[("brew_casks", "zoom"), ("npm", "only-forged")],
        );
        let on_server = Membership::new(
            &c,
            &Table::new(),
            &server,
            &[(&mac1, true), (&forged, false)],
        );
        assert!(!on_server.includes("brew_casks", "zoom"));
        // A package no trusted record lists can only wait in the inbox
        assert!(on_server.includes("npm", "only-forged"));
    }

    #[test]
    fn removal_on_one_machine_does_not_change_other_profiles() {
        let (c, mut mac1, mac2, server) = fleet();
        let mut t = Table::new();
        entry(&mut t, "npm:typescript", set(&["dev", "server"]));
        // mac1 uninstalls typescript: its record drops it and keeps a tombstone
        mac1.packages
            .get_mut("npm")
            .unwrap()
            .retain(|n| n != "typescript");
        mac1.removed_packages
            .insert("npm".to_string(), vec!["typescript".to_string()]);

        let on_server = Membership::new(&c, &t, &server, &[(&mac1, true), (&mac2, true)]);
        assert!(on_server.includes("npm", "typescript"));
        let on_mac2 = Membership::new(&c, &t, &mac2, &[(&mac1, true), (&server, true)]);
        assert!(on_mac2.includes("npm", "typescript"));
    }

    #[test]
    fn brew_names_match_with_or_without_tap() {
        let (c, mac1, mac2, server) = fleet();
        let mut t = Table::new();
        entry(&mut t, "brew_casks:homebrew/cask/zoom", set(&["server"]));
        let on_server = Membership::new(&c, &t, &server, &[(&mac1, true), (&mac2, true)]);
        assert!(on_server.includes("brew_casks", "zoom"));
    }

    #[test]
    fn settle_drops_items_for_packages_the_profile_left() {
        use crate::packages::inbox::{Inbox, InboxItem, Kind, Reason};
        let (c, mac1, mac2, server) = fleet();
        let mut t = Table::new();
        entry(&mut t, "uv:ruff", set(&["dev", "server"]));
        let mut inbox = Inbox::default();
        inbox.items.push(InboxItem {
            kind: Kind::Package,
            manager: "uv".to_string(),
            name: "ruff".to_string(),
            version: None,
            tap: None,
            source_machine: Some("mac2".to_string()),
            commit: None,
            signer: None,
            reasons: vec![Reason::Unsigned],
            advisories: Vec::new(),
            first_seen: chrono::Utc::now(),
        });

        let on_server = Membership::new(&c, &t, &server, &[(&mac1, true), (&mac2, true)]);
        inbox.settle(Vec::new(), &[], |m, n| on_server.includes(m, n));
        assert_eq!(inbox.items.len(), 1);

        entry(&mut t, "uv:ruff", on_server.left("uv", "ruff").unwrap());
        let on_server = Membership::new(&c, &t, &server, &[(&mac1, true), (&mac2, true)]);
        inbox.settle(Vec::new(), &[], |m, n| on_server.includes(m, n));
        assert!(inbox.items.is_empty());
    }

    #[test]
    fn merge_entry_keeps_other_packages_and_reports_a_conflict() {
        let text = "[profiles]\n\"npm:a\" = [\"dev\"]\n\"npm:b\" = [\"dev\", \"server\"]\n";
        let (merged, conflict) =
            merge_entry(text, "npm:c", &BTreeSet::new(), &set(&["server"])).unwrap();
        assert!(!conflict);
        let file: ProfilesFile = toml::from_str(&merged).unwrap();
        assert_eq!(file.profiles.len(), 3);
        assert_eq!(file.profiles["npm:b"], vec!["dev", "server"]);
        assert_eq!(file.profiles["npm:c"], vec!["server"]);
        // Sorted, so two machines write the same bytes for the same table
        assert!(merged.find("npm:a").unwrap() < merged.find("npm:c").unwrap());

        // Another machine changed npm:b after this machine read it as dev only
        let (_, conflict) = merge_entry(text, "npm:b", &set(&["dev"]), &set(&["server"])).unwrap();
        assert!(conflict);
    }

    #[test]
    fn table_ignores_unknown_fields() {
        let file: ProfilesFile =
            toml::from_str("version = 2\n[profiles]\n\"npm:a\" = [\"dev\"]\n[later]\nx = 1\n")
                .unwrap();
        assert_eq!(file.profiles["npm:a"], vec!["dev"]);
    }
}

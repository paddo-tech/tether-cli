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

/// The table in the sync repo. A missing file is an empty table, so every package has
/// implicit membership. A file that exists but does not read is an error, never an empty
/// table: implicit membership would widen every package the table narrows.
pub fn read_table(sync_path: &Path) -> Result<Table> {
    let text = match std::fs::read_to_string(sync_path.join(FILE)) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Table::new()),
        Err(e) => anyhow::bail!("Cannot read {} in the sync repo: {}", FILE, e),
    };
    toml::from_str::<ProfilesFile>(&text)
        .map(|file| file.profiles)
        .map_err(|e| anyhow::anyhow!("Cannot read {} in the sync repo: {}", FILE, e))
}

/// Apply one package's new members to the file text.
fn merge_entry(text: &str, id: &str, members: &BTreeSet<String>) -> Result<String> {
    let mut file: ProfilesFile = toml::from_str(text)?;
    // The canonical entry replaces any other spelling of the same package
    let canonical = id.split_once(':').map(|(manager, name)| key(manager, name));
    file.profiles
        .retain(|other, _| other.split_once(':').map(|(m, n)| key(m, n)) != canonical);
    file.profiles
        .insert(id.to_string(), members.iter().cloned().collect());
    Ok(toml::to_string_pretty(&file)?)
}

/// A change to one package's members. A save applies it to the members as the remote has
/// them, so a change another machine made in the meantime stays.
#[derive(Debug, Clone, Default)]
pub struct Edit {
    pub add: BTreeSet<String>,
    pub remove: BTreeSet<String>,
}

impl Edit {
    /// The members after the edit.
    pub fn apply(&self, members: &BTreeSet<String>) -> BTreeSet<String> {
        members
            .iter()
            .chain(&self.add)
            .filter(|p| !self.remove.contains(*p))
            .cloned()
            .collect()
    }
}

/// Apply `edit` to one package's members and push it. Returns the new members, or None
/// when the edit would leave the package with no member: removing the last profile is a
/// plain uninstall.
pub fn save_edit(
    config: &Config,
    manager: &str,
    name: &str,
    edit: &Edit,
) -> Result<Option<BTreeSet<String>>> {
    let sync_path = SyncEngine::sync_path()?;
    let machine_id = SyncState::load()?.machine_id;
    save_members(&sync_path, &canonical_id(manager, name), edit, |table| {
        let this = signing::own_record(&sync_path, &machine_id)?
            .unwrap_or_else(|| MachineState::new(&machine_id));
        Ok(Membership::load(config, &sync_path, &this, table)?.members(manager, name))
    })
}

/// Apply `edit` to the members `members_now` reads from the table, and push the entry.
/// Under the sync lock, each attempt resets to the remote branch and reads the table again,
/// so edits from other machines are kept.
fn save_members(
    sync_path: &Path,
    id: &str,
    edit: &Edit,
    members_now: impl Fn(&Table) -> Result<BTreeSet<String>>,
) -> Result<Option<BTreeSet<String>>> {
    let _sync_lock = crate::sync::acquire_sync_lock(true)?;
    let repo = GitBackend::open(sync_path)?;
    // Each attempt resets to the remote branch, which must drop only this edit's commit
    if repo.has_changes()? || repo.has_unpushed_commits() {
        anyhow::bail!(
            "The sync repo has changes that are not pushed. Run 'tether sync', then try again"
        );
    }
    let path = sync_path.join(FILE);
    let mut last_error = None;
    for _ in 0..3 {
        repo.fetch()?;
        repo.reset_to_remote()?;
        let table = read_table(sync_path)?;
        let members = edit.apply(&members_now(&table)?);
        if members.is_empty() {
            return Ok(None);
        }
        let previous = match std::fs::read_to_string(&path) {
            Ok(text) => Some(text),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
            Err(e) => return Err(e.into()),
        };
        let text = merge_entry(previous.as_deref().unwrap_or_default(), id, &members)?;
        let written = (|| {
            std::fs::create_dir_all(path.parent().expect("FILE has a directory"))?;
            crate::sync::atomic_write(&path, text.as_bytes())?;
            repo.commit(
                &format!("Set profiles of {}", id),
                &crate::sync::local_hostname(),
            )
        })();
        let committed = written.is_ok();
        let pushed = written.and_then(|()| repo.push_once());
        let Err(e) = pushed else {
            return Ok(Some(members));
        };
        // A failed attempt leaves the repo as the remote has it, so a sync never pushes it
        repo.reset_to_remote()?;
        if previous.is_none() {
            match std::fs::remove_file(&path) {
                Err(e) if e.kind() != std::io::ErrorKind::NotFound => return Err(e.into()),
                _ => {}
            }
        }
        if !committed {
            return Err(e);
        }
        last_error = Some(e);
    }
    Err(last_error.expect("three attempts failed"))
}

/// Manager key and name.
type Key = (String, String);

/// A Homebrew formula or cask may be named with or without its tap. A Python package name
/// compares as PEP 503 normalizes it. npm, pnpm, bun and gem names are case-sensitive.
fn key(manager: &str, name: &str) -> Key {
    let name = match manager {
        "brew_formulae" | "brew_casks" => normalize_formula_name(name).to_string(),
        "uv" => pep503(name),
        _ => name.to_string(),
    };
    (manager.to_string(), name)
}

/// Lowercase, with each run of `-`, `_` and `.` as one `-`.
fn pep503(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    for c in name.chars() {
        if matches!(c, '-' | '_' | '.') {
            if !out.ends_with('-') {
                out.push('-');
            }
        } else {
            out.extend(c.to_lowercase());
        }
    }
    out
}

/// The table id for a package: `manager:name`, with the name as reads compare it. Writes
/// use it, so one package never has two entries.
pub fn canonical_id(manager: &str, name: &str) -> String {
    let (manager, name) = key(manager, name);
    format!("{}:{}", manager, name)
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
        // Entries that name one package two ways count together
        let mut explicit: HashMap<Key, BTreeSet<String>> = HashMap::new();
        for (id, members) in table {
            if let Some((manager, name)) = id.split_once(':') {
                explicit
                    .entry(key(manager, name))
                    .or_default()
                    .extend(members.iter().cloned());
            }
        }
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

    /// Read the records in the repo as a sync does. `this` is this machine's current record.
    pub fn load(
        config: &Config,
        sync_path: &Path,
        this: &MachineState,
        table: &Table,
    ) -> Result<Self> {
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
        Ok(Self::new(config, table, this, &others))
    }

    /// For commands outside a sync: this machine's last saved record, and the repo as it is.
    pub fn load_current(config: &Config) -> Result<Self> {
        let state = SyncState::load()?;
        let sync_path = SyncEngine::sync_path()?;
        let this = signing::own_record(&sync_path, &state.machine_id)?
            .unwrap_or_else(|| MachineState::new(&state.machine_id));
        Self::load(config, &sync_path, &this, &read_table(&sync_path)?)
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

    fn share(profiles: &[&str]) -> Edit {
        Edit {
            add: set(profiles),
            ..Edit::default()
        }
    }

    fn leave(profile: &str) -> Edit {
        Edit {
            remove: set(&[profile]),
            ..Edit::default()
        }
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
        let members = share(&["server"]).apply(&on_mac1.members("brew_casks", "zoom"));
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
        let members = leave(&on_server.profile).apply(&on_server.members("brew_casks", "zoom"));
        assert_eq!(members, set(&["dev"]));
        entry(&mut t, "brew_casks:zoom", members);

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
        assert!(leave("dev")
            .apply(&on_mac1.members("brew_casks", "zoom"))
            .is_empty());
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

        entry(
            &mut t,
            "uv:ruff",
            leave("server").apply(&on_server.members("uv", "ruff")),
        );
        let on_server = Membership::new(&c, &t, &server, &[(&mac1, true), (&mac2, true)]);
        inbox.settle(Vec::new(), &[], |m, n| on_server.includes(m, n));
        assert!(inbox.items.is_empty());
    }

    #[test]
    fn merge_entry_keeps_other_packages() {
        let text = "[profiles]\n\"npm:a\" = [\"dev\"]\n\"npm:b\" = [\"dev\", \"server\"]\n";
        let merged = merge_entry(text, "npm:c", &set(&["server"])).unwrap();
        let file: ProfilesFile = toml::from_str(&merged).unwrap();
        assert_eq!(file.profiles.len(), 3);
        assert_eq!(file.profiles["npm:b"], vec!["dev", "server"]);
        assert_eq!(file.profiles["npm:c"], vec!["server"]);
        // Sorted, so two machines write the same bytes for the same table
        assert!(merged.find("npm:a").unwrap() < merged.find("npm:c").unwrap());
    }

    #[test]
    fn ids_are_canonical_and_aliases_merge() {
        assert_eq!(canonical_id("uv", "Ruamel.YAML__x"), "uv:ruamel-yaml-x");
        assert_eq!(
            canonical_id("brew_casks", "homebrew/cask/zoom"),
            "brew_casks:zoom"
        );
        assert_eq!(canonical_id("npm", "JSONStream"), "npm:JSONStream");

        let (c, mac1, mac2, server) = fleet();
        let mut t = Table::new();
        entry(&mut t, "uv:Ruff", set(&["server"]));
        entry(&mut t, "uv:ruff", set(&["dev"]));
        let on_server = Membership::new(&c, &t, &server, &[(&mac1, true), (&mac2, true)]);
        assert_eq!(on_server.members("uv", "RUFF"), set(&["dev", "server"]));

        // A write keeps one entry under the canonical id
        let text = "[profiles]\n\"uv:Ruff\" = [\"server\"]\n\"uv:ruff\" = [\"dev\"]\n";
        let merged = merge_entry(text, "uv:ruff", &set(&["dev", "server"])).unwrap();
        let file: ProfilesFile = toml::from_str(&merged).unwrap();
        assert_eq!(file.profiles.len(), 1);
        assert_eq!(file.profiles["uv:ruff"], vec!["dev", "server"]);
    }

    #[test]
    fn an_unreadable_table_is_an_error_not_an_empty_table() {
        let dir = tempfile::tempdir().unwrap();
        assert!(read_table(dir.path()).unwrap().is_empty());
        std::fs::create_dir_all(dir.path().join("packages")).unwrap();
        std::fs::write(dir.path().join(FILE), "[profiles]\n\"npm:a\" = \"dev\"\n").unwrap();
        assert!(read_table(dir.path()).is_err());
        std::fs::write(dir.path().join(FILE), "[profiles]\n\"npm:a\" = [\"dev\"]\n").unwrap();
        assert_eq!(read_table(dir.path()).unwrap()["npm:a"], vec!["dev"]);
    }

    #[test]
    fn table_ignores_unknown_fields() {
        let file: ProfilesFile =
            toml::from_str("version = 2\n[profiles]\n\"npm:a\" = [\"dev\"]\n[later]\nx = 1\n")
                .unwrap();
        assert_eq!(file.profiles["npm:a"], vec!["dev"]);
    }

    fn git(dir: &Path, args: &[&str]) {
        let status = std::process::Command::new("git")
            .args(args)
            .current_dir(dir)
            .output()
            .unwrap();
        assert!(status.status.success(), "git {:?}: {:?}", args, status);
    }

    fn read_entries(dir: &Path) -> Table {
        read_table(dir).unwrap()
    }

    /// The members a table entry lists, as `save_members` reads them.
    fn listed(id: &'static str) -> impl Fn(&Table) -> Result<BTreeSet<String>> {
        move |table| {
            Ok(table
                .get(id)
                .map(|m| m.iter().cloned().collect())
                .unwrap_or_default())
        }
    }

    /// A bare remote whose main has the table, and two clones of it.
    fn remote_with_clones(
        root: &Path,
        table: &str,
    ) -> (std::path::PathBuf, std::path::PathBuf, std::path::PathBuf) {
        let remote = root.join("remote.git");
        let seed = root.join("seed");
        std::fs::create_dir_all(&remote).unwrap();
        std::fs::create_dir_all(seed.join("packages")).unwrap();
        git(&remote, &["init", "-q", "--bare", "-b", "main"]);
        git(&seed, &["init", "-q", "-b", "main"]);
        std::fs::write(seed.join(FILE), table).unwrap();
        GitBackend::new(seed.clone())
            .commit("seed", "test")
            .unwrap();
        git(
            &seed,
            &["remote", "add", "origin", remote.to_str().unwrap()],
        );
        git(&seed, &["push", "-q", "origin", "main"]);
        let clone = |name: &str| {
            let dir = root.join(name);
            git(
                root,
                &[
                    "clone",
                    "-q",
                    remote.to_str().unwrap(),
                    dir.to_str().unwrap(),
                ],
            );
            dir
        };
        let (a, b) = (clone("a"), clone("b"));
        (remote, a, b)
    }

    #[test]
    fn save_members_starts_from_the_remote_and_leaves_nothing_behind() {
        let root = tempfile::tempdir().unwrap();
        let (remote, a, b) = remote_with_clones(
            root.path(),
            "[profiles]\n\"npm:a\" = [\"dev\"]\n\"npm:b\" = [\"dev\"]\n",
        );

        // b changes both entries and pushes; a is now behind the remote. a's edit applies
        // to the members the remote has, so b's change to the same entry stays
        save_members(&b, "npm:b", &share(&["server"]), listed("npm:b")).unwrap();
        save_members(&b, "npm:a", &share(&["mini"]), listed("npm:a")).unwrap();
        let saved = save_members(&a, "npm:a", &share(&["server"]), listed("npm:a")).unwrap();
        assert_eq!(saved, Some(set(&["dev", "mini", "server"])));
        let table = read_entries(&a);
        assert_eq!(table["npm:a"], vec!["dev", "mini", "server"]);
        assert_eq!(table["npm:b"], vec!["dev", "server"]);
        let repo = GitBackend::open(&a).unwrap();
        assert!(!repo.has_changes().unwrap() && !repo.has_unpushed_commits());

        // A save never commits changes it did not make
        std::fs::write(a.join("stray"), "x").unwrap();
        assert!(save_members(&a, "npm:a", &leave("dev"), listed("npm:a")).is_err());
        std::fs::remove_file(a.join("stray")).unwrap();

        // A rejected push leaves the clone as the remote has it
        let hook = remote.join("hooks/pre-receive");
        let reject_pushes = || {
            std::fs::write(&hook, "#!/bin/sh\nexit 1\n").unwrap();
            std::fs::set_permissions(&hook, std::os::unix::fs::PermissionsExt::from_mode(0o755))
                .unwrap();
        };
        reject_pushes();
        assert!(save_members(&a, "npm:a", &leave("dev"), listed("npm:a")).is_err());
        assert_eq!(read_entries(&a)["npm:a"], vec!["dev", "mini", "server"]);
        assert!(!repo.has_changes().unwrap() && !repo.has_unpushed_commits());

        // Also when the file did not exist before the save
        std::fs::remove_file(&hook).unwrap();
        git(&a, &["rm", "-q", FILE]);
        GitBackend::new(a.clone()).commit("drop", "test").unwrap();
        git(&a, &["push", "-q", "origin", "main"]);
        reject_pushes();
        assert!(save_members(&a, "npm:a", &share(&["dev"]), listed("npm:a")).is_err());
        assert!(!a.join(FILE).exists());
        assert!(!repo.has_changes().unwrap() && !repo.has_unpushed_commits());
    }
}

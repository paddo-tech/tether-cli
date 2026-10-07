use crate::cli::output::Colorize;
use crate::cli::{Output, Prompt};
use crate::config::Config;
use crate::packages::inbox;
use crate::sync::signing;
use crate::sync::state::valid_machine_id;
use crate::sync::{GitBackend, MachineState, SyncEngine, SyncState};
use anyhow::Result;
use chrono::Local;
use comfy_table::{Attribute, Cell, Color};
use std::path::Path;

/// The machine id `name` refers to: a machine id, or the hostname of exactly one machine.
/// A name that matches neither is returned as given, so a command can act on an id that
/// has no record, such as untrust.
pub fn resolve(sync_path: &Path, name: &str) -> Result<String> {
    resolve_in(&MachineState::list_all(sync_path).unwrap_or_default(), name)
}

fn resolve_in(machines: &[MachineState], name: &str) -> Result<String> {
    if machines.iter().any(|m| m.machine_id == name) {
        return Ok(name.to_string());
    }
    let host = |h: &str| h.trim_end_matches(".local").to_ascii_lowercase();
    let ids: Vec<&str> = machines
        .iter()
        .filter(|m| host(&m.hostname) == host(name))
        .map(|m| m.machine_id.as_str())
        .collect();
    match ids.as_slice() {
        [] => Ok(name.to_string()),
        [id] => Ok(id.to_string()),
        _ => anyhow::bail!(
            "Hostname {} names more than one machine: {}. Use a machine id",
            name,
            ids.join(", ")
        ),
    }
}

pub async fn list(json: bool) -> Result<()> {
    let config = Config::load()?;
    if !config.has_personal_features() {
        anyhow::bail!("Machine management is not available in team-only mode");
    }

    let sync_path = SyncEngine::sync_path()?;
    let machines = MachineState::list_all(&sync_path)?;

    let state = SyncState::load()?;
    let current_machine = &state.machine_id;
    let statuses = signing::record_statuses(&sync_path, current_machine)?;
    if json {
        return Output::json(&list_json(&config, &machines, &statuses, current_machine));
    }
    if machines.is_empty() {
        Output::info("No machines synced yet");
        return Ok(());
    }
    let old_ids = signing::old_ids_of_this_machine(&sync_path, &machines, current_machine);
    let old_builds = signing::old_builds(&machines, current_machine);

    println!();
    println!("{}", "Synced Machines".bright_cyan().bold());
    println!();

    let mut table = Output::table_full();
    table.set_header(vec![
        Cell::new("Machine")
            .add_attribute(Attribute::Bold)
            .fg(Color::Cyan),
        Cell::new("Profile")
            .add_attribute(Attribute::Bold)
            .fg(Color::Cyan),
        Cell::new("Hostname")
            .add_attribute(Attribute::Bold)
            .fg(Color::Cyan),
        Cell::new("Version")
            .add_attribute(Attribute::Bold)
            .fg(Color::Cyan),
        Cell::new("Last Sync")
            .add_attribute(Attribute::Bold)
            .fg(Color::Cyan),
        Cell::new("Signing Key")
            .add_attribute(Attribute::Bold)
            .fg(Color::Cyan),
        Cell::new("").add_attribute(Attribute::Bold).fg(Color::Cyan),
    ]);

    for machine in &machines {
        let is_current = &machine.machine_id == current_machine;
        let marker = if is_current {
            Cell::new("(this machine)").fg(Color::Green)
        } else if old_ids.iter().any(|o| o.machine_id == machine.machine_id) {
            Cell::new("(may be an old id of this machine)").fg(Color::Yellow)
        } else if old_builds.contains(&machine.machine_id) {
            Cell::new("(on 1.x)").fg(Color::Yellow)
        } else {
            Cell::new("")
        };
        let local_time = machine.last_sync.with_timezone(&Local);

        let version = if machine.cli_version.is_empty() {
            "-".to_string()
        } else {
            machine.cli_version.clone()
        };

        let profile = machine
            .profile
            .as_deref()
            .unwrap_or(config.profile_name(&machine.machine_id));

        let (text, color) = statuses
            .iter()
            .find(|(id, _, _)| id == &machine.machine_id)
            .map_or(("-".to_string(), Color::Reset), |(_, status, fp)| {
                key_label(*status, fp.as_deref())
            });
        let key_cell = Cell::new(text).fg(color);

        table.add_row(vec![
            if is_current {
                Cell::new(&machine.machine_id).fg(Color::Green)
            } else {
                Cell::new(&machine.machine_id)
            },
            Cell::new(profile),
            Cell::new(&machine.hostname),
            Cell::new(version),
            Cell::new(local_time.format("%Y-%m-%d %H:%M:%S").to_string()),
            key_cell,
            marker,
        ]);
    }

    println!("{table}");
    println!();
    if !old_ids.is_empty() {
        print_old_id_hints(&old_ids);
        println!();
    }
    if !old_builds.is_empty() {
        print_old_build_notes(&old_builds);
        println!();
    }

    Ok(())
}

/// `machines list --json`. `record` is how this machine reads the record: trusted,
/// untrusted, replayed or signature_failed. `signer` is the fingerprint whose signature
/// verifies, or null.
fn list_json(
    config: &Config,
    machines: &[MachineState],
    statuses: &[(String, signing::RecordStatus, Option<String>)],
    this: &str,
) -> serde_json::Value {
    use signing::RecordStatus;
    serde_json::Value::Array(
        machines
            .iter()
            .map(|m| {
                let (status, signer) = statuses
                    .iter()
                    .find(|(id, _, _)| *id == m.machine_id)
                    .map_or((RecordStatus::Untrusted, None), |(_, s, fp)| {
                        (*s, fp.clone())
                    });
                serde_json::json!({
                    "id": m.machine_id,
                    "hostname": m.hostname,
                    "profile": m.profile.as_deref().unwrap_or(config.profile_name(&m.machine_id)),
                    "version": m.cli_version,
                    "os": m.os,
                    "last_sync": m.last_sync,
                    "this_machine": m.machine_id == this,
                    "record": match status {
                        RecordStatus::Trusted => "trusted",
                        RecordStatus::Untrusted => "untrusted",
                        RecordStatus::Replayed => "replayed",
                        RecordStatus::SignatureFailed => "signature_failed",
                    },
                    "signer": signer,
                })
            })
            .collect(),
    )
}

/// One line per machine on 1.x, which a rolling upgrade leaves behind for a while.
pub fn print_old_build_notes(ids: &[String]) {
    for id in ids {
        Output::warning(&format!("{} is {}", id, signing::OLD_BUILD_NOTE));
    }
}

/// One machine's record, its key and how this machine trusts it.
pub async fn show(name: Option<&str>) -> Result<()> {
    let config = Config::load()?;
    if !config.has_personal_features() {
        anyhow::bail!("Machine management is not available in team-only mode");
    }
    let sync_path = SyncEngine::sync_path()?;
    let this = SyncState::load()?.machine_id;
    let name = name.unwrap_or(&this);
    let id = resolve(&sync_path, name)?;
    let machines = MachineState::list_all(&sync_path)?;
    let Some(record) = machines.iter().find(|m| m.machine_id == id) else {
        anyhow::bail!(
            "Machine '{}' not found. Run 'tether machines list' to see the machines",
            name
        );
    };
    let (status, signer) = signing::record_statuses(&sync_path, &this)?
        .into_iter()
        .find(|(i, _, _)| *i == id)
        .map_or((signing::RecordStatus::Untrusted, None), |(_, s, fp)| {
            (s, fp)
        });
    let trusted = signing::TrustStore::load()?
        .key_for(&id)
        .map(signing::fingerprint);
    let packages: usize = record.packages.values().map(Vec::len).sum();

    Output::section(&format!("Machine {}", id));
    println!();
    Output::key_value(
        "Machine",
        &if id == this {
            format!("{} (this machine)", id)
        } else {
            id.clone()
        },
    );
    Output::key_value("Hostname", &record.hostname);
    Output::key_value(
        "Profile",
        record
            .profile
            .as_deref()
            .unwrap_or(config.profile_name(&id)),
    );
    Output::key_value(
        "Version",
        if record.cli_version.is_empty() {
            "-"
        } else {
            &record.cli_version
        },
    );
    Output::key_value("OS", &format!("{} {}", record.os, record.os_version));
    Output::key_value(
        "Last sync",
        &record
            .last_sync
            .with_timezone(&Local)
            .format("%Y-%m-%d %H:%M:%S")
            .to_string(),
    );
    Output::key_value("Record", status.label());
    Output::key_value(
        "Signed by",
        signer.as_deref().unwrap_or("no valid signature"),
    );
    let trust = match (&trusted, &signer) {
        _ if id == this => "this machine".to_string(),
        (Some(t), Some(s)) if t == s => "trusted".to_string(),
        (Some(t), _) => format!("trusted key is {}, which did not sign this record", t),
        (None, _) => "not trusted".to_string(),
    };
    Output::key_value("Trust", &trust);
    Output::key_value(
        "Packages",
        &format!(
            "{} listed, {} removed",
            packages,
            record
                .removed_packages
                .values()
                .map(Vec::len)
                .sum::<usize>()
        ),
    );
    Output::key_value("Dotfiles", &record.dotfiles.len().to_string());
    println!();
    if id == this {
        Output::dim(
            "Compare the 'Signed by' fingerprint with the one other machines show for this machine",
        );
    } else if let (None, Some(fp)) = (&trusted, &signer) {
        Output::dim(&format!(
            "After you compare the fingerprint on that machine: tether machines trust {} --fingerprint {}",
            id, fp
        ));
    }
    Ok(())
}

/// The signing key column: the fingerprint, if a signature verifies, and how a sync reads
/// the record.
fn key_label(status: signing::RecordStatus, fingerprint: Option<&str>) -> (String, Color) {
    use signing::RecordStatus;
    let color = match status {
        RecordStatus::Trusted => Color::Green,
        RecordStatus::Untrusted => Color::Yellow,
        RecordStatus::Replayed | RecordStatus::SignatureFailed => Color::Red,
    };
    let text = match (fingerprint, status) {
        (None, RecordStatus::Untrusted) => "-".to_string(),
        (None, _) => status.label().to_string(),
        (Some(fp), RecordStatus::Trusted | RecordStatus::Untrusted) => {
            format!("{} ({})", fp, status.label())
        }
        (Some(fp), _) => format!("{} {}", fp, status.label()),
    };
    (text, color)
}

/// One line per record that looks like an earlier id of this machine, with the command
/// that removes it.
pub fn print_old_id_hints(old_ids: &[signing::OldId]) {
    for id in old_ids.iter().map(|o| &o.machine_id) {
        Output::info(&format!(
            "{} may be an old id of this machine, a guess from its hostname and age. \
             If no other machine uses this hostname, remove it: tether machines remove {}",
            id, id
        ));
    }
}

pub async fn profile_set(profile: &str) -> Result<()> {
    let mut config = Config::load()?;

    if !Config::is_safe_profile_name(profile) {
        anyhow::bail!("Invalid profile name: '{}'", profile);
    }

    if !config.profiles.contains_key(profile) {
        anyhow::bail!(
            "Profile '{}' not found. Available profiles: {}",
            profile,
            if config.profiles.is_empty() {
                "(none)".to_string()
            } else {
                config
                    .profiles
                    .keys()
                    .cloned()
                    .collect::<Vec<_>>()
                    .join(", ")
            }
        );
    }

    let state = SyncState::load()?;
    config
        .machine_profiles
        .insert(state.machine_id.clone(), profile.to_string());
    config.save()?;

    Output::success(&format!(
        "Assigned profile '{}' to this machine ({})",
        profile, state.machine_id
    ));
    Ok(())
}

pub async fn profile_unset() -> Result<()> {
    let mut config = Config::load()?;
    let state = SyncState::load()?;

    if config.machine_profiles.remove(&state.machine_id).is_some() {
        config.save()?;
        Output::success(&format!(
            "Removed profile from this machine ({})",
            state.machine_id
        ));
    } else {
        Output::info("No profile assigned to this machine");
    }

    Ok(())
}

/// Rename this machine. Only a machine can sign its own record, so another machine's record
/// is never moved: it would no longer verify under the new id.
/// `old` is the deprecated form's first name, which must be this machine.
pub async fn rename(old: Option<&str>, new: &str) -> Result<()> {
    let mut config = Config::load()?;
    if !config.has_personal_features() {
        anyhow::bail!("Machine management is not available in team-only mode");
    }

    let sync_path = SyncEngine::sync_path()?;
    // No other writer may save this machine's record between its read and the save
    let _sync_lock = crate::sync::acquire_sync_lock(true)?;
    let mut state = SyncState::load()?;
    let current = state.machine_id.clone();
    let old = old.unwrap_or(&current);
    if state.machine_id != old {
        anyhow::bail!(
            "Only machine '{}' can rename itself. Run 'tether machines rename' on that machine",
            old
        );
    }
    if !valid_machine_id(new) {
        anyhow::bail!("Machine names use letters, digits, '.', '_' and '-' only");
    }
    let machines_dir = sync_path.join("machines");

    let old_file = machines_dir.join(format!("{}.json", old));
    let new_file = machines_dir.join(format!("{}.json", new));

    if !old_file.exists() {
        anyhow::bail!("Machine '{}' not found", old);
    }

    if new_file.exists() {
        anyhow::bail!("Machine '{}' already exists", new);
    }

    signing::rename_own_record(&sync_path, old, new)?;
    state.machine_id = new.to_string();
    state.save()?;

    // Migrate profile assignment if one exists
    if let Some(profile) = config.machine_profiles.remove(old) {
        config.machine_profiles.insert(new.to_string(), profile);
        config.save()?;
    }

    // Commit and push
    let git = GitBackend::open(&sync_path)?;
    git.commit(
        &format!("Rename machine {} to {}", old, new),
        &crate::sync::local_hostname(),
    )?;
    git.push()?;

    Output::success(&format!("Renamed machine '{}' to '{}'", old, new));
    Output::info(&format!(
        "Other machines trust this key only as '{}'. On each of them, run 'tether machines trust {} --fingerprint {}'",
        old,
        new,
        signing::fingerprint(signing::load_or_create(new)?.public_key())
    ));
    Ok(())
}

pub async fn remove(name: &str, yes: bool) -> Result<()> {
    let config = Config::load()?;
    if !config.has_personal_features() {
        anyhow::bail!("Machine management is not available in team-only mode");
    }
    let resolved = resolve(&SyncEngine::sync_path()?, name)?;
    let name = resolved.as_str();

    if !valid_machine_id(name) {
        anyhow::bail!("Invalid machine id '{}'", name);
    }

    let state = SyncState::load()?;

    if state.machine_id == name {
        anyhow::bail!(
            "Cannot remove the current machine. Run this command on another machine to remove this one"
        );
    }

    let sync_path = SyncEngine::sync_path()?;
    // A sync may replace the record while the question is open, so only the record shown
    // is removed
    let digest = record_digest(&sync_path, name)?;

    let record = MachineState::list_all(&sync_path)?
        .into_iter()
        .find(|m| m.machine_id == name);
    if !yes && !Prompt::confirm(&format!("Remove machine '{}'?", name), false)? {
        return Ok(());
    }

    let _sync_lock = crate::sync::acquire_sync_lock(true)?;
    let untrusted = remove_record(name, &digest)?;
    GitBackend::open(&sync_path)?.push()?;

    Output::success(&format!("Removed machine '{}'", name));
    if let Some(record) = record {
        Output::info(&removed_summary(&record));
    }
    if untrusted {
        Output::info(&format!(
            "This machine no longer trusts the key of '{}'. Other machines still trust it",
            name
        ));
    }
    Ok(())
}

/// What a removal deleted, so a removal that `-y` did not ask about is on record.
fn removed_summary(record: &MachineState) -> String {
    format!(
        "Removed record machines/{}.json: hostname {}, last sync {}, {} packages",
        record.machine_id,
        record.hostname,
        record
            .last_sync
            .with_timezone(&Local)
            .format("%Y-%m-%d %H:%M:%S"),
        record.packages.values().map(Vec::len).sum::<usize>()
    )
}

/// Remove a record the dashboard showed as an old id of this machine, only while it still is
/// one with the same bytes. A pull may have replaced it with another machine's record since.
/// The caller holds the sync lock and pushes.
pub fn remove_old_record(machine_id: &str, digest: &str) -> Result<bool> {
    let sync_path = SyncEngine::sync_path()?;
    let machines = MachineState::list_all(&sync_path)?;
    let this_id = SyncState::load()?.machine_id;
    if !signing::old_ids_of_this_machine(&sync_path, &machines, &this_id)
        .iter()
        .any(|o| o.machine_id == machine_id && o.digest == digest)
    {
        anyhow::bail!(
            "machines/{}.json changed since you confirmed. Review it again",
            machine_id
        );
    }
    remove_record(machine_id, digest)
}

/// Remove another machine's record, its signature and its profile assignment, commit, and
/// untrust its key on this machine, when the record still has the SHA-256 `digest` the user
/// confirmed. Returns true when this machine trusted the key. The caller holds the sync lock
/// and pushes.
pub fn remove_record(name: &str, digest: &str) -> Result<bool> {
    // The id names files to delete, so a path such as `../../state` never reaches a join
    if !valid_machine_id(name) {
        anyhow::bail!("Invalid machine id '{}'", name);
    }
    if SyncState::load()?.machine_id == name {
        anyhow::bail!("Cannot remove the current machine");
    }
    let sync_path = SyncEngine::sync_path()?;
    if record_digest(&sync_path, name)? != digest {
        anyhow::bail!(
            "machines/{}.json changed since you confirmed. Review it again",
            name
        );
    }
    let paths = record_paths(&sync_path, name)?;
    GitBackend::open(&sync_path)?.remove_and_commit(
        &paths,
        &format!("Remove machine {}", name),
        &crate::sync::local_hostname(),
    )?;

    let mut config = Config::load()?;
    if config.machine_profiles.remove(name).is_some() {
        config.save()?;
    }
    // A later record under this id with the same key must wait for approval again
    inbox::untrust_machine(name)
}

/// The record, its signature, and the public key file earlier builds published, relative to
/// the sync repo. Anyone who can push can commit a symlink, so a symlink or a path outside
/// the repo is refused rather than removed or restored. A file name must match `name`
/// exactly: on a case-insensitive file system `MAC` would open `mac.json`, while the trust
/// store and config know only `mac`.
fn record_paths(sync_path: &Path, name: &str) -> Result<Vec<String>> {
    let root = sync_path.canonicalize()?;
    let listed: Vec<String> = match std::fs::read_dir(sync_path.join("machines")) {
        Ok(entries) => entries
            .filter_map(|e| e.ok()?.file_name().into_string().ok())
            .collect(),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Vec::new(),
        Err(e) => return Err(e.into()),
    };
    let record = format!("{}.json", name);
    if !listed.contains(&record) {
        if let Some(other) = listed.iter().find(|f| f.eq_ignore_ascii_case(&record)) {
            anyhow::bail!(
                "Machine '{}' not found. Did you mean '{}'? Machine ids are case-sensitive",
                name,
                other.trim_end_matches(".json")
            );
        }
        anyhow::bail!("Machine '{}' not found", name);
    }
    let mut paths = Vec::new();
    for ext in ["json", "json.sig", "pub"] {
        let file = format!("{}.{}", name, ext);
        if !listed.contains(&file) {
            continue;
        }
        let path = format!("machines/{}", file);
        let full = sync_path.join(&path);
        let meta = std::fs::symlink_metadata(&full)?;
        if meta.file_type().is_symlink() {
            anyhow::bail!("{} is a symlink. Tether will not remove it", path);
        }
        if !full.canonicalize()?.starts_with(&root) {
            anyhow::bail!("{} resolves outside the sync repo", path);
        }
        paths.push(path);
    }
    Ok(paths)
}

/// SHA-256 of `name`'s record, after the checks of [`record_paths`].
fn record_digest(sync_path: &Path, name: &str) -> Result<String> {
    record_paths(sync_path, name)?;
    let bytes = std::fs::read(sync_path.join("machines").join(format!("{}.json", name)))?;
    Ok(crate::sha256_hex(&bytes))
}

/// Trust only a key whose fingerprint the user gave or was shown and accepted.
pub async fn trust(name: &str, fingerprint: Option<&str>) -> Result<()> {
    let config = Config::load()?;
    if !config.has_personal_features() {
        anyhow::bail!("Machine management is not available in team-only mode");
    }
    let sync_path = SyncEngine::sync_path()?;
    let resolved = resolve(&sync_path, name)?;
    let name = resolved.as_str();
    let fingerprint = match fingerprint {
        Some(fingerprint) => fingerprint.to_string(),
        None => {
            let Some(current) = inbox::signing_fingerprint(&sync_path, name) else {
                anyhow::bail!("Machine {} has no signed machine record", name);
            };
            if !Prompt::is_interactive() || Prompt::assume_yes() {
                anyhow::bail!(
                    "Run 'tether machines show {}' on that machine and compare the key, then run \
                     'tether machines trust {} --fingerprint {}'",
                    name,
                    name,
                    current
                );
            }
            Output::info(&format!("Machine {} signs with key {}", name, current));
            if !Prompt::confirm(
                "Trust this key? Compare it with 'tether machines show' on that machine",
                false,
            )? {
                return Ok(());
            }
            current
        }
    };
    let trusted = inbox::trust_machine(&sync_path, name, &fingerprint)?;
    Output::success(&format!(
        "Trusted machine {} with key {}",
        trusted.machine_id, trusted.fingerprint
    ));
    Ok(())
}

pub async fn untrust(name: &str) -> Result<()> {
    // A trusted id with no record, such as the old id of a renamed machine, is never
    // read as another machine's hostname
    let resolved = if signing::TrustStore::load()?.key_for(name).is_some() {
        name.to_string()
    } else {
        resolve(&SyncEngine::sync_path()?, name)?
    };
    let name = resolved.as_str();
    if SyncState::load()?.machine_id == name {
        anyhow::bail!("Cannot untrust the current machine");
    }
    if inbox::untrust_machine(name)? {
        Output::success(&format!("Machine {} is no longer trusted", name));
    } else {
        Output::info(&format!("Machine {} was not trusted", name));
    }
    Ok(())
}

const PROFILE_MANAGERS: [&str; 6] = ["brew", "npm", "pnpm", "bun", "gem", "uv"];

/// Create a profile. `from` copies an existing profile without asking. `managers` sets the
/// package managers instead of asking. With `-y`, the wizard takes every default answer.
pub async fn profile_create(
    name: &str,
    from: Option<&str>,
    managers: Option<&[String]>,
) -> Result<()> {
    let mut config = Config::load()?;

    if !Config::is_safe_profile_name(name) {
        anyhow::bail!("Invalid profile name: '{}'", name);
    }

    if config.profiles.contains_key(name) {
        anyhow::bail!("Profile '{}' already exists", name);
    }
    if let Some(unknown) = managers
        .unwrap_or_default()
        .iter()
        .find(|m| !PROFILE_MANAGERS.contains(&m.as_str()))
    {
        anyhow::bail!(
            "Unknown package manager '{}'. Managers: {}",
            unknown,
            PROFILE_MANAGERS.join(", ")
        );
    }

    if let Some(from) = from {
        let Some(mut profile) = config.profiles.get(from).cloned() else {
            anyhow::bail!("Profile '{}' not found", from);
        };
        if let Some(managers) = managers {
            profile.packages = managers.to_vec();
        }
        config.profiles.insert(name.to_string(), profile);
        config.save()?;
        Output::success(&format!("Created profile '{}' from '{}'", name, from));
        Output::info(&format!(
            "Assign it on a machine: tether machines profile set {}",
            name
        ));
        return Ok(());
    }

    // Gather all known dotfiles from all existing profiles
    let mut all_dotfiles: Vec<String> = Vec::new();
    for profile in config.profiles.values() {
        for entry in &profile.dotfiles {
            let path = entry.path().to_string();
            if !all_dotfiles.contains(&path) {
                all_dotfiles.push(path);
            }
        }
    }
    // Also include global dotfiles
    for entry in &config.dotfiles.files {
        let path = entry.path().to_string();
        if !all_dotfiles.contains(&path) {
            all_dotfiles.push(path);
        }
    }
    all_dotfiles.sort();

    // Select dotfiles
    let dotfile_options: Vec<&str> = all_dotfiles.iter().map(|s| s.as_str()).collect();
    let defaults: Vec<usize> = (0..all_dotfiles.len()).collect();
    let selected_dotfiles = if all_dotfiles.is_empty() {
        vec![]
    } else {
        Prompt::multi_select(
            "Select dotfiles for this profile",
            dotfile_options,
            &defaults,
        )?
    };

    // For each selected dotfile, ask shared or profile-specific
    let mut profile_dotfiles = Vec::new();
    for idx in &selected_dotfiles {
        let path = &all_dotfiles[*idx];
        // Common files default to shared
        let default_shared = path == ".gitconfig" || path == ".gitignore_global";
        let shared = Prompt::question(&format!("Share {} across profiles?", path), default_shared)?;
        profile_dotfiles.push(crate::config::ProfileDotfileEntry::WithOptions {
            path: path.clone(),
            shared,
            create_if_missing: false,
            on_conflict: Default::default(),
        });
    }

    // Select dirs
    let mut all_dirs: Vec<String> = Vec::new();
    for profile in config.profiles.values() {
        for dir in &profile.dirs {
            if !all_dirs.contains(dir) {
                all_dirs.push(dir.clone());
            }
        }
    }
    for dir in &config.dotfiles.dirs {
        if !all_dirs.contains(dir) {
            all_dirs.push(dir.clone());
        }
    }
    all_dirs.sort();

    let selected_dirs = if all_dirs.is_empty() {
        vec![]
    } else {
        let dir_options: Vec<&str> = all_dirs.iter().map(|s| s.as_str()).collect();
        let dir_defaults: Vec<usize> = (0..all_dirs.len()).collect();
        Prompt::multi_select("Select directories", dir_options, &dir_defaults)?
    };
    let dirs: Vec<String> = selected_dirs.iter().map(|i| all_dirs[*i].clone()).collect();

    let packages: Vec<String> = match managers {
        Some(managers) => managers.to_vec(),
        None => {
            let mgr_defaults: Vec<usize> = (0..PROFILE_MANAGERS.len()).collect();
            Prompt::multi_select(
                "Select package managers",
                PROFILE_MANAGERS.to_vec(),
                &mgr_defaults,
            )?
            .iter()
            .map(|i| PROFILE_MANAGERS[*i].to_string())
            .collect()
        }
    };

    let profile = crate::config::ProfileConfig {
        dotfiles: profile_dotfiles,
        dirs,
        packages,
    };

    config.profiles.insert(name.to_string(), profile);
    config.save()?;

    Output::success(&format!("Created profile '{}'", name));
    Output::info(&format!(
        "Assign it on a machine: tether machines profile set {}",
        name
    ));
    Ok(())
}

pub async fn profile_edit(name: &str) -> Result<()> {
    let mut config = Config::load()?;

    let profile = match config.profiles.get(name) {
        Some(p) => p.clone(),
        None => {
            anyhow::bail!("Profile '{}' not found", name);
        }
    };

    // Show current dotfiles and let user toggle
    let current_paths: Vec<String> = profile
        .dotfiles
        .iter()
        .map(|e| e.path().to_string())
        .collect();

    // Gather all known dotfiles
    let mut all_dotfiles: Vec<String> = current_paths.clone();
    for p in config.profiles.values() {
        for entry in &p.dotfiles {
            let path = entry.path().to_string();
            if !all_dotfiles.contains(&path) {
                all_dotfiles.push(path);
            }
        }
    }
    for entry in &config.dotfiles.files {
        let path = entry.path().to_string();
        if !all_dotfiles.contains(&path) {
            all_dotfiles.push(path);
        }
    }
    all_dotfiles.sort();

    let dotfile_options: Vec<&str> = all_dotfiles.iter().map(|s| s.as_str()).collect();
    let defaults: Vec<usize> = all_dotfiles
        .iter()
        .enumerate()
        .filter(|(_, p)| current_paths.contains(p))
        .map(|(i, _)| i)
        .collect();

    let selected = if all_dotfiles.is_empty() {
        vec![]
    } else {
        Prompt::multi_select("Select dotfiles", dotfile_options, &defaults)?
    };

    let mut new_dotfiles = Vec::new();
    for idx in &selected {
        let path = &all_dotfiles[*idx];
        let existing = profile.dotfiles.iter().find(|e| e.path() == path);
        let existing_shared = existing.map(|e| e.shared()).unwrap_or(false);
        let on_conflict = existing.map(|e| e.on_conflict()).unwrap_or_default();
        let default_shared = existing_shared || path == ".gitconfig" || path == ".gitignore_global";
        let shared = Prompt::question(&format!("Share {} across profiles?", path), default_shared)?;
        new_dotfiles.push(crate::config::ProfileDotfileEntry::WithOptions {
            path: path.clone(),
            shared,
            create_if_missing: false,
            on_conflict,
        });
    }

    // Package managers
    let mgr_defaults: Vec<usize> = PROFILE_MANAGERS
        .iter()
        .enumerate()
        .filter(|(_, m)| profile.packages.is_empty() || profile.packages.contains(&m.to_string()))
        .map(|(i, _)| i)
        .collect();
    let selected_managers = Prompt::multi_select(
        "Select package managers",
        PROFILE_MANAGERS.to_vec(),
        &mgr_defaults,
    )?;
    let packages: Vec<String> = selected_managers
        .iter()
        .map(|i| PROFILE_MANAGERS[*i].to_string())
        .collect();

    let updated = crate::config::ProfileConfig {
        dotfiles: new_dotfiles,
        dirs: profile.dirs.clone(),
        packages,
    };

    config.profiles.insert(name.to_string(), updated);
    config.save()?;

    Output::success(&format!("Updated profile '{}'", name));
    Ok(())
}

pub async fn profile_list() -> Result<()> {
    let config = Config::load()?;

    if config.profiles.is_empty() {
        Output::info("No profiles defined");
        return Ok(());
    }

    println!();
    Output::section("Profiles");
    println!();

    let mut table = Output::table_full();
    table.set_header(vec![
        Cell::new("Profile")
            .add_attribute(Attribute::Bold)
            .fg(Color::Cyan),
        Cell::new("Dotfiles")
            .add_attribute(Attribute::Bold)
            .fg(Color::Cyan),
        Cell::new("Dirs")
            .add_attribute(Attribute::Bold)
            .fg(Color::Cyan),
        Cell::new("Packages")
            .add_attribute(Attribute::Bold)
            .fg(Color::Cyan),
        Cell::new("Machines")
            .add_attribute(Attribute::Bold)
            .fg(Color::Cyan),
    ]);

    let mut profile_names: Vec<_> = config.profiles.keys().collect();
    profile_names.sort();

    for name in profile_names {
        let profile = &config.profiles[name];
        let machines: Vec<&str> = config
            .machine_profiles
            .iter()
            .filter(|(_, v)| v.as_str() == name.as_str())
            .map(|(k, _)| k.as_str())
            .collect();

        let packages_display = if profile.packages.is_empty() {
            "all".to_string()
        } else {
            profile.packages.join(", ")
        };

        table.add_row(vec![
            Cell::new(name),
            Cell::new(profile.dotfiles.len().to_string()),
            Cell::new(profile.dirs.len().to_string()),
            Cell::new(packages_display),
            Cell::new(if machines.is_empty() {
                "-".to_string()
            } else {
                machines.join(", ")
            }),
        ]);
    }

    println!("{table}");
    println!();

    Ok(())
}

#[cfg(test)]
mod tests {
    #[test]
    fn a_machine_is_named_by_id_or_a_unique_hostname() {
        let machine = |id: &str, host: &str| {
            let mut m = crate::sync::MachineState::new(id);
            m.hostname = host.to_string();
            m
        };
        let fleet = [
            machine("a1", "laptop.local"),
            machine("b2", "server"),
            machine("c3", "twin"),
            machine("d4", "twin"),
        ];
        let resolve = |name: &str| super::resolve_in(&fleet, name);
        assert_eq!(resolve("b2").unwrap(), "b2");
        assert_eq!(resolve("laptop").unwrap(), "a1");
        assert_eq!(resolve("Laptop.local").unwrap(), "a1");
        assert_eq!(resolve("server").unwrap(), "b2");
        assert_eq!(resolve("gone").unwrap(), "gone");
        assert!(resolve("twin").unwrap_err().to_string().contains("c3, d4"));
    }

    #[test]
    fn removal_summary_names_the_record_it_removed() {
        let mut record = crate::sync::MachineState::new("old-mac");
        record.hostname = "mac".to_string();
        record
            .packages
            .insert("npm".to_string(), vec!["a".to_string(), "b".to_string()]);
        let summary = super::removed_summary(&record);
        assert!(summary.starts_with("Removed record machines/old-mac.json: hostname mac"));
        assert!(summary.ends_with(", 2 packages"));
    }

    #[test]
    fn key_column_shows_records_a_sync_ignores() {
        use crate::sync::signing::RecordStatus;
        use comfy_table::Color;

        let fp = Some("SHA256:abc");
        assert_eq!(
            super::key_label(RecordStatus::Trusted, fp),
            ("SHA256:abc (trusted)".to_string(), Color::Green)
        );
        assert_eq!(
            super::key_label(RecordStatus::Replayed, fp),
            ("SHA256:abc replayed (ignored)".to_string(), Color::Red)
        );
        assert_eq!(
            super::key_label(RecordStatus::SignatureFailed, None),
            ("signature failed (ignored)".to_string(), Color::Red)
        );
        assert_eq!(
            super::key_label(RecordStatus::Untrusted, None),
            ("-".to_string(), Color::Yellow)
        );
    }

    use super::*;

    #[test]
    fn remove_record_refuses_ids_that_leave_the_machines_dir() {
        for id in ["../../state", "../config", "a/b", ".git"] {
            let err = remove_record(id, "").unwrap_err().to_string();
            assert!(err.contains("Invalid machine id"), "{}: {}", id, err);
        }
    }

    #[test]
    fn record_paths_refuse_symlinks_and_paths_outside_the_repo() {
        let tmp = tempfile::TempDir::new().unwrap();
        let repo = tmp.path().join("sync");
        let machines = repo.join("machines");
        std::fs::create_dir_all(&machines).unwrap();
        std::fs::write(tmp.path().join("state.json"), "state").unwrap();
        std::fs::write(machines.join("a.json"), "{}").unwrap();
        std::fs::write(machines.join("a.json.sig"), "sig").unwrap();
        assert_eq!(
            record_paths(&repo, "a").unwrap(),
            vec!["machines/a.json", "machines/a.json.sig"]
        );

        std::os::unix::fs::symlink("../../state.json", machines.join("old.json")).unwrap();
        let err = record_paths(&repo, "old").unwrap_err().to_string();
        assert!(err.contains("symlink"), "{err}");
        assert!(record_paths(&repo, "gone")
            .unwrap_err()
            .to_string()
            .contains("not found"));
        let shown = record_digest(&repo, "a").unwrap();
        assert_eq!(shown, crate::sha256_hex(b"{}"));
        std::fs::write(machines.join("a.json"), "{\"generation\":2}").unwrap();
        assert_ne!(record_digest(&repo, "a").unwrap(), shown);
        // On a case-insensitive file system `A` opens `a.json`, but it is not that machine
        let err = record_paths(&repo, "A").unwrap_err().to_string();
        assert!(err.contains("Did you mean 'a'"), "{err}");

        // A machines directory that is itself a link leaves the repo
        let outside = tmp.path().join("outside");
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(outside.join("b.json"), "{}").unwrap();
        std::fs::remove_dir_all(&machines).unwrap();
        std::os::unix::fs::symlink(&outside, &machines).unwrap();
        let err = record_paths(&repo, "b").unwrap_err().to_string();
        assert!(err.contains("outside"), "{err}");
    }
}

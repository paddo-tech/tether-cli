use crate::cli::{Output, Prompt};
use crate::config::Config;
use crate::packages::inbox;
use crate::sync::signing;
use crate::sync::state::valid_machine_id;
use crate::sync::{GitBackend, MachineState, SyncEngine, SyncState};
use anyhow::Result;
use chrono::Local;
use comfy_table::{Attribute, Cell, Color};
use owo_colors::OwoColorize;

pub async fn list() -> Result<()> {
    let config = Config::load()?;
    if !config.has_personal_features() {
        Output::warning("Machine management not available in team-only mode");
        return Ok(());
    }

    let sync_path = SyncEngine::sync_path()?;
    let machines = MachineState::list_all(&sync_path)?;

    if machines.is_empty() {
        Output::info("No machines synced yet");
        return Ok(());
    }

    let state = SyncState::load()?;
    let current_machine = &state.machine_id;
    let signers = signing::record_signers(&sync_path);
    let trusted = inbox::trusted_machines()?;
    let old_ids = signing::old_ids_of_this_machine(&sync_path, &machines, current_machine);

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

        let key = signers
            .iter()
            .find(|(id, _)| id == &machine.machine_id)
            .map(|(_, key)| signing::fingerprint(key));
        let key_cell = match key {
            Some(fp)
                if trusted
                    .iter()
                    .any(|t| t.machine_id == machine.machine_id && t.fingerprint == fp) =>
            {
                Cell::new(format!("{} (trusted)", fp)).fg(Color::Green)
            }
            Some(fp) => Cell::new(format!("{} (untrusted)", fp)).fg(Color::Yellow),
            None => Cell::new("-"),
        };

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

    Ok(())
}

/// One line per record that looks like an earlier id of this machine, with the command
/// that removes it.
pub fn print_old_id_hints(old_ids: &[signing::OldId]) {
    for id in old_ids.iter().map(|o| &o.machine_id) {
        Output::info(&format!(
            "{} may be an old id of this machine, a guess from its hostname, age and build. \
             If no other machine uses this hostname, remove it: tether machines remove {}",
            id, id
        ));
    }
}

pub async fn profile_set(profile: &str) -> Result<()> {
    let mut config = Config::load()?;

    if !Config::is_safe_profile_name(profile) {
        Output::error(&format!("Invalid profile name: '{}'", profile));
        return Ok(());
    }

    if !config.profiles.contains_key(profile) {
        Output::error(&format!(
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
        ));
        return Ok(());
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
pub async fn rename(old: &str, new: &str) -> Result<()> {
    let mut config = Config::load()?;
    if !config.has_personal_features() {
        Output::warning("Machine management not available in team-only mode");
        return Ok(());
    }

    let sync_path = SyncEngine::sync_path()?;
    // No other writer may save this machine's record between its read and the save
    let _sync_lock = crate::sync::acquire_sync_lock(true)?;
    let mut state = SyncState::load()?;
    if state.machine_id != old {
        Output::error(&format!(
            "Only machine '{}' can rename itself. Run 'tether machines rename' on that machine",
            old
        ));
        return Ok(());
    }
    if !valid_machine_id(new) {
        Output::error("Machine names use letters, digits, '.', '_' and '-' only");
        return Ok(());
    }
    let machines_dir = sync_path.join("machines");

    let old_file = machines_dir.join(format!("{}.json", old));
    let new_file = machines_dir.join(format!("{}.json", new));

    if !old_file.exists() {
        Output::error(&format!("Machine '{}' not found", old));
        return Ok(());
    }

    if new_file.exists() {
        Output::error(&format!("Machine '{}' already exists", new));
        return Ok(());
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
        "Other machines trust this key only as '{}'. On each of them, run 'tether machines trust {} {}'",
        old,
        new,
        signing::fingerprint(signing::load_or_create(new)?.public_key())
    ));
    Ok(())
}

pub async fn remove(name: &str) -> Result<()> {
    let config = Config::load()?;
    if !config.has_personal_features() {
        Output::warning("Machine management not available in team-only mode");
        return Ok(());
    }

    if !valid_machine_id(name) {
        Output::error(&format!("Invalid machine id '{}'", name));
        return Ok(());
    }

    let state = SyncState::load()?;

    if state.machine_id == name {
        Output::error("Cannot remove the current machine");
        Output::info("Use this command from a different machine to remove this one");
        return Ok(());
    }

    let sync_path = SyncEngine::sync_path()?;
    if !sync_path
        .join("machines")
        .join(format!("{}.json", name))
        .exists()
    {
        Output::error(&format!("Machine '{}' not found", name));
        return Ok(());
    }

    if !Prompt::confirm(&format!("Remove machine '{}'?", name), false)? {
        return Ok(());
    }

    let _sync_lock = crate::sync::acquire_sync_lock(true)?;
    let untrusted = remove_record(name)?;
    GitBackend::open(&sync_path)?.push()?;

    Output::success(&format!("Removed machine '{}'", name));
    if untrusted {
        Output::info(&format!(
            "This machine no longer trusts the key of '{}'. Other machines still trust it",
            name
        ));
    }
    Ok(())
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
    remove_record(machine_id)
}

/// Remove another machine's record, its signature and its profile assignment, commit, and
/// untrust its key on this machine. Returns true when this machine trusted the key.
/// The caller holds the sync lock and pushes.
pub fn remove_record(name: &str) -> Result<bool> {
    // The id names files to delete, so a path such as `../../state` never reaches a join
    if !valid_machine_id(name) {
        anyhow::bail!("Invalid machine id '{}'", name);
    }
    if SyncState::load()?.machine_id == name {
        anyhow::bail!("Cannot remove the current machine");
    }
    let sync_path = SyncEngine::sync_path()?;
    let machine_file = sync_path.join("machines").join(format!("{}.json", name));
    if !machine_file.exists() {
        anyhow::bail!("Machine '{}' not found", name);
    }

    // The record, its signature, and the public key file earlier builds published
    let paths: Vec<String> = ["json", "json.sig", "pub"]
        .iter()
        .map(|ext| format!("machines/{}.{}", name, ext))
        .filter(|path| sync_path.join(path).exists())
        .collect();
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

/// Trust only a key whose fingerprint the user gave or was shown and accepted.
pub async fn trust(name: &str, fingerprint: Option<&str>) -> Result<()> {
    let config = Config::load()?;
    if !config.has_personal_features() {
        Output::warning("Machine management not available in team-only mode");
        return Ok(());
    }
    let sync_path = SyncEngine::sync_path()?;
    let fingerprint = match fingerprint {
        Some(fingerprint) => fingerprint.to_string(),
        None => {
            let Some(current) = inbox::signing_fingerprint(&sync_path, name) else {
                anyhow::bail!("Machine {} has no signed machine record", name);
            };
            if !std::io::IsTerminal::is_terminal(&std::io::stdin()) {
                anyhow::bail!(
                    "Check the key on {}, then run 'tether machines trust {} {}'",
                    name,
                    name,
                    current
                );
            }
            Output::info(&format!("Machine {} signs with key {}", name, current));
            if !crate::cli::Prompt::confirm(
                "Trust this key? Compare it with 'tether machines list' on that machine",
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
    if SyncState::load()?.machine_id == name {
        Output::error("Cannot untrust the current machine");
        return Ok(());
    }
    if inbox::untrust_machine(name)? {
        Output::success(&format!("Machine {} is no longer trusted", name));
    } else {
        Output::info(&format!("Machine {} was not trusted", name));
    }
    Ok(())
}

pub async fn profile_create(name: &str) -> Result<()> {
    let mut config = Config::load()?;

    if !Config::is_safe_profile_name(name) {
        Output::error(&format!("Invalid profile name: '{}'", name));
        return Ok(());
    }

    if config.profiles.contains_key(name) {
        Output::error(&format!("Profile '{}' already exists", name));
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
        let shared = Prompt::confirm(&format!("Share {} across profiles?", path), default_shared)?;
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

    // Select package managers
    let all_managers = ["brew", "npm", "pnpm", "bun", "gem", "uv"];
    let manager_options: Vec<&str> = all_managers.to_vec();
    let mgr_defaults: Vec<usize> = (0..all_managers.len()).collect();
    let selected_managers =
        Prompt::multi_select("Select package managers", manager_options, &mgr_defaults)?;
    let packages: Vec<String> = selected_managers
        .iter()
        .map(|i| all_managers[*i].to_string())
        .collect();

    let profile = crate::config::ProfileConfig {
        dotfiles: profile_dotfiles,
        dirs,
        packages,
    };

    config.profiles.insert(name.to_string(), profile);
    config.save()?;

    Output::success(&format!("Created profile '{}'", name));
    Output::info(&format!(
        "Assign to a machine: tether machines profile set {}",
        name
    ));
    Ok(())
}

pub async fn profile_edit(name: &str) -> Result<()> {
    let mut config = Config::load()?;

    let profile = match config.profiles.get(name) {
        Some(p) => p.clone(),
        None => {
            Output::error(&format!("Profile '{}' not found", name));
            return Ok(());
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
        let shared = Prompt::confirm(&format!("Share {} across profiles?", path), default_shared)?;
        new_dotfiles.push(crate::config::ProfileDotfileEntry::WithOptions {
            path: path.clone(),
            shared,
            create_if_missing: false,
            on_conflict,
        });
    }

    // Package managers
    let all_managers = ["brew", "npm", "pnpm", "bun", "gem", "uv"];
    let manager_options: Vec<&str> = all_managers.to_vec();
    let mgr_defaults: Vec<usize> = all_managers
        .iter()
        .enumerate()
        .filter(|(_, m)| profile.packages.is_empty() || profile.packages.contains(&m.to_string()))
        .map(|(i, _)| i)
        .collect();
    let selected_managers =
        Prompt::multi_select("Select package managers", manager_options, &mgr_defaults)?;
    let packages: Vec<String> = selected_managers
        .iter()
        .map(|i| all_managers[*i].to_string())
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
    use super::*;

    #[test]
    fn remove_record_refuses_ids_that_leave_the_machines_dir() {
        for id in ["../../state", "../config", "a/b", ".git"] {
            let err = remove_record(id).unwrap_err().to_string();
            assert!(err.contains("Invalid machine id"), "{}: {}", id, err);
        }
    }
}

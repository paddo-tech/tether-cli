use crate::cli::output::relative_time;
use crate::cli::output::Colorize;
use crate::cli::Output;
use crate::config::Config;
use crate::sync::{signing, ConflictState, MachineState, SyncEngine, SyncState};
use anyhow::Result;

pub async fn run(json: bool) -> Result<()> {
    let config = match Config::load() {
        Ok(c) => c,
        Err(e) if e.to_string().contains("Config version") => return Err(e),
        Err(_) => anyhow::bail!("Tether is not initialized. Run 'tether init' first."),
    };

    let state = SyncState::load()?;
    if json {
        return Output::json(&status_json(&config, &state)?);
    }

    Output::section("Tether Status");
    println!();

    // Machine
    Output::key_value("Machine", &state.machine_id);
    Output::key_value("Profile", config.profile_name(&state.machine_id));
    Output::key_value("Version", env!("CARGO_PKG_VERSION"));

    // Last Sync
    let sync_time = relative_time(state.last_sync);
    let sync_badge = Output::badge("synced", true);
    Output::key_value("Last Sync", &format!("{}  {}", sync_time, sync_badge));

    // Daemon status
    let pid = read_daemon_pid()?;
    let (status_label, is_running) = match pid {
        Some(pid) if is_process_running(pid) => (format!("Running (PID {pid})"), true),
        Some(pid) => (format!("Not running (stale PID {pid})"), false),
        None => ("Not running".to_string(), false),
    };
    let daemon_badge = Output::badge(if is_running { "active" } else { "stopped" }, is_running);
    Output::key_value("Daemon", &format!("{}  {}", status_label, daemon_badge));

    let inbox = crate::packages::inbox::list()?.len();
    Output::key_value(
        "Inbox",
        &match inbox {
            0 => "empty".to_string(),
            n => format!("{} waiting. Review with 'tether packages inbox'", n),
        },
    );
    let conflict_state = ConflictState::load().unwrap_or_default();
    Output::key_value(
        "Conflicts",
        &match conflict_state.conflicts.len() {
            0 => "none".to_string(),
            n => format!("{}. Resolve with 'tether resolve'", n),
        },
    );

    // Features summary
    let mut enabled_features = Vec::new();
    if config.features.personal_dotfiles {
        enabled_features.push("dotfiles");
    }
    if config.features.personal_packages {
        enabled_features.push("packages");
    }
    if config.features.team_dotfiles {
        enabled_features.push("team");
    }
    if config.features.collab_secrets {
        enabled_features.push("collab");
    }
    if !enabled_features.is_empty() {
        Output::key_value("Features", &enabled_features.join(", "));
    }

    if config.has_personal_features() {
        let sync_path = SyncEngine::sync_path()?;
        let machines = MachineState::list_all(&sync_path).unwrap_or_default();
        let old_ids = signing::old_ids_of_this_machine(&sync_path, &machines, &state.machine_id);
        if !old_ids.is_empty() {
            println!();
            super::machines::print_old_id_hints(&old_ids);
        }
        let statuses = signing::record_statuses(&sync_path, &state.machine_id)?;
        let old_builds = signing::old_builds(&machines, &statuses, &state.machine_id);
        if !old_builds.is_empty() {
            println!();
            super::machines::print_old_build_notes(&old_builds);
        }
    }

    // Conflicts warning
    if !conflict_state.conflicts.is_empty() {
        println!();
        println!("  {}", format!("{} Conflicts", Output::WARN).red().bold());
        Output::divider();
        for conflict in &conflict_state.conflicts {
            let time = relative_time(conflict.detected_at);
            println!(
                "  {:<18} {}",
                conflict.file_path.yellow(),
                time.bright_black()
            );
        }
        println!(
            "{}",
            "Run 'tether resolve' to fix conflicts".yellow().bold()
        );
    }

    // Split files into dotfiles and project configs
    let (dotfiles, project_configs): (Vec<_>, Vec<_>) = state
        .files
        .iter()
        .partition(|(file, _)| !file.starts_with("project:"));

    // Dotfiles
    if config.features.personal_dotfiles && !dotfiles.is_empty() {
        println!();
        println!("  {}", "Dotfiles".bright_cyan().bold());
        Output::divider();
        for (file, file_state) in &dotfiles {
            let (icon, status) = if file_state.synced {
                (Output::CHECK.green().to_string(), "Synced".to_string())
            } else {
                (Output::WARN.yellow().to_string(), "Modified".to_string())
            };
            let time = relative_time(file_state.last_modified);
            println!(
                "  {:<18} {} {:<10} {}",
                file,
                icon,
                status,
                time.bright_black()
            );
        }
    } else if config.features.personal_dotfiles {
        println!();
        Output::dim("  No dotfiles synced yet");
    }

    // Project configs
    if !project_configs.is_empty() {
        let mut org_to_team: std::collections::HashMap<String, String> =
            std::collections::HashMap::new();
        if let Some(teams) = &config.teams {
            for (team_name, team_config) in &teams.teams {
                if team_config.enabled {
                    for org in &team_config.orgs {
                        org_to_team.insert(org.to_lowercase(), team_name.clone());
                    }
                }
            }
        }

        let mut team_projects: std::collections::HashMap<
            String,
            Vec<(&String, &crate::sync::FileState)>,
        > = std::collections::HashMap::new();
        let mut personal_projects: Vec<(&String, &crate::sync::FileState)> = Vec::new();

        for (file, file_state) in &project_configs {
            let display_name = file.strip_prefix("project:").unwrap_or(file);
            if let Some(org) = crate::sync::extract_org_from_normalized_url(display_name) {
                if let Some(team_name) = org_to_team.get(&org.to_lowercase()) {
                    team_projects
                        .entry(team_name.clone())
                        .or_default()
                        .push((file, file_state));
                } else {
                    personal_projects.push((file, file_state));
                }
            } else {
                personal_projects.push((file, file_state));
            }
        }

        for (team_name, projects) in &team_projects {
            println!();
            println!(
                "  {}",
                format!("Team: {} (project secrets)", team_name)
                    .bright_cyan()
                    .bold()
            );
            Output::divider();
            for (file, file_state) in projects {
                let display_name = (*file).strip_prefix("project:").unwrap_or(file);
                let (icon, status) = if file_state.synced {
                    (Output::CHECK.green().to_string(), "Synced".to_string())
                } else {
                    (Output::WARN.yellow().to_string(), "Modified".to_string())
                };
                let time = relative_time(file_state.last_modified);
                println!(
                    "  {:<18} {} {:<10} {}",
                    display_name,
                    icon,
                    status,
                    time.bright_black()
                );
            }
        }

        if !personal_projects.is_empty() {
            println!();
            println!("  {}", "Personal Project Configs".bright_cyan().bold());
            Output::divider();
            for (file, file_state) in &personal_projects {
                let display_name = (*file).strip_prefix("project:").unwrap_or(file);
                let (icon, status) = if file_state.synced {
                    (Output::CHECK.green().to_string(), "Synced".to_string())
                } else {
                    (Output::WARN.yellow().to_string(), "Modified".to_string())
                };
                let time = relative_time(file_state.last_modified);
                println!(
                    "  {:<18} {} {:<10} {}",
                    display_name,
                    icon,
                    status,
                    time.bright_black()
                );
            }
        }
    }

    // Packages
    if config.features.personal_packages && !state.packages.is_empty() {
        println!();
        println!("  {}", "Packages".bright_cyan().bold());
        Output::divider();
        match crate::sync::membership::Membership::load_current(&config) {
            Ok(m) => println!(
                "  Installs the packages of profile {}. Not installed here: {} package(s) of \
                 other profiles",
                m.profile,
                m.excluded().len()
            ),
            Err(e) => Output::warning(&format!("{}. No synced package installs", e)),
        }
        for (manager, pkg_state) in &state.packages {
            let time = pkg_state
                .last_modified
                .map(relative_time)
                .unwrap_or_else(|| "-".to_string());
            println!(
                "  {:<18} {} {:<10} {}",
                manager,
                Output::CHECK.green(),
                "Synced",
                time.bright_black()
            );
        }
    } else if config.features.personal_packages {
        println!();
        Output::dim("  No packages synced yet");
    }

    println!();
    Ok(())
}

/// `status --json`. Times are RFC 3339 in UTC.
fn status_json(config: &Config, state: &SyncState) -> Result<serde_json::Value> {
    let pid = read_daemon_pid()?.filter(|pid| is_process_running(*pid));
    let features: Vec<&str> = [
        ("dotfiles", config.features.personal_dotfiles),
        ("packages", config.features.personal_packages),
        ("team", config.features.team_dotfiles),
        ("collab", config.features.collab_secrets),
    ]
    .into_iter()
    .filter_map(|(name, on)| on.then_some(name))
    .collect();
    let mut files: Vec<_> = state.files.iter().collect();
    files.sort_by_key(|(file, _)| *file);
    let mut packages: Vec<_> = state.packages.iter().collect();
    packages.sort_by_key(|(manager, _)| *manager);
    let conflicts = ConflictState::load().unwrap_or_default().conflicts;
    Ok(serde_json::json!({
        "machine": state.machine_id,
        "profile": config.profile_name(&state.machine_id),
        "version": env!("CARGO_PKG_VERSION"),
        "last_sync": state.last_sync,
        "daemon": { "running": pid.is_some(), "pid": pid },
        "features": features,
        "inbox": crate::packages::inbox::list()?.len(),
        "conflicts": conflicts
            .iter()
            .map(|c| serde_json::json!({ "file": c.file_path, "detected_at": c.detected_at }))
            .collect::<Vec<_>>(),
        "files": files
            .iter()
            .map(|(file, f)| serde_json::json!({
                "file": file,
                "synced": f.synced,
                "last_modified": f.last_modified,
            }))
            .collect::<Vec<_>>(),
        "packages": packages
            .iter()
            .map(|(manager, p)| serde_json::json!({
                "manager": manager,
                "last_sync": p.last_sync,
                "last_modified": p.last_modified,
            }))
            .collect::<Vec<_>>(),
    }))
}

fn read_daemon_pid() -> Result<Option<u32>> {
    let pid_path = Config::config_dir()?.join("daemon.pid");
    if !pid_path.exists() {
        return Ok(None);
    }

    let contents = std::fs::read_to_string(&pid_path)?;
    match contents.trim().parse::<u32>() {
        Ok(pid) if pid > 0 => Ok(Some(pid)),
        _ => Ok(None),
    }
}

fn is_process_running(pid: u32) -> bool {
    unsafe {
        if libc::kill(pid as libc::pid_t, 0) == 0 {
            return true;
        }
        // ESRCH = no such process, EPERM = exists but no permission
        std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
    }
}

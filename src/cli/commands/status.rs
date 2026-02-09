use crate::cli::output::relative_time;
use crate::cli::Output;
use crate::config::Config;
use crate::sync::{ConflictState, SyncState};
use anyhow::Result;
use owo_colors::OwoColorize;

pub async fn run() -> Result<()> {
    let config = match Config::load() {
        Ok(c) => c,
        Err(e) => {
            let msg = e.to_string();
            if msg.contains("Config version") {
                Output::error(&msg);
            } else {
                Output::error("Tether is not initialized. Run 'tether init' first.");
            }
            return Ok(());
        }
    };

    let state = SyncState::load()?;

    Output::section("Tether Status");
    println!();

    // Machine
    Output::key_value("Machine", &state.machine_id);
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

    // Conflicts warning
    let conflict_state = ConflictState::load().unwrap_or_default();
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

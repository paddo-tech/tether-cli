use crate::cli::Output;
use crate::config::Config;
use crate::sync::{ConflictState, FileState, SyncState};
use anyhow::Result;
use chrono::Local;
use comfy_table::{Attribute, Cell, Color};
use owo_colors::OwoColorize;
use std::path::PathBuf;

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

    // Features summary (one line)
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
        Output::dim(&format!("Features: {}", enabled_features.join(", ")));
        println!();
    }

    // Daemon status
    let pid = read_daemon_pid()?;
    let (status_label, is_running) = match pid {
        Some(pid) if is_process_running(pid) => (format!("Running (PID {pid})"), true),
        Some(pid) => (format!("Not running (stale PID {pid})"), false),
        None => ("Not running".to_string(), false),
    };

    let mut daemon_table = Output::table_full();
    daemon_table
        .set_header(vec![
            Cell::new("Daemon")
                .add_attribute(Attribute::Bold)
                .fg(Color::Cyan),
            Cell::new(""),
        ])
        .add_row(vec![
            Cell::new("Status"),
            Cell::new(format!("{} {}", Output::DOT, status_label)).fg(if is_running {
                Color::Green
            } else {
                Color::Yellow
            }),
        ])
        .add_row(vec![
            Cell::new("Log"),
            Cell::new(daemon_log_path()?.display().to_string()),
        ]);
    println!("{daemon_table}");
    println!();

    // Conflicts warning
    let conflict_state = ConflictState::load().unwrap_or_default();
    if !conflict_state.conflicts.is_empty() {
        let mut conflict_table = Output::table_full();
        conflict_table.set_header(vec![
            Cell::new(format!("{}  Conflicts", Output::WARN))
                .add_attribute(Attribute::Bold)
                .fg(Color::Red),
            Cell::new("Detected")
                .add_attribute(Attribute::Bold)
                .fg(Color::Red),
        ]);

        for conflict in &conflict_state.conflicts {
            let local_time = conflict.detected_at.with_timezone(&Local);
            conflict_table.add_row(vec![
                Cell::new(&conflict.file_path).fg(Color::Yellow),
                Cell::new(local_time.format("%Y-%m-%d %H:%M").to_string()),
            ]);
        }
        println!("{conflict_table}");
        println!(
            "{}",
            "Run 'tether resolve' to fix conflicts".yellow().bold()
        );
        println!();
    }

    // Sync info
    let mut sync_table = Output::table_full();
    sync_table
        .set_header(vec![
            Cell::new("Sync")
                .add_attribute(Attribute::Bold)
                .fg(Color::Cyan),
            Cell::new(""),
        ])
        .add_row(vec![
            Cell::new("Last Sync"),
            Cell::new(
                state
                    .last_sync
                    .with_timezone(&Local)
                    .format("%Y-%m-%d %H:%M")
                    .to_string(),
            )
            .fg(Color::Green),
        ])
        .add_row(vec![
            Cell::new("Last Upgrade"),
            Cell::new(
                state
                    .last_upgrade
                    .map(|t| t.with_timezone(&Local).format("%Y-%m-%d %H:%M").to_string())
                    .unwrap_or_else(|| "Never".to_string()),
            ),
        ])
        .add_row(vec![Cell::new("Machine"), Cell::new(&state.machine_id)])
        .add_row(vec![Cell::new("Backend"), Cell::new(&config.backend.url)]);
    println!("{sync_table}");
    println!();

    // Split files into dotfiles and project configs
    let (dotfiles, project_configs): (Vec<_>, Vec<_>) = state
        .files
        .iter()
        .partition(|(file, _)| !file.starts_with("project:"));

    // Dotfiles - minimal table for lists (only show if feature enabled)
    if config.features.personal_dotfiles && !dotfiles.is_empty() {
        let mut files_table = Output::table_minimal();
        files_table.set_header(vec![
            Cell::new("Dotfiles")
                .add_attribute(Attribute::Bold)
                .fg(Color::Cyan),
            Cell::new("Status")
                .add_attribute(Attribute::Bold)
                .fg(Color::Cyan),
            Cell::new("Modified")
                .add_attribute(Attribute::Bold)
                .fg(Color::Cyan),
        ]);

        for (file, file_state) in &dotfiles {
            let (status, color) = if file_state.synced {
                (format!("{} Synced", Output::CHECK), Color::Green)
            } else {
                (format!("{} Modified", Output::WARN), Color::Yellow)
            };

            files_table.add_row(vec![
                Cell::new(file),
                Cell::new(status).fg(color),
                Cell::new(
                    file_state
                        .last_modified
                        .with_timezone(&Local)
                        .format("%Y-%m-%d %H:%M")
                        .to_string(),
                ),
            ]);
        }
        println!("{files_table}");
        println!();
    } else if config.features.personal_dotfiles {
        Output::dim("  No dotfiles synced yet");
        println!();
    }

    // Project configs - split by team vs personal
    if !project_configs.is_empty() {
        // Build org -> team mapping
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

        // Split projects by ownership
        let mut team_projects: std::collections::HashMap<String, Vec<(&String, &FileState)>> =
            std::collections::HashMap::new();
        let mut personal_projects: Vec<(&String, &FileState)> = Vec::new();

        for (file, file_state) in &project_configs {
            let display_name = file.strip_prefix("project:").unwrap_or(file);
            // Extract org from path like "github.com/org/repo/..."
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

        // Show team projects first
        for (team_name, projects) in &team_projects {
            let mut project_table = Output::table_minimal();
            project_table.set_header(vec![
                Cell::new(format!("Team: {} (project secrets)", team_name))
                    .add_attribute(Attribute::Bold)
                    .fg(Color::Cyan),
                Cell::new("Status")
                    .add_attribute(Attribute::Bold)
                    .fg(Color::Cyan),
                Cell::new("Modified")
                    .add_attribute(Attribute::Bold)
                    .fg(Color::Cyan),
            ]);

            for (file, file_state) in projects {
                let (status, color) = if file_state.synced {
                    (format!("{} Synced", Output::CHECK), Color::Green)
                } else {
                    (format!("{} Modified", Output::WARN), Color::Yellow)
                };
                let display_name = (*file).strip_prefix("project:").unwrap_or(file);
                project_table.add_row(vec![
                    Cell::new(display_name),
                    Cell::new(status).fg(color),
                    Cell::new(
                        file_state
                            .last_modified
                            .with_timezone(&Local)
                            .format("%Y-%m-%d %H:%M")
                            .to_string(),
                    ),
                ]);
            }
            println!("{project_table}");
            println!();
        }

        // Show personal projects
        if !personal_projects.is_empty() {
            let mut project_table = Output::table_minimal();
            project_table.set_header(vec![
                Cell::new("Personal Project Configs")
                    .add_attribute(Attribute::Bold)
                    .fg(Color::Cyan),
                Cell::new("Status")
                    .add_attribute(Attribute::Bold)
                    .fg(Color::Cyan),
                Cell::new("Modified")
                    .add_attribute(Attribute::Bold)
                    .fg(Color::Cyan),
            ]);

            for (file, file_state) in &personal_projects {
                let (status, color) = if file_state.synced {
                    (format!("{} Synced", Output::CHECK), Color::Green)
                } else {
                    (format!("{} Modified", Output::WARN), Color::Yellow)
                };
                let display_name = (*file).strip_prefix("project:").unwrap_or(file);
                project_table.add_row(vec![
                    Cell::new(display_name),
                    Cell::new(status).fg(color),
                    Cell::new(
                        file_state
                            .last_modified
                            .with_timezone(&Local)
                            .format("%Y-%m-%d %H:%M")
                            .to_string(),
                    ),
                ]);
            }
            println!("{project_table}");
            println!();
        }
    }

    // Packages - minimal table for lists (only show if feature enabled)
    if config.features.personal_packages && !state.packages.is_empty() {
        let mut packages_table = Output::table_minimal();
        packages_table.set_header(vec![
            Cell::new("Packages")
                .add_attribute(Attribute::Bold)
                .fg(Color::Cyan),
            Cell::new("Last Sync")
                .add_attribute(Attribute::Bold)
                .fg(Color::Cyan),
        ]);

        for (manager, pkg_state) in &state.packages {
            packages_table.add_row(vec![
                Cell::new(format!("{} {}", Output::CHECK, manager)).fg(Color::Green),
                Cell::new(
                    pkg_state
                        .last_sync
                        .with_timezone(&Local)
                        .format("%Y-%m-%d %H:%M")
                        .to_string(),
                ),
            ]);
        }
        println!("{packages_table}");
        println!();
    } else if config.features.personal_packages {
        Output::dim("  No packages synced yet");
        println!();
    }

    Ok(())
}

fn daemon_log_path() -> Result<PathBuf> {
    Ok(Config::config_dir()?.join("daemon.log"))
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
            true
        } else {
            let err = std::io::Error::last_os_error();
            err.kind() != std::io::ErrorKind::NotFound
        }
    }
}

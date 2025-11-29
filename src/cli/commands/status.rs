use crate::cli::Output;
use crate::config::Config;
use crate::sync::SyncState;
use anyhow::Result;
use comfy_table::{presets::UTF8_FULL, Attribute, Cell, Color, ContentArrangement, Table};
use owo_colors::OwoColorize;
use std::path::PathBuf;

pub async fn run() -> Result<()> {
    let config = match Config::load() {
        Ok(c) => c,
        Err(_) => {
            Output::error("Tether is not initialized. Run 'tether init' first.");
            return Ok(());
        }
    };

    let state = SyncState::load()?;

    println!();
    println!("{}", "🔗 Tether Status".bright_cyan().bold());
    println!();

    // Daemon table
    let pid = read_daemon_pid()?;
    let (status_label, status_color) = match pid {
        Some(pid) if is_process_running(pid) => {
            (format!("● Running (PID {pid})"), Color::Green)
        }
        Some(pid) => (format!("● Not running (stale PID {pid})"), Color::Yellow),
        None => ("● Not running".to_string(), Color::Yellow),
    };

    let mut daemon_table = Table::new();
    daemon_table
        .load_preset(UTF8_FULL)
        .set_content_arrangement(ContentArrangement::Dynamic)
        .set_header(vec![
            Cell::new("Daemon")
                .add_attribute(Attribute::Bold)
                .fg(Color::Cyan),
            Cell::new(""),
        ])
        .add_row(vec![
            Cell::new("Status"),
            Cell::new(status_label).fg(status_color),
        ])
        .add_row(vec![
            Cell::new("Info"),
            Cell::new(format!("Log file: {}", daemon_log_path()?.display())),
        ]);
    println!("{daemon_table}");
    println!();

    // Sync table
    let mut sync_table = Table::new();
    sync_table
        .load_preset(UTF8_FULL)
        .set_content_arrangement(ContentArrangement::Dynamic)
        .set_header(vec![
            Cell::new("Sync")
                .add_attribute(Attribute::Bold)
                .fg(Color::Cyan),
            Cell::new(""),
        ])
        .add_row(vec![
            Cell::new("Last Sync"),
            Cell::new(state.last_sync.format("%Y-%m-%d %H:%M:%S").to_string()).fg(Color::Green),
        ])
        .add_row(vec![Cell::new("Machine ID"), Cell::new(&state.machine_id)])
        .add_row(vec![Cell::new("Backend"), Cell::new(&config.backend.url)]);
    println!("{sync_table}");
    println!();

    // Dotfiles table
    if !state.files.is_empty() {
        let mut files_table = Table::new();
        files_table
            .load_preset(UTF8_FULL)
            .set_content_arrangement(ContentArrangement::Dynamic)
            .set_header(vec![
                Cell::new("📁 Dotfiles")
                    .add_attribute(Attribute::Bold)
                    .fg(Color::Cyan),
                Cell::new("Status")
                    .add_attribute(Attribute::Bold)
                    .fg(Color::Cyan),
                Cell::new("Last Modified")
                    .add_attribute(Attribute::Bold)
                    .fg(Color::Cyan),
            ]);

        for (file, file_state) in &state.files {
            let status_cell = if file_state.synced {
                Cell::new("✓ Synced").fg(Color::Green)
            } else {
                Cell::new("⚠ Modified").fg(Color::Yellow)
            };

            files_table.add_row(vec![
                Cell::new(file),
                status_cell,
                Cell::new(
                    file_state
                        .last_modified
                        .format("%Y-%m-%d %H:%M:%S")
                        .to_string(),
                ),
            ]);
        }
        println!("{files_table}");
        println!();
    } else {
        println!("{}", "📁 Dotfiles: No files synced yet".bright_black());
        println!();
    }

    // Packages table
    if !state.packages.is_empty() {
        let mut packages_table = Table::new();
        packages_table
            .load_preset(UTF8_FULL)
            .set_content_arrangement(ContentArrangement::Dynamic)
            .set_header(vec![
                Cell::new("📦 Package Manager")
                    .add_attribute(Attribute::Bold)
                    .fg(Color::Cyan),
                Cell::new("Last Sync")
                    .add_attribute(Attribute::Bold)
                    .fg(Color::Cyan),
            ]);

        for (manager, pkg_state) in &state.packages {
            packages_table.add_row(vec![
                Cell::new(format!("✓ {}", manager)).fg(Color::Green),
                Cell::new(pkg_state.last_sync.format("%Y-%m-%d %H:%M:%S").to_string()),
            ]);
        }
        println!("{packages_table}");
        println!();
    } else {
        println!("{}", "📦 Packages: No packages synced yet".bright_black());
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

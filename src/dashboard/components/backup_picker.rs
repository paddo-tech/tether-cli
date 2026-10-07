//! The backups of one dotfile, as `tether restore` lists them, to restore one.

use super::confirm::Confirm;
use super::{cursor_down, picker as picker_popup};
use crate::cli::output::relative_time;
use crate::dashboard::app::{App, Overlay};
use crate::dashboard::msg::Cmd;
use crossterm::event::{KeyCode, KeyEvent};
use ratatui::prelude::*;

pub struct BackupPicker {
    pub path: String,
    /// Backup timestamps that hold the file, newest first
    pub backups: Vec<String>,
    pub cursor: usize,
}

/// Open the backups of a dotfile, or say there are none.
pub fn open(app: &mut App, path: &str) {
    let backups: Vec<String> = crate::sync::list_backups()
        .unwrap_or_default()
        .into_iter()
        .filter(|ts| {
            crate::sync::list_backup_files(ts)
                .unwrap_or_default()
                .iter()
                .any(|(category, file)| category == "dotfiles" && file == path)
        })
        .collect();
    if backups.is_empty() {
        app.flash_info(format!("No backup of {}", path));
        return;
    }
    app.overlays.push(Overlay::BackupPicker(BackupPicker {
        path: path.to_string(),
        backups,
        cursor: 0,
    }));
}

/// The overlay was popped off the stack; push it back to keep it open. Enter only picks
/// the backup: the restore waits for `y` in the confirm.
pub fn handle_key(app: &mut App, mut picker: BackupPicker, key: KeyEvent) -> Option<Cmd> {
    match key.code {
        KeyCode::Esc => return None,
        KeyCode::Char('j') | KeyCode::Down => cursor_down(&mut picker.cursor, picker.backups.len()),
        KeyCode::Char('k') | KeyCode::Up => picker.cursor = picker.cursor.saturating_sub(1),
        KeyCode::Enter => {
            if let Some(timestamp) = picker.backups.get(picker.cursor) {
                app.overlays.push(Overlay::Confirm(Confirm::RestoreBackup {
                    path: picker.path,
                    timestamp: timestamp.clone(),
                    arming: Default::default(),
                }));
            }
            return None;
        }
        _ => {}
    }
    app.overlays.push(Overlay::BackupPicker(picker));
    None
}

pub fn render(f: &mut Frame, app: &App, picker: &BackupPicker) {
    let t = &app.theme;
    let rows = picker
        .backups
        .iter()
        .map(|ts| {
            let age = crate::sync::backup::parse_backup_timestamp(ts)
                .map(relative_time)
                .unwrap_or_default();
            (
                Line::from(Span::styled(ts.clone(), Style::default().fg(t.text))),
                Line::from(Span::styled(age, Style::default().fg(t.dim))),
            )
        })
        .collect();
    picker_popup(
        f,
        app,
        &format!("Backups of {}", picker.path),
        rows,
        picker.cursor,
        &[("⏎", "pick"), ("esc", "close")],
        Some("Tether backs a file up before a sync or restore overwrites it"),
    );
}

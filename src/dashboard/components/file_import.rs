use super::{clamp_cursor, cursor_down, picker as picker_popup};
use crate::dashboard::app::{App, Overlay};
use crate::dashboard::config_edit;
use crate::dashboard::msg::Cmd;
use crossterm::event::{KeyCode, KeyEvent};
use ratatui::prelude::*;

pub struct ImportItem {
    pub path: String,
    pub source_profile: String,
}

/// Picker for dotfiles tracked by other profiles.
pub struct FileImport {
    pub items: Vec<ImportItem>,
    pub cursor: usize,
}

/// The overlay was popped off the stack; push it back to keep it open.
pub fn handle_key(app: &mut App, mut picker: FileImport, key: KeyEvent) -> Option<Cmd> {
    match key.code {
        KeyCode::Esc => return None,
        KeyCode::Char('j') | KeyCode::Down => cursor_down(&mut picker.cursor, picker.items.len()),
        KeyCode::Char('k') | KeyCode::Up => picker.cursor = picker.cursor.saturating_sub(1),
        KeyCode::Enter if picker.cursor < picker.items.len() => {
            let item = picker.items.remove(picker.cursor);
            import(app, &item.path);
            if picker.items.is_empty() {
                return None;
            }
            clamp_cursor(&mut picker.cursor, picker.items.len());
        }
        _ => {}
    }
    app.overlays.push(Overlay::FileImport(picker));
    None
}

fn import(app: &mut App, path: &str) {
    let (Some(config), Some(ss)) = (&mut app.state.config, &app.state.sync_state) else {
        return;
    };
    if let Err(e) = config_edit::add_profile_dotfile(config, &ss.machine_id, path) {
        app.flash_error(e);
        app.reload_state();
        return;
    }
    if let Ok(mut sync_state) = crate::sync::SyncState::load() {
        sync_state.dismissed_imports.remove(path);
        let _ = sync_state.save();
    }
    app.flash_success(format!("imported {}", path));
    app.reload_state();
}

pub fn render(f: &mut Frame, app: &App, picker: &FileImport) {
    let t = &app.theme;
    let rows = picker
        .items
        .iter()
        .map(|i| {
            (
                Line::from(Span::styled(i.path.clone(), Style::default().fg(t.text))),
                Line::from(Span::styled(
                    i.source_profile.clone(),
                    Style::default().fg(t.team),
                )),
            )
        })
        .collect();
    picker_popup(
        f,
        app,
        "Import file from profile",
        rows,
        picker.cursor,
        &[("⏎", "import"), ("esc", "close")],
        None,
    );
}

use super::{cursor_down, picker as picker_popup};
use crate::dashboard::app::{App, Overlay};
use crate::dashboard::msg::Cmd;
use crossterm::event::{KeyCode, KeyEvent};
use ratatui::prelude::*;

/// Picker for this machine's profile.
pub struct ProfilePicker {
    pub options: Vec<String>,
    pub cursor: usize,
}

/// The overlay was popped off the stack; push it back to keep it open.
pub fn handle_key(app: &mut App, mut picker: ProfilePicker, key: KeyEvent) -> Option<Cmd> {
    match key.code {
        KeyCode::Esc => return None,
        KeyCode::Char('j') | KeyCode::Down => cursor_down(&mut picker.cursor, picker.options.len()),
        KeyCode::Char('k') | KeyCode::Up => picker.cursor = picker.cursor.saturating_sub(1),
        KeyCode::Enter => {
            select(app, picker);
            return None;
        }
        _ => {}
    }
    app.overlays.push(Overlay::ProfilePicker(picker));
    None
}

fn select(app: &mut App, picker: ProfilePicker) {
    let (Some(config), Some(sync_state)) = (&mut app.state.config, &app.state.sync_state) else {
        return;
    };
    if let Some(profile_name) = picker.options.into_iter().nth(picker.cursor) {
        config
            .machine_profiles
            .insert(sync_state.machine_id.clone(), profile_name);
    }
    if config.save().is_err() {
        app.flash_error("save failed");
    }
    app.reload_state();
}

pub fn render(f: &mut Frame, app: &App, picker: &ProfilePicker) {
    let t = &app.theme;
    let current = app
        .state
        .config
        .as_ref()
        .map(|c| c.profile_name(app.machine_id()).to_string());
    let rows = picker
        .options
        .iter()
        .map(|o| {
            let right = if current.as_deref() == Some(o.as_str()) {
                Span::styled("current", Style::default().fg(t.ok))
            } else {
                Span::raw("")
            };
            (
                Line::from(Span::styled(o.clone(), Style::default().fg(t.text))),
                Line::from(right),
            )
        })
        .collect();
    picker_popup(
        f,
        app,
        "Profile for this machine",
        rows,
        picker.cursor,
        &[("⏎", "select"), ("esc", "cancel")],
        Some("New: tether machines profile create <name>"),
    );
}

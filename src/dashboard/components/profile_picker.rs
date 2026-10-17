use super::confirm::Confirm;
use super::machines::FleetTarget;
use super::{cursor_down, picker as picker_popup};
use crate::dashboard::app::{App, Overlay};
use crate::dashboard::msg::Cmd;
use crossterm::event::{KeyCode, KeyEvent};
use ratatui::prelude::*;

/// Picker for a machine's profile.
pub struct ProfilePicker {
    pub options: Vec<String>,
    pub cursor: usize,
    /// Another machine, which gets the profile by fleet command. `None` is this machine
    pub target: Option<FleetTarget>,
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
    if let Some(target) = picker.target {
        if let Some(profile) = picker.options.into_iter().nth(picker.cursor) {
            app.overlays.push(Overlay::Confirm(Confirm::Fleet {
                action: crate::sync::commands::Action::Profile { profile },
                local: false,
                targets: vec![target],
                arming: Default::default(),
            }));
        }
        return;
    }
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
    let current = match &picker.target {
        Some(target) => app
            .state
            .machines
            .iter()
            .find(|m| m.machine_id == target.id)
            .map(|m| super::machines::profile_of(app, m)),
        None => app
            .state
            .config
            .as_ref()
            .map(|c| c.profile_name(app.machine_id()).to_string()),
    };
    let title = match &picker.target {
        Some(target) => format!("Profile for {}", target.label),
        None => "Profile for this machine".to_string(),
    };
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
        &title,
        rows,
        picker.cursor,
        &[("⏎", "select"), ("esc", "cancel")],
        Some("New: tether machines profile create <name>"),
    );
}

use super::{cursor_down, manager_label, picker as picker_popup};
use crate::dashboard::app::{App, Overlay};
use crate::dashboard::msg::Cmd;
use crate::sync::membership::Edit;
use crossterm::event::{KeyCode, KeyEvent};
use ratatui::prelude::*;
use std::collections::BTreeSet;

/// Checklist of the profiles a package belongs to.
pub struct PackageProfiles {
    pub manager_key: String,
    pub name: String,
    pub options: Vec<String>,
    pub checked: BTreeSet<String>,
    /// The members when the checklist opened. A save compares the repo against them
    pub base: BTreeSet<String>,
    pub cursor: usize,
}

/// Open the checklist with the package's current members checked.
pub fn open(app: &mut App, manager_key: &str, name: &str) {
    if let Some(error) = app.state.membership_error.clone() {
        app.flash_error(error);
        return;
    }
    let (Some(config), Some(membership)) = (&app.state.config, &app.state.membership) else {
        return;
    };
    let mut options: Vec<String> = config.profiles.keys().cloned().collect();
    options.sort();
    let checked = membership.members(manager_key, name);
    app.overlays.push(Overlay::PackageProfiles(PackageProfiles {
        manager_key: manager_key.to_string(),
        name: name.to_string(),
        options,
        base: checked.clone(),
        checked,
        cursor: 0,
    }));
}

/// The overlay was popped off the stack; push it back to keep it open.
pub fn handle_key(app: &mut App, mut picker: PackageProfiles, key: KeyEvent) -> Option<Cmd> {
    match key.code {
        KeyCode::Esc => return None,
        KeyCode::Char('j') | KeyCode::Down => cursor_down(&mut picker.cursor, picker.options.len()),
        KeyCode::Char('k') | KeyCode::Up => picker.cursor = picker.cursor.saturating_sub(1),
        KeyCode::Char(' ') => {
            if let Some(option) = picker.options.get(picker.cursor) {
                if !picker.checked.remove(option) {
                    picker.checked.insert(option.clone());
                }
            }
        }
        KeyCode::Enter => {
            if picker.checked.is_empty() {
                app.flash_error("A package needs one profile at least. Uninstall it instead");
            } else {
                let edit = Edit {
                    add: picker.checked.difference(&picker.base).cloned().collect(),
                    remove: picker.base.difference(&picker.checked).cloned().collect(),
                    seen: Some(picker.base),
                };
                if edit.add.is_empty() && edit.remove.is_empty() {
                    return None;
                }
                return Some(Cmd::SaveProfiles {
                    manager_key: picker.manager_key,
                    name: picker.name,
                    edit,
                });
            }
        }
        _ => {}
    }
    app.overlays.push(Overlay::PackageProfiles(picker));
    None
}

pub fn render(f: &mut Frame, app: &App, picker: &PackageProfiles) {
    let t = &app.theme;
    let current = app.state.membership.as_ref().map(|m| m.profile.as_str());
    let rows = picker
        .options
        .iter()
        .map(|o| {
            let mark = if picker.checked.contains(o) {
                "[x] "
            } else {
                "[ ] "
            };
            let right = if current == Some(o.as_str()) {
                Span::styled("this machine", Style::default().fg(t.ok))
            } else {
                Span::raw("")
            };
            (
                Line::from(vec![
                    Span::styled(mark, Style::default().fg(t.accent)),
                    Span::styled(o.clone(), Style::default().fg(t.text)),
                ]),
                Line::from(right),
            )
        })
        .collect();
    picker_popup(
        f,
        app,
        &format!(
            "Profiles for {} ({})",
            picker.name,
            manager_label(&picker.manager_key)
        ),
        rows,
        picker.cursor,
        &[("space", "toggle"), ("⏎", "save"), ("esc", "cancel")],
        Some("Machines in the checked profiles install it"),
    );
}

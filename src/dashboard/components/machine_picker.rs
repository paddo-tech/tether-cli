use super::confirm::Confirm;
use super::machines::{command_refusal, FleetTarget};
use super::{cursor_down, manager_label, picker as picker_popup};
use crate::dashboard::app::{App, Overlay};
use crate::dashboard::msg::Cmd;
use crate::sync::commands::Action;
use crossterm::event::{KeyCode, KeyEvent};
use ratatui::prelude::*;
use std::collections::BTreeSet;

/// One machine in the checklist.
pub struct Choice {
    pub target: FleetTarget,
    pub this: bool,
    /// Why this machine cannot send it a fleet command
    pub refusal: Option<String>,
}

/// Checklist of the machines that a package install or uninstall applies to. This machine
/// is checked when the checklist opens.
pub struct MachinePicker {
    pub uninstall: bool,
    pub manager_key: String,
    pub name: String,
    pub options: Vec<Choice>,
    pub checked: BTreeSet<String>,
    pub cursor: usize,
}

/// Open the checklist when another machine can take the change. With no such machine,
/// return false, and the caller acts on this machine alone, as before fleet commands.
/// An uninstall offers the machines that list the package, an install those that lack it.
pub fn open(app: &mut App, uninstall: bool, manager_key: &str, name: &str) -> bool {
    use crate::sync::membership::canonical_id;
    let this = app.machine_id().to_string();
    let id = canonical_id(manager_key, name);
    let others: Vec<Choice> = app
        .state
        .machines
        .iter()
        .filter(|m| m.machine_id != this)
        .filter(|m| {
            let listed = m
                .packages
                .get(manager_key)
                .is_some_and(|p| p.iter().any(|n| canonical_id(manager_key, n) == id));
            listed == uninstall
        })
        .map(|m| Choice {
            target: FleetTarget::of(m),
            this: false,
            refusal: command_refusal(app, m),
        })
        .collect();
    if others.iter().all(|c| c.refusal.is_some()) {
        return false;
    }
    let here = app
        .state
        .machines
        .iter()
        .find(|m| m.machine_id == this)
        .map(FleetTarget::of)
        .unwrap_or_else(|| FleetTarget {
            id: this.clone(),
            label: crate::sync::local_hostname(),
            last_sync: chrono::Utc::now(),
        });
    let mut options = vec![Choice {
        target: here,
        this: true,
        refusal: None,
    }];
    options.extend(others);
    app.overlays.push(Overlay::MachinePicker(MachinePicker {
        uninstall,
        manager_key: manager_key.to_string(),
        name: name.to_string(),
        options,
        checked: BTreeSet::from([this]),
        cursor: 0,
    }));
    true
}

/// The overlay was popped off the stack; push it back to keep it open.
pub fn handle_key(app: &mut App, mut picker: MachinePicker, key: KeyEvent) -> Option<Cmd> {
    match key.code {
        KeyCode::Esc => return None,
        KeyCode::Char('j') | KeyCode::Down => cursor_down(&mut picker.cursor, picker.options.len()),
        KeyCode::Char('k') | KeyCode::Up => picker.cursor = picker.cursor.saturating_sub(1),
        KeyCode::Char(' ') => {
            if let Some(choice) = picker.options.get(picker.cursor) {
                if let Some(why) = &choice.refusal {
                    app.flash_info(why.clone());
                } else if !picker.checked.remove(&choice.target.id) {
                    picker.checked.insert(choice.target.id.clone());
                }
            }
        }
        KeyCode::Enter => {
            if picker.checked.is_empty() {
                app.flash_info("Check a machine first");
            } else {
                choose(app, picker);
                return None;
            }
        }
        _ => {}
    }
    app.overlays.push(Overlay::MachinePicker(picker));
    None
}

/// Only this machine checked: the same question as without fleet commands. Other machines
/// checked: one confirm that names them all.
fn choose(app: &mut App, picker: MachinePicker) {
    let local = picker
        .options
        .iter()
        .any(|c| c.this && picker.checked.contains(&c.target.id));
    let targets: Vec<FleetTarget> = picker
        .options
        .into_iter()
        .filter(|c| !c.this && picker.checked.contains(&c.target.id))
        .map(|c| c.target)
        .collect();
    if targets.is_empty() {
        if picker.uninstall {
            app.overlays.push(Overlay::Confirm(Confirm::Uninstall {
                manager_key: picker.manager_key,
                name: picker.name,
                arming: Default::default(),
            }));
        } else if let Some(Overlay::PkgImport(import)) = app.overlays.last_mut() {
            import.confirm = Some((picker.manager_key, picker.name));
        }
        return;
    }
    let (manager, name) = (picker.manager_key, picker.name);
    let action = if picker.uninstall {
        Action::Uninstall { manager, name }
    } else {
        Action::Install { manager, name }
    };
    app.overlays.push(Overlay::Confirm(Confirm::Fleet {
        action,
        local,
        targets,
        arming: Default::default(),
    }));
}

pub fn render(f: &mut Frame, app: &App, picker: &MachinePicker) {
    let t = &app.theme;
    let rows = picker
        .options
        .iter()
        .map(|c| {
            let mark = if picker.checked.contains(&c.target.id) {
                "[x] "
            } else {
                "[ ] "
            };
            let right = match (&c.refusal, c.this) {
                (_, true) => Span::styled("this machine", Style::default().fg(t.ok)),
                (Some(_), _) => Span::styled("cannot run commands", Style::default().fg(t.dim)),
                (None, false) => Span::raw(""),
            };
            let color = if c.refusal.is_some() { t.dim } else { t.text };
            (
                Line::from(vec![
                    Span::styled(mark, Style::default().fg(t.accent)),
                    Span::styled(c.target.label.clone(), Style::default().fg(color)),
                ]),
                Line::from(right),
            )
        })
        .collect();
    let verb = if picker.uninstall {
        "Uninstall"
    } else {
        "Install"
    };
    picker_popup(
        f,
        app,
        &format!(
            "{} {} ({}) on",
            verb,
            picker.name,
            manager_label(&picker.manager_key)
        ),
        rows,
        picker.cursor,
        &[("space", "toggle"), ("⏎", "next"), ("esc", "cancel")],
        Some("Other machines get a signed command and run it on their next sync"),
    );
}

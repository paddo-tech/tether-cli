use super::{backdrop, confirm, cursor_down, manager_label, picker as picker_popup};
use crate::dashboard::app::{App, Overlay};
use crate::dashboard::msg::Cmd;
use crossterm::event::{KeyCode, KeyEvent};
use ratatui::prelude::*;

pub struct PkgImportItem {
    pub manager_key: String,
    pub name: String,
    pub sources: Vec<String>,
}

/// Picker for packages installed on other machines. Stays open while installs run.
pub struct PkgImport {
    pub items: Vec<PkgImportItem>,
    pub cursor: usize,
    /// (manager_key, name) awaiting install confirmation.
    pub confirm: Option<(String, String)>,
}

impl PkgImport {
    /// Drop an installed package; false when nothing is left to offer.
    pub fn remove(&mut self, manager_key: &str, name: &str) -> bool {
        self.items
            .retain(|i| !(i.manager_key == manager_key && i.name == name));
        if self.cursor >= self.items.len() {
            self.cursor = self.items.len().saturating_sub(1);
        }
        !self.items.is_empty()
    }
}

/// The overlay was popped off the stack; push it back to keep it open.
pub fn handle_key(app: &mut App, mut picker: PkgImport, key: KeyEvent) -> Option<Cmd> {
    let mut cmd = None;
    if let Some((manager_key, name)) = picker.confirm.take() {
        match key.code {
            KeyCode::Char('y') | KeyCode::Enter => {
                cmd = Some(app.start_install(manager_key, name));
            }
            KeyCode::Char('n') | KeyCode::Esc => {}
            _ => picker.confirm = Some((manager_key, name)),
        }
    } else {
        match key.code {
            KeyCode::Esc => return None,
            KeyCode::Char('j') | KeyCode::Down => {
                cursor_down(&mut picker.cursor, picker.items.len())
            }
            KeyCode::Char('k') | KeyCode::Up => picker.cursor = picker.cursor.saturating_sub(1),
            KeyCode::Enter if picker.cursor < picker.items.len() && app.running.is_none() => {
                let item = &picker.items[picker.cursor];
                picker.confirm = Some((item.manager_key.clone(), item.name.clone()));
            }
            _ => {}
        }
    }
    app.overlays.push(Overlay::PkgImport(picker));
    cmd
}

/// First source host, then how many more have the package.
fn sources_label(sources: &[String]) -> String {
    let first = sources
        .first()
        .map(|s| s.trim_end_matches(".local"))
        .unwrap_or("");
    match sources.len() {
        0 | 1 => first.to_string(),
        n => format!("{} +{}", first, n - 1),
    }
}

/// The picker first, then its install question on top of it.
pub fn render(f: &mut Frame, app: &App, picker: &PkgImport) {
    let t = &app.theme;
    let rows = picker
        .items
        .iter()
        .map(|i| {
            (
                Line::from(vec![
                    Span::styled(i.name.clone(), Style::default().fg(t.text)),
                    Span::styled(
                        format!("  {}", manager_label(&i.manager_key)),
                        Style::default().fg(t.dim),
                    ),
                ]),
                Line::from(Span::styled(
                    sources_label(&i.sources),
                    Style::default().fg(t.team),
                )),
            )
        })
        .collect();
    picker_popup(
        f,
        app,
        "Import package",
        rows,
        picker.cursor,
        &[("⏎", "install"), ("esc", "close")],
        None,
    );

    if let Some((manager_key, name)) = &picker.confirm {
        backdrop(f, t);
        app.hits.borrow_mut().clear();
        confirm::render_popup(
            f,
            app,
            "Install",
            &format!("Install {} ({})?", name, manager_label(manager_key)),
            t.ok,
        );
    }
}

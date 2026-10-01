use super::{centered, confirm, cursor_down, manager_label};
use crate::dashboard::app::{App, Overlay};
use crate::dashboard::msg::Cmd;
use crate::dashboard::theme::Theme;
use crossterm::event::{KeyCode, KeyEvent};
use ratatui::{prelude::*, widgets::*};

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

pub fn render(f: &mut Frame, picker: &PkgImport, t: &Theme) {
    if let Some((manager_key, name)) = &picker.confirm {
        confirm::render_popup(
            f,
            "Install",
            &format!("Install {} ({})?", name, manager_label(manager_key)),
            t.ok,
            t,
        );
    }

    let area = f.area();
    let title = " Import package ";
    let max_item_len = picker
        .items
        .iter()
        .map(|i| {
            let sources = i.sources.join(", ");
            i.name.len() + manager_label(&i.manager_key).len() + sources.len() + 6
        })
        .max()
        .unwrap_or(20);
    let min_width = max_item_len.max(title.len() + 2).max(40) + 6;
    let width = (min_width as u16).min(area.width.saturating_sub(4));
    let max_visible = 15usize;
    let visible = picker.items.len().min(max_visible);
    let height = ((visible + 5) as u16).min(area.height.saturating_sub(2));
    let popup_area = centered(area, width, height);

    f.render_widget(Clear, popup_area);

    let scroll = if picker.cursor >= max_visible {
        picker.cursor - max_visible + 1
    } else {
        0
    };

    let mut text = vec![Line::from("")];
    for (i, item) in picker
        .items
        .iter()
        .enumerate()
        .skip(scroll)
        .take(max_visible)
    {
        let selected = i == picker.cursor;
        let marker = if selected { "> " } else { "  " };
        let label = manager_label(&item.manager_key);
        let sources = item.sources.join(", ");
        let style = if selected {
            Style::default().fg(t.text).bg(t.selection).bold()
        } else {
            Style::default().fg(t.text)
        };
        let dim = if selected {
            Style::default().fg(t.selection).bg(t.selection)
        } else {
            Style::default().fg(t.muted)
        };
        text.push(Line::from(vec![
            Span::styled(format!("  {}{}", marker, item.name), style),
            Span::styled(format!(" ({}) ", label), dim),
            Span::styled(format!("[{}]", sources), dim),
        ]));
    }
    text.push(Line::from(""));
    text.push(Line::from(vec![
        Span::styled("  j/k", t.key_hint()),
        Span::styled(" navigate  ", Style::default().fg(t.muted)),
        Span::styled("Enter", t.key_hint()),
        Span::styled(" install  ", Style::default().fg(t.muted)),
        Span::styled("Esc", t.key_hint()),
        Span::styled(" close", Style::default().fg(t.muted)),
    ]));

    let paragraph = Paragraph::new(text).block(
        Block::default()
            .title(title)
            .borders(Borders::ALL)
            .border_style(Style::default().fg(t.accent)),
    );
    f.render_widget(paragraph, popup_area);
}

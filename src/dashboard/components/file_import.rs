use super::{centered, clamp_cursor, cursor_down};
use crate::dashboard::app::{App, Overlay};
use crate::dashboard::config_edit;
use crate::dashboard::msg::Cmd;
use crate::dashboard::theme::Theme;
use crossterm::event::{KeyCode, KeyEvent};
use ratatui::{prelude::*, widgets::*};

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
    if !config_edit::add_profile_dotfile(config, &ss.machine_id, path) {
        app.flash_error("import failed");
        return;
    }
    if let Ok(mut sync_state) = crate::sync::SyncState::load() {
        sync_state.dismissed_imports.remove(path);
        let _ = sync_state.save();
    }
    app.flash_success(format!("imported {}", path));
    app.reload_state();
}

pub fn render(f: &mut Frame, picker: &FileImport, t: &Theme) {
    let area = f.area();
    let title = " Import file from profile ";
    let max_item_len = picker
        .items
        .iter()
        .map(|i| i.path.len() + i.source_profile.len() + 3)
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
            Span::styled(format!("  {}{}", marker, item.path), style),
            Span::styled(format!(" [{}]", item.source_profile), dim),
        ]));
    }
    text.push(Line::from(""));
    text.push(Line::from(vec![
        Span::styled("  j/k", t.key_hint()),
        Span::styled(" navigate  ", Style::default().fg(t.muted)),
        Span::styled("Enter", t.key_hint()),
        Span::styled(" import  ", Style::default().fg(t.muted)),
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

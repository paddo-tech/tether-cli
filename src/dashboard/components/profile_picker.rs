use super::{centered, cursor_down};
use crate::dashboard::app::{App, Overlay};
use crate::dashboard::msg::Cmd;
use crate::dashboard::theme::Theme;
use crossterm::event::{KeyCode, KeyEvent};
use ratatui::{prelude::*, widgets::*};

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

pub fn render(f: &mut Frame, picker: &ProfilePicker, t: &Theme) {
    let area = f.area();
    let title = " Profile (this machine) ";
    let hint = "  New: tether machines profile create <name>";
    let max_option_len = picker.options.iter().map(|o| o.len()).max().unwrap_or(10);
    let min_width = (max_option_len + 10)
        .max(title.len() + 2)
        .max(hint.len() + 4);
    let width = (min_width as u16).min(area.width.saturating_sub(4));
    let height = ((picker.options.len() + 5) as u16).min(area.height.saturating_sub(2));
    let popup_area = centered(area, width, height);

    f.render_widget(Clear, popup_area);

    let mut text = vec![Line::from("")];
    for (i, option) in picker.options.iter().enumerate() {
        let marker = if i == picker.cursor { "> " } else { "  " };
        let style = if i == picker.cursor {
            Style::default().fg(t.text).bg(t.selection).bold()
        } else {
            Style::default().fg(t.text)
        };
        text.push(Line::from(Span::styled(
            format!("  {}{}", marker, option),
            style,
        )));
    }
    text.push(Line::from(""));
    text.push(Line::from(vec![
        Span::styled("  j/k", t.key_hint()),
        Span::styled(" navigate  ", Style::default().fg(t.muted)),
        Span::styled("Enter", t.key_hint()),
        Span::styled(" select  ", Style::default().fg(t.muted)),
        Span::styled("Esc", t.key_hint()),
        Span::styled(" cancel", Style::default().fg(t.muted)),
    ]));
    text.push(Line::from(Span::styled(hint, Style::default().fg(t.muted))));

    let paragraph = Paragraph::new(text).block(
        Block::default()
            .title(title)
            .borders(Borders::ALL)
            .border_style(Style::default().fg(t.accent)),
    );
    f.render_widget(paragraph, popup_area);
}

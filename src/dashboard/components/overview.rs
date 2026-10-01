use super::{activity, cursor_down, files, machines, packages};
use crate::dashboard::app::App;
use crate::dashboard::msg::KeyOutcome;
use crossterm::event::{KeyCode, KeyEvent};
use ratatui::prelude::*;

pub fn handle_key(app: &mut App, key: KeyEvent) -> KeyOutcome {
    match key.code {
        KeyCode::Char('j') | KeyCode::Down => {
            let len = files::build_overview_rows(&app.state).len();
            cursor_down(&mut app.overview_scroll, len);
        }
        KeyCode::Char('k') | KeyCode::Up => {
            app.overview_scroll = app.overview_scroll.saturating_sub(1);
        }
        _ => return KeyOutcome::Ignored,
    }
    KeyOutcome::Handled(None)
}

pub fn render(f: &mut Frame, area: Rect, app: &App) {
    let content_chunks = Layout::vertical([
        Constraint::Percentage(40),
        Constraint::Percentage(30),
        Constraint::Percentage(30),
    ])
    .split(area);

    let top_chunks = Layout::horizontal([Constraint::Percentage(50), Constraint::Percentage(50)])
        .split(content_chunks[0]);

    files::render_overview(f, top_chunks[0], app);
    packages::render_overview(f, top_chunks[1], app);
    machines::render_overview(f, content_chunks[1], app);
    activity::render(f, content_chunks[2], &app.state.activity_lines, &app.theme);
}

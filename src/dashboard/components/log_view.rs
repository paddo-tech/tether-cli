//! Read-only view of the daemon log, newest at the bottom.

use super::activity::{line_color, strip_ansi};
use super::{centered, popup, scrollbar};
use crate::dashboard::app::{App, Overlay};
use crate::dashboard::msg::Cmd;
use crate::dashboard::state::DashboardState;
use crossterm::event::{KeyCode, KeyEvent};
use ratatui::{prelude::*, widgets::Paragraph};
use std::cell::Cell;

/// Bytes read from the end of the log, so a large log opens fast.
const TAIL_BYTES: u64 = 512 * 1024;

pub struct LogView {
    pub lines: Vec<String>,
    /// Lines between the last line shown and the end of the log
    pub from_end: usize,
    /// Lines the last draw had room for
    pub rows: Cell<usize>,
}

impl LogView {
    /// The log at `path`; no lines without one.
    pub fn open(path: Option<&std::path::Path>) -> Self {
        Self {
            lines: path
                .map(|p| DashboardState::read_log_tail(p, TAIL_BYTES, usize::MAX))
                .unwrap_or_default()
                .iter()
                .map(|l| strip_ansi(l))
                .collect(),
            from_end: 0,
            rows: Cell::new(1),
        }
    }
}

/// The overlay was popped off the stack; push it back to keep it open.
pub fn handle_key(app: &mut App, mut view: LogView, key: KeyEvent) -> Option<Cmd> {
    let page = view.rows.get().max(1);
    let last = view.lines.len().saturating_sub(page);
    view.from_end = match key.code {
        KeyCode::Esc => return None,
        KeyCode::Char('k') | KeyCode::Up => view.from_end + 1,
        KeyCode::Char('j') | KeyCode::Down => view.from_end.saturating_sub(1),
        KeyCode::PageUp => view.from_end + page,
        KeyCode::PageDown => view.from_end.saturating_sub(page),
        KeyCode::Char('g') | KeyCode::Home => last,
        KeyCode::Char('G') | KeyCode::End => 0,
        _ => view.from_end,
    }
    .min(last);
    app.overlays.push(Overlay::Log(view));
    None
}

pub fn render(f: &mut Frame, app: &App, view: &LogView) {
    let t = &app.theme;
    let area = f.area();
    let rect = centered(
        area,
        area.width.saturating_sub(4),
        area.height.saturating_sub(2),
    );
    let hints = Line::from(vec![
        Span::styled(" j/k", t.key_hint()),
        Span::styled(" scroll  ", Style::default().fg(t.muted)),
        Span::styled("g/G", t.key_hint()),
        Span::styled(" top/end  ", Style::default().fg(t.muted)),
        Span::styled("esc", t.key_hint()),
        Span::styled(" close ", Style::default().fg(t.muted)),
    ]);
    let block = popup(f, rect, "Daemon log", t.accent, t)
        .title_top(
            Line::from(Span::styled(
                format!(" {} lines, read-only ", view.lines.len()),
                Style::default().fg(t.dim),
            ))
            .right_aligned(),
        )
        .title_bottom(hints.right_aligned());
    let inner = block.inner(rect);
    f.render_widget(block, rect);
    let rows = inner.height as usize;
    view.rows.set(rows.max(1));
    if view.lines.is_empty() {
        f.render_widget(
            Paragraph::new(Span::styled(
                "The daemon log is empty",
                Style::default().fg(t.dim),
            )),
            inner,
        );
        return;
    }
    let end = view.lines.len() - view.from_end.min(view.lines.len());
    let start = end.saturating_sub(rows);
    let lines: Vec<Line> = view.lines[start..end]
        .iter()
        .map(|l| {
            Line::from(Span::styled(
                l.as_str(),
                Style::default().fg(line_color(l, t)),
            ))
        })
        .collect();
    f.render_widget(Paragraph::new(lines), inner);
    scrollbar(f, rect, view.lines.len(), start, rows, t);
}

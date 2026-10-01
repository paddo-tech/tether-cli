pub mod activity;
pub mod config;
pub mod confirm;
pub mod diff;
pub mod file_import;
pub mod files;
pub mod header;
pub mod help;
pub mod machines;
pub mod overview;
pub mod packages;
pub mod palette;
pub mod pkg_import;
pub mod profile_picker;
pub mod security;
pub mod sparkline;
pub mod tabs;
pub mod toast;

use crate::dashboard::app::{App, Hit};
use crate::dashboard::theme::{mix, Theme};
use ratatui::{
    prelude::*,
    widgets::{
        Block, BorderType, Borders, Clear, Padding, Paragraph, Scrollbar, ScrollbarOrientation,
        ScrollbarState,
    },
};

/// Display label for a package manager key
pub fn manager_label(key: &str) -> &str {
    match key {
        "brew_formulae" => "Brew formulae",
        "brew_casks" => "Brew casks",
        "brew_taps" => "Brew taps",
        "npm" => "npm",
        "pnpm" => "pnpm",
        "bun" => "Bun",
        "gem" => "Gem",
        "uv" => "uv",
        _ => key,
    }
}

/// Centered popup area clamped to the frame.
pub fn centered(area: Rect, width: u16, height: u16) -> Rect {
    let width = width.min(area.width);
    let height = height.min(area.height);
    let x = area.x + (area.width - width) / 2;
    let y = area.y + (area.height - height) / 2;
    Rect::new(x, y, width, height)
}

/// Move a list cursor down by one, stopping at the last row.
pub fn cursor_down(cursor: &mut usize, len: usize) {
    if *cursor < len.saturating_sub(1) {
        *cursor += 1;
    }
}

/// Clamp a list cursor after the row count shrinks.
pub fn clamp_cursor(cursor: &mut usize, len: usize) {
    if *cursor >= len {
        *cursor = len.saturating_sub(1);
    }
}

/// First visible row that keeps the cursor on screen.
pub fn scroll_for(cursor: usize, visible: usize) -> usize {
    (cursor + 1).saturating_sub(visible.max(1))
}

/// A rounded panel. The focused panel has the accent border.
pub fn panel<'a>(title: impl Into<Line<'a>>, focused: bool, t: &Theme) -> Block<'a> {
    let (border, title_style) = if focused {
        (t.border_focus, Style::default().fg(t.accent).bold())
    } else {
        (t.border, Style::default().fg(t.text).bold())
    };
    let title: Line = title.into();
    Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(border))
        .title(title.style(title_style))
        .padding(Padding::horizontal(1))
        .style(Style::default().bg(t.base_bg))
}

/// Clear a popup's area and return its rounded, raised block.
pub fn popup<'a>(f: &mut Frame, area: Rect, title: &'a str, border: Color, t: &Theme) -> Block<'a> {
    f.render_widget(Clear, area);
    Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(border))
        .title(Line::from(format!(" {} ", title)).style(Style::default().fg(border).bold()))
        .padding(Padding::horizontal(1))
        .style(Style::default().bg(t.popup_bg).fg(t.text))
}

/// Dim everything already drawn, so a modal popup stands out.
pub fn backdrop(f: &mut Frame, t: &Theme) {
    let area = f.area();
    let buf = f.buffer_mut();
    for y in area.top()..area.bottom() {
        for x in area.left()..area.right() {
            let cell = &mut buf[(x, y)];
            if t.rgb {
                let fg = if cell.fg == Color::Reset {
                    t.text
                } else {
                    cell.fg
                };
                let bg = if cell.bg == Color::Reset {
                    t.base_bg
                } else {
                    cell.bg
                };
                cell.fg = mix(fg, t.base_bg, 0.6);
                cell.bg = mix(bg, t.popup_bg, 0.5);
            } else {
                cell.modifier.insert(Modifier::DIM);
            }
        }
    }
}

/// Paint a selected row: raised background and an accent bar in the left padding.
pub fn select_row(f: &mut Frame, row: Rect, t: &Theme) {
    f.buffer_mut()
        .set_style(row, Style::default().bg(t.selection));
    if row.x > 0 {
        let bar = Rect::new(row.x - 1, row.y, 1, 1);
        f.render_widget(
            Paragraph::new("▌").style(Style::default().fg(t.accent).bg(t.selection)),
            bar,
        );
    }
}

/// One list row: `left` truncated so `right` stays visible at the right edge.
pub fn row(f: &mut Frame, area: Rect, left: Line, right: Line) {
    let rw = (right.width() as u16).min(area.width);
    let lw = area.width.saturating_sub(if rw > 0 { rw + 1 } else { 0 });
    f.render_widget(Paragraph::new(left), Rect { width: lw, ..area });
    if rw > 0 {
        f.render_widget(
            Paragraph::new(right),
            Rect::new(area.right() - rw, area.y, rw, 1),
        );
    }
}

/// Draw a list's rows, selection and scrollbar inside a panel; records row hits.
pub fn list<T>(
    f: &mut Frame,
    app: &App,
    panel_area: Rect,
    inner: Rect,
    rows: &[T],
    cursor: usize,
    mut draw: impl FnMut(&mut Frame, Rect, &T, bool),
) {
    let t = &app.theme;
    let visible = inner.height as usize;
    let scroll = scroll_for(cursor, visible);
    for (i, item) in rows.iter().enumerate().skip(scroll).take(visible) {
        let y = inner.y + (i - scroll) as u16;
        let area = Rect::new(inner.x, y, inner.width, 1);
        let selected = i == cursor;
        if selected {
            select_row(f, area, t);
        }
        draw(f, area, item, selected);
        app.add_hit(area, Hit::Row(i));
    }
    scrollbar(f, panel_area, rows.len(), scroll, visible, t);
}

/// Scrollbar on a panel's right border, shown only when the content overflows.
pub fn scrollbar(
    f: &mut Frame,
    panel_area: Rect,
    total: usize,
    offset: usize,
    visible: usize,
    t: &Theme,
) {
    if total <= visible || panel_area.height < 3 {
        return;
    }
    let mut state = ScrollbarState::new(total.saturating_sub(visible))
        .position(offset)
        .viewport_content_length(visible);
    let bar = Scrollbar::new(ScrollbarOrientation::VerticalRight)
        .begin_symbol(None)
        .end_symbol(None)
        .track_symbol(Some("│"))
        .thumb_symbol("┃")
        .track_style(Style::default().fg(t.border))
        .thumb_style(Style::default().fg(t.accent));
    f.render_stateful_widget(
        bar,
        panel_area.inner(Margin {
            vertical: 1,
            horizontal: 0,
        }),
        &mut state,
    );
}

/// A popup list: rows of (left, right) lines, a cursor, and key hints in the bottom border.
pub fn picker(
    f: &mut Frame,
    app: &App,
    title: &str,
    rows: Vec<(Line, Line)>,
    cursor: usize,
    hints: &[(&str, &str)],
    note: Option<&str>,
) {
    let t = &app.theme;
    let area = f.area();
    let content_w = rows
        .iter()
        .map(|(l, r)| l.width() + r.width() + 2)
        .chain(note.map(|n| n.chars().count()))
        .max()
        .unwrap_or(20)
        .max(title.len() + 4)
        .max(40);
    let width = (content_w as u16 + 6).min(area.width.saturating_sub(4));
    let note_h = if note.is_some() { 2 } else { 0 };
    let visible = rows.len().min(15);
    let height = (visible as u16 + 4 + note_h).min(area.height.saturating_sub(2));
    let rect = centered(area, width, height);
    let hint_line = Line::from(
        hints
            .iter()
            .flat_map(|(k, d)| {
                [
                    Span::styled(format!(" {}", k), t.key_hint()),
                    Span::styled(format!(" {} ", d), Style::default().fg(t.muted)),
                ]
            })
            .collect::<Vec<_>>(),
    );
    let block = popup(f, rect, title, t.accent, t).title_bottom(hint_line.right_aligned());
    let inner = block.inner(rect);
    f.render_widget(block, rect);
    let list_area = Rect {
        y: inner.y + 1,
        height: inner.height.saturating_sub(1 + note_h),
        ..inner
    };
    let visible = list_area.height as usize;
    let scroll = scroll_for(cursor, visible);
    for (i, (left, right)) in rows.into_iter().enumerate().skip(scroll).take(visible) {
        let r = Rect::new(
            list_area.x,
            list_area.y + (i - scroll) as u16,
            list_area.width,
            1,
        );
        if i == cursor {
            select_row(f, r, t);
        }
        row(f, r, left, right);
        app.add_hit(r, Hit::Item(i));
    }
    if let Some(note) = note {
        f.render_widget(
            Paragraph::new(Span::styled(note, Style::default().fg(t.dim))),
            Rect::new(inner.x, inner.bottom().saturating_sub(1), inner.width, 1),
        );
    }
}

/// Shorten to `max` display columns with an ellipsis.
pub fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let mut out: String = s.chars().take(max.saturating_sub(1)).collect();
    out.push('…');
    out
}

/// Braille spinner frame for an animation clock in milliseconds.
pub fn spinner(ms: u128) -> &'static str {
    const FRAMES: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
    FRAMES[(ms / 80) as usize % FRAMES.len()]
}

/// 0..1..0 over `period_ms`, for pulsing colors.
pub fn pulse(ms: u128, period_ms: u128) -> f32 {
    let phase = (ms % period_ms) as f32 / period_ms as f32;
    0.5 - 0.5 * (phase * std::f32::consts::TAU).cos()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scroll_keeps_cursor_visible() {
        assert_eq!(scroll_for(0, 10), 0);
        assert_eq!(scroll_for(9, 10), 0);
        assert_eq!(scroll_for(10, 10), 1);
        assert_eq!(scroll_for(5, 0), 5);
    }

    #[test]
    fn truncate_adds_ellipsis() {
        assert_eq!(truncate("abcdef", 4), "abc…");
        assert_eq!(truncate("abc", 4), "abc");
    }

    #[test]
    fn centered_clamps_to_area() {
        let r = centered(Rect::new(0, 0, 20, 10), 40, 4);
        assert_eq!(r, Rect::new(0, 3, 20, 4));
    }
}

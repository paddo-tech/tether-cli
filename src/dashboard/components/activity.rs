use crate::dashboard::theme::Theme;
use ratatui::{prelude::*, widgets::*};

pub fn render(f: &mut Frame, area: Rect, lines: &[String], t: &Theme) {
    let text = if lines.is_empty() {
        Text::from(Span::styled("  No activity", Style::default().fg(t.muted)))
    } else {
        Text::from(
            lines
                .iter()
                .map(|l| Line::from(Span::styled(l.as_str(), Style::default().fg(t.muted))))
                .collect::<Vec<_>>(),
        )
    };

    let paragraph = Paragraph::new(text).wrap(Wrap { trim: false }).block(
        Block::default()
            .title(" Activity ")
            .borders(Borders::ALL)
            .border_style(Style::default().fg(t.border)),
    );
    f.render_widget(paragraph, area);
}

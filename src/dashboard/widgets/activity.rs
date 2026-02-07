use ratatui::{prelude::*, widgets::*};

pub fn render(f: &mut Frame, area: Rect, lines: &[String]) {
    let text = if lines.is_empty() {
        Text::from(Span::styled(
            "  No activity",
            Style::default().fg(Color::DarkGray),
        ))
    } else {
        Text::from(
            lines
                .iter()
                .map(|l| {
                    Line::from(Span::styled(
                        l.as_str(),
                        Style::default().fg(Color::DarkGray),
                    ))
                })
                .collect::<Vec<_>>(),
        )
    };

    let paragraph = Paragraph::new(text).wrap(Wrap { trim: false }).block(
        Block::default()
            .title(" Activity ")
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::DarkGray)),
    );
    f.render_widget(paragraph, area);
}

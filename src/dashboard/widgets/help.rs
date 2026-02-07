use ratatui::{prelude::*, widgets::*};

pub fn render_bar(f: &mut Frame, area: Rect) {
    let spans = vec![
        Span::styled(" q", Style::default().fg(Color::Yellow).bold()),
        Span::styled("uit ", Style::default().fg(Color::DarkGray)),
        Span::styled("s", Style::default().fg(Color::Yellow).bold()),
        Span::styled("ync ", Style::default().fg(Color::DarkGray)),
        Span::styled("d", Style::default().fg(Color::Yellow).bold()),
        Span::styled("aemon ", Style::default().fg(Color::DarkGray)),
        Span::styled("r", Style::default().fg(Color::Yellow).bold()),
        Span::styled("efresh ", Style::default().fg(Color::DarkGray)),
        Span::styled("Tab", Style::default().fg(Color::Yellow).bold()),
        Span::styled(" next tab ", Style::default().fg(Color::DarkGray)),
        Span::styled("1-5", Style::default().fg(Color::Yellow).bold()),
        Span::styled(" tabs ", Style::default().fg(Color::DarkGray)),
        Span::styled("?", Style::default().fg(Color::Yellow).bold()),
        Span::styled(" help", Style::default().fg(Color::DarkGray)),
    ];

    let paragraph = Paragraph::new(Line::from(spans));
    f.render_widget(paragraph, area);
}

pub fn render_overlay(f: &mut Frame) {
    let area = f.area();
    if area.height < 10 || area.width < 30 {
        let hint = Paragraph::new(Span::styled(
            " Press ? to close help ",
            Style::default().fg(Color::Yellow),
        ));
        let y = area.height.saturating_sub(2);
        f.render_widget(hint, Rect::new(0, y, area.width, 1));
        return;
    }

    let width = 50u16.min(area.width.saturating_sub(4));
    let height = 16u16.min(area.height.saturating_sub(4));
    let x = (area.width.saturating_sub(width)) / 2;
    let y = (area.height.saturating_sub(height)) / 2;
    let popup_area = Rect::new(x, y, width, height);

    f.render_widget(Clear, popup_area);

    let help_text = vec![
        Line::from(""),
        Line::from(vec![
            Span::styled("  q / Esc   ", Style::default().fg(Color::Yellow).bold()),
            Span::raw("Quit"),
        ]),
        Line::from(vec![
            Span::styled("  s         ", Style::default().fg(Color::Yellow).bold()),
            Span::raw("Trigger sync"),
        ]),
        Line::from(vec![
            Span::styled("  d         ", Style::default().fg(Color::Yellow).bold()),
            Span::raw("Start/stop daemon"),
        ]),
        Line::from(vec![
            Span::styled("  r         ", Style::default().fg(Color::Yellow).bold()),
            Span::raw("Refresh data"),
        ]),
        Line::from(vec![
            Span::styled("  Tab       ", Style::default().fg(Color::Yellow).bold()),
            Span::raw("Next tab"),
        ]),
        Line::from(vec![
            Span::styled("  1-5       ", Style::default().fg(Color::Yellow).bold()),
            Span::raw("Switch tab"),
        ]),
        Line::from(vec![
            Span::styled("  j/k       ", Style::default().fg(Color::Yellow).bold()),
            Span::raw("Scroll down/up"),
        ]),
        Line::from(vec![
            Span::styled("  Enter     ", Style::default().fg(Color::Yellow).bold()),
            Span::raw("Toggle/edit (Config tab)"),
        ]),
        Line::from(vec![
            Span::styled("  ?         ", Style::default().fg(Color::Yellow).bold()),
            Span::raw("Toggle help"),
        ]),
        Line::from(vec![
            Span::styled("  Ctrl+c    ", Style::default().fg(Color::Yellow).bold()),
            Span::raw("Force quit"),
        ]),
        Line::from(""),
    ];

    let paragraph = Paragraph::new(help_text).block(
        Block::default()
            .title(" Keyboard Shortcuts ")
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::Cyan)),
    );
    f.render_widget(paragraph, popup_area);
}

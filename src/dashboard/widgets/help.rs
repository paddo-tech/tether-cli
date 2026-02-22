use crate::dashboard::Tab;
use ratatui::{prelude::*, widgets::*};

pub fn render_bar(f: &mut Frame, area: Rect, active_tab: Tab) {
    let mut spans = vec![
        Span::styled(" q", Style::default().fg(Color::Yellow).bold()),
        Span::styled("uit ", Style::default().fg(Color::DarkGray)),
        Span::styled("s", Style::default().fg(Color::Yellow).bold()),
        Span::styled("ync ", Style::default().fg(Color::DarkGray)),
        Span::styled("d", Style::default().fg(Color::Yellow).bold()),
        Span::styled("aemon ", Style::default().fg(Color::DarkGray)),
        Span::styled("r", Style::default().fg(Color::Yellow).bold()),
        Span::styled("efresh ", Style::default().fg(Color::DarkGray)),
    ];

    match active_tab {
        Tab::Config => {
            spans.extend([
                Span::styled("Enter", Style::default().fg(Color::Yellow).bold()),
                Span::styled(" edit ", Style::default().fg(Color::DarkGray)),
            ]);
        }
        Tab::Packages => {
            spans.extend([
                Span::styled("Enter", Style::default().fg(Color::Yellow).bold()),
                Span::styled(" expand/uninstall ", Style::default().fg(Color::DarkGray)),
            ]);
        }
        Tab::Machines => {
            spans.extend([
                Span::styled("Enter", Style::default().fg(Color::Yellow).bold()),
                Span::styled(" expand ", Style::default().fg(Color::DarkGray)),
                Span::styled("p", Style::default().fg(Color::Yellow).bold()),
                Span::styled(" profile ", Style::default().fg(Color::DarkGray)),
            ]);
        }
        Tab::Files => {
            spans.extend([
                Span::styled("Enter", Style::default().fg(Color::Yellow).bold()),
                Span::styled(" expand/diff ", Style::default().fg(Color::DarkGray)),
                Span::styled("t", Style::default().fg(Color::Yellow).bold()),
                Span::styled(" shared ", Style::default().fg(Color::DarkGray)),
                Span::styled("R", Style::default().fg(Color::Yellow).bold()),
                Span::styled("estore ", Style::default().fg(Color::DarkGray)),
            ]);
        }
        _ => {}
    }

    spans.extend([
        Span::styled("?", Style::default().fg(Color::Yellow).bold()),
        Span::styled(" help", Style::default().fg(Color::DarkGray)),
    ]);

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
    let height = 29u16.min(area.height.saturating_sub(4));
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
            Span::raw("Expand/edit (context)"),
        ]),
        Line::from(""),
        Line::from(Span::styled(
            "  Files tab:",
            Style::default().fg(Color::Cyan).bold(),
        )),
        Line::from(vec![
            Span::styled("  Enter     ", Style::default().fg(Color::Yellow).bold()),
            Span::raw("Expand section/file/history/diff"),
        ]),
        Line::from(vec![
            Span::styled("  t         ", Style::default().fg(Color::Yellow).bold()),
            Span::raw("Toggle shared across profiles"),
        ]),
        Line::from(vec![
            Span::styled("  R         ", Style::default().fg(Color::Yellow).bold()),
            Span::raw("Restore file to selected commit"),
        ]),
        Line::from(""),
        Line::from(Span::styled(
            "  Config list sub-view:",
            Style::default().fg(Color::Cyan).bold(),
        )),
        Line::from(vec![
            Span::styled("  a         ", Style::default().fg(Color::Yellow).bold()),
            Span::raw("Add item"),
        ]),
        Line::from(vec![
            Span::styled("  d         ", Style::default().fg(Color::Yellow).bold()),
            Span::raw("Delete item"),
        ]),
        Line::from(vec![
            Span::styled("  t         ", Style::default().fg(Color::Yellow).bold()),
            Span::raw("Toggle create (dotfiles)"),
        ]),
        Line::from(""),
        Line::from(Span::styled(
            "  Packages tab:",
            Style::default().fg(Color::Cyan).bold(),
        )),
        Line::from(vec![
            Span::styled("  Enter     ", Style::default().fg(Color::Yellow).bold()),
            Span::raw("Expand/uninstall"),
        ]),
        Line::from(""),
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

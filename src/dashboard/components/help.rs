use crate::dashboard::app::Tab;
use crate::dashboard::theme::Theme;
use ratatui::{prelude::*, widgets::*};

pub fn render_bar(f: &mut Frame, area: Rect, active_tab: Tab, t: &Theme) {
    let mut spans = vec![
        Span::styled(" q", t.key_hint()),
        Span::styled("uit ", Style::default().fg(t.muted)),
        Span::styled("s", t.key_hint()),
        Span::styled("ync ", Style::default().fg(t.muted)),
        Span::styled("d", t.key_hint()),
        Span::styled("aemon ", Style::default().fg(t.muted)),
        Span::styled("r", t.key_hint()),
        Span::styled("efresh ", Style::default().fg(t.muted)),
    ];

    match active_tab {
        Tab::Config => {
            spans.extend([
                Span::styled("Enter", t.key_hint()),
                Span::styled(" edit ", Style::default().fg(t.muted)),
            ]);
        }
        Tab::Packages => {
            spans.extend([
                Span::styled("Enter", t.key_hint()),
                Span::styled(" expand/uninstall ", Style::default().fg(t.muted)),
            ]);
        }
        Tab::Machines => {
            spans.extend([
                Span::styled("Enter", t.key_hint()),
                Span::styled(" expand ", Style::default().fg(t.muted)),
                Span::styled("p", t.key_hint()),
                Span::styled(" profile ", Style::default().fg(t.muted)),
            ]);
        }
        Tab::Files => {
            spans.extend([
                Span::styled("Enter", t.key_hint()),
                Span::styled(" expand/diff ", Style::default().fg(t.muted)),
                Span::styled("t", t.key_hint()),
                Span::styled(" shared ", Style::default().fg(t.muted)),
                Span::styled("R", t.key_hint()),
                Span::styled("estore ", Style::default().fg(t.muted)),
            ]);
        }
        _ => {}
    }

    spans.extend([
        Span::styled("?", t.key_hint()),
        Span::styled(" help", Style::default().fg(t.muted)),
    ]);

    let paragraph = Paragraph::new(Line::from(spans));
    f.render_widget(paragraph, area);
}

pub fn render_overlay(f: &mut Frame, t: &Theme) {
    let area = f.area();
    if area.height < 10 || area.width < 30 {
        let hint = Paragraph::new(Span::styled(
            " Press ? to close help ",
            Style::default().fg(t.key),
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
            Span::styled("  q / Esc   ", t.key_hint()),
            Span::raw("Quit"),
        ]),
        Line::from(vec![
            Span::styled("  s         ", t.key_hint()),
            Span::raw("Trigger sync"),
        ]),
        Line::from(vec![
            Span::styled("  d         ", t.key_hint()),
            Span::raw("Start/stop daemon"),
        ]),
        Line::from(vec![
            Span::styled("  r         ", t.key_hint()),
            Span::raw("Refresh data"),
        ]),
        Line::from(vec![
            Span::styled("  Tab       ", t.key_hint()),
            Span::raw("Next tab"),
        ]),
        Line::from(vec![
            Span::styled("  1-5       ", t.key_hint()),
            Span::raw("Switch tab"),
        ]),
        Line::from(vec![
            Span::styled("  j/k       ", t.key_hint()),
            Span::raw("Scroll down/up"),
        ]),
        Line::from(vec![
            Span::styled("  Enter     ", t.key_hint()),
            Span::raw("Expand/edit (context)"),
        ]),
        Line::from(""),
        Line::from(Span::styled(
            "  Files tab:",
            Style::default().fg(t.accent).bold(),
        )),
        Line::from(vec![
            Span::styled("  Enter     ", t.key_hint()),
            Span::raw("Expand section/file/history/diff"),
        ]),
        Line::from(vec![
            Span::styled("  t         ", t.key_hint()),
            Span::raw("Toggle shared across profiles"),
        ]),
        Line::from(vec![
            Span::styled("  R         ", t.key_hint()),
            Span::raw("Restore file to selected commit"),
        ]),
        Line::from(""),
        Line::from(Span::styled(
            "  Config list sub-view:",
            Style::default().fg(t.accent).bold(),
        )),
        Line::from(vec![
            Span::styled("  a         ", t.key_hint()),
            Span::raw("Add item"),
        ]),
        Line::from(vec![
            Span::styled("  d         ", t.key_hint()),
            Span::raw("Delete item"),
        ]),
        Line::from(vec![
            Span::styled("  t         ", t.key_hint()),
            Span::raw("Toggle create (dotfiles)"),
        ]),
        Line::from(""),
        Line::from(Span::styled(
            "  Packages tab:",
            Style::default().fg(t.accent).bold(),
        )),
        Line::from(vec![
            Span::styled("  Enter     ", t.key_hint()),
            Span::raw("Expand/uninstall, toggle history diff"),
        ]),
        Line::from(vec![
            Span::styled("  h         ", t.key_hint()),
            Span::raw("Toggle manifest history"),
        ]),
        Line::from(vec![
            Span::styled("  R         ", t.key_hint()),
            Span::raw("Roll back to history entry"),
        ]),
        Line::from(""),
        Line::from(vec![
            Span::styled("  ?         ", t.key_hint()),
            Span::raw("Toggle help"),
        ]),
        Line::from(vec![
            Span::styled("  Ctrl+c    ", t.key_hint()),
            Span::raw("Force quit"),
        ]),
        Line::from(""),
    ];

    let paragraph = Paragraph::new(help_text).block(
        Block::default()
            .title(" Keyboard Shortcuts ")
            .borders(Borders::ALL)
            .border_style(Style::default().fg(t.accent)),
    );
    f.render_widget(paragraph, popup_area);
}

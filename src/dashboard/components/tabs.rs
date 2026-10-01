use crate::dashboard::app::{App, Tab};
use ratatui::{prelude::*, widgets::Tabs};

pub fn render(f: &mut Frame, area: Rect, app: &App) {
    let t = &app.theme;
    let titles: Vec<Line> = Tab::all()
        .iter()
        .enumerate()
        .map(|(i, tab)| {
            let num = format!("{}", i + 1);
            if *tab == app.active_tab {
                Line::from(vec![
                    Span::styled(num, Style::default().fg(t.key).bold()),
                    Span::raw(":"),
                    Span::styled(tab.title(), Style::default().fg(t.text).bold()),
                ])
            } else {
                Line::from(vec![
                    Span::styled(num, Style::default().fg(t.muted)),
                    Span::raw(":"),
                    Span::styled(tab.title(), Style::default().fg(t.muted)),
                ])
            }
        })
        .collect();

    let tabs = Tabs::new(titles)
        .divider(Span::styled(" | ", Style::default().fg(t.muted)))
        .select(
            Tab::all()
                .iter()
                .position(|tab| *tab == app.active_tab)
                .unwrap_or(0),
        );
    f.render_widget(tabs, area);
}

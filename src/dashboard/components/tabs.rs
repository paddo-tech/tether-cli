use crate::dashboard::app::{App, Hit, Tab};
use ratatui::prelude::*;

/// Tab bar of pills. Narrow terminals drop inactive titles and keep their numbers.
pub fn render(f: &mut Frame, area: Rect, app: &App) {
    let t = &app.theme;
    let tabs = Tab::all();
    let full: u16 = tabs.iter().map(|tab| tab.title().len() as u16 + 5).sum();
    let compact = full > area.width;

    let mut x = area.x;
    for (i, tab) in tabs.iter().enumerate() {
        let active = *tab == app.active_tab;
        let label = if compact && !active {
            format!(" {} ", i + 1)
        } else {
            format!(" {} {} ", i + 1, tab.title())
        };
        let w = label.chars().count() as u16;
        if x + w > area.right() {
            break;
        }
        let rect = Rect::new(x, area.y, w, 1);
        let line = if active {
            Line::from(vec![
                Span::styled(
                    format!(" {} ", i + 1),
                    Style::default().fg(t.brand_fg).bg(t.accent).bold(),
                ),
                Span::styled(
                    format!("{} ", tab.title()),
                    Style::default().fg(t.brand_fg).bg(t.accent).bold(),
                ),
            ])
        } else if compact {
            Line::from(Span::styled(label, Style::default().fg(t.muted)))
        } else {
            Line::from(vec![
                Span::styled(format!(" {} ", i + 1), Style::default().fg(t.dim)),
                Span::styled(format!("{} ", tab.title()), Style::default().fg(t.muted)),
            ])
        };
        f.render_widget(line, rect);
        app.add_hit(rect, Hit::Tab(*tab));
        x += w + 1;
    }
}

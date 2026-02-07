use crate::dashboard::state::DashboardState;
use ratatui::{prelude::*, widgets::*};

pub fn render(f: &mut Frame, area: Rect, state: &DashboardState) {
    let items: Vec<ListItem> = match &state.sync_state {
        Some(sync_state) => {
            let mut managers: Vec<_> = sync_state.packages.iter().collect();
            managers.sort_by(|a, b| a.0.cmp(b.0));

            if managers.is_empty() {
                vec![ListItem::new(Span::styled(
                    "  No packages tracked",
                    Style::default().fg(Color::DarkGray),
                ))]
            } else {
                managers
                    .into_iter()
                    .map(|(name, pkg_state)| {
                        let badge =
                            Span::styled(format!(" {} ", name), Style::default().fg(Color::Cyan));
                        let hash_preview = if pkg_state.hash.len() >= 8 {
                            &pkg_state.hash[..8]
                        } else {
                            &pkg_state.hash
                        };
                        ListItem::new(Line::from(vec![
                            badge,
                            Span::raw("  "),
                            Span::styled(
                                format!("#{}", hash_preview),
                                Style::default().fg(Color::DarkGray),
                            ),
                        ]))
                    })
                    .collect()
            }
        }
        None => vec![ListItem::new(Span::styled(
            "  No sync state",
            Style::default().fg(Color::DarkGray),
        ))],
    };

    let list = List::new(items).block(
        Block::default()
            .title(" Packages ")
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::DarkGray)),
    );
    f.render_widget(list, area);
}

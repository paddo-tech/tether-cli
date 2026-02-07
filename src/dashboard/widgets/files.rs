use crate::cli::output::relative_time;
use crate::dashboard::state::DashboardState;
use ratatui::{prelude::*, widgets::*};

pub fn render(f: &mut Frame, area: Rect, state: &DashboardState, scroll_offset: usize) {
    let items: Vec<ListItem> = match &state.sync_state {
        Some(sync_state) => {
            let mut files: Vec<_> = sync_state.files.iter().collect();
            files.sort_by(|a, b| a.0.cmp(b.0));

            files
                .into_iter()
                .skip(scroll_offset)
                .map(|(path, file_state)| {
                    let badge = if file_state.synced {
                        Span::styled(" ok ", Style::default().fg(Color::Green))
                    } else {
                        Span::styled(" ** ", Style::default().fg(Color::Yellow))
                    };
                    let time = relative_time(file_state.last_modified);
                    ListItem::new(Line::from(vec![
                        badge,
                        Span::raw(" "),
                        Span::styled(path.clone(), Style::default().fg(Color::White)),
                        Span::raw("  "),
                        Span::styled(time, Style::default().fg(Color::DarkGray)),
                    ]))
                })
                .collect()
        }
        None => vec![ListItem::new(Span::styled(
            "  No sync state",
            Style::default().fg(Color::DarkGray),
        ))],
    };

    let list = List::new(items).block(
        Block::default()
            .title(" Dotfiles ")
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::DarkGray)),
    );
    f.render_widget(list, area);
}

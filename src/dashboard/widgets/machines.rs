use crate::cli::output::relative_time;
use crate::dashboard::state::DashboardState;
use ratatui::{prelude::*, widgets::*};

pub fn render(f: &mut Frame, area: Rect, state: &DashboardState) {
    let items: Vec<ListItem> = if state.machines.is_empty() {
        vec![ListItem::new(Span::styled(
            "  No machines found",
            Style::default().fg(Color::DarkGray),
        ))]
    } else {
        state
            .machines
            .iter()
            .map(|m| {
                let is_current = state
                    .sync_state
                    .as_ref()
                    .map(|s| s.machine_id == m.machine_id)
                    .unwrap_or(false);

                let marker = if is_current {
                    Span::styled(" * ", Style::default().fg(Color::Green).bold())
                } else {
                    Span::styled("   ", Style::default())
                };

                let name = Span::styled(
                    &m.machine_id,
                    if is_current {
                        Style::default().fg(Color::White).bold()
                    } else {
                        Style::default().fg(Color::White)
                    },
                );

                let time = relative_time(m.last_sync);
                let file_count = m.files.len();
                let pkg_count: usize = m.packages.values().map(|v| v.len()).sum();

                ListItem::new(Line::from(vec![
                    marker,
                    name,
                    Span::raw("  "),
                    Span::styled(
                        format!("{}f {}p", file_count, pkg_count),
                        Style::default().fg(Color::DarkGray),
                    ),
                    Span::raw("  "),
                    Span::styled(time, Style::default().fg(Color::DarkGray)),
                ]))
            })
            .collect()
    };

    let list = List::new(items).block(
        Block::default()
            .title(" Machines ")
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::DarkGray)),
    );
    f.render_widget(list, area);
}

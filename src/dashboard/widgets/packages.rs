use super::manager_label;
use crate::dashboard::state::DashboardState;
use ratatui::{prelude::*, widgets::*};

/// Row in the flat package list
pub enum PkgRow {
    Header {
        manager_key: String,
        label: String,
        count: usize,
    },
    Package {
        manager_key: String,
        name: String,
    },
}

/// Build the flat list of rows from machine state
pub fn build_rows(state: &DashboardState, expanded: Option<&str>) -> Vec<PkgRow> {
    let current_machine_id = state
        .sync_state
        .as_ref()
        .map(|s| s.machine_id.as_str())
        .unwrap_or("");

    let machine = state
        .machines
        .iter()
        .find(|m| m.machine_id == current_machine_id);

    let Some(machine) = machine else {
        return Vec::new();
    };

    let mut managers: Vec<_> = machine.packages.iter().collect();
    managers.sort_by(|a, b| a.0.cmp(b.0));

    let mut rows = Vec::new();
    for (key, packages) in &managers {
        rows.push(PkgRow::Header {
            manager_key: (*key).clone(),
            label: manager_label(key).to_string(),
            count: packages.len(),
        });
        if expanded == Some(key.as_str()) {
            let mut sorted_pkgs: Vec<_> = (*packages).clone();
            sorted_pkgs.sort();
            for pkg in &sorted_pkgs {
                rows.push(PkgRow::Package {
                    manager_key: (*key).clone(),
                    name: pkg.clone(),
                });
            }
        }
    }
    rows
}

pub fn render(
    f: &mut Frame,
    area: Rect,
    state: &DashboardState,
    expanded: Option<&str>,
    cursor: usize,
) {
    let rows = build_rows(state, expanded);

    let block = Block::default()
        .title(" Packages ")
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::DarkGray));
    let inner_area = block.inner(area);
    f.render_widget(block, area);

    if rows.is_empty() {
        let msg = Paragraph::new(Span::styled(
            "  No package data for this machine",
            Style::default().fg(Color::DarkGray),
        ));
        f.render_widget(msg, inner_area);
        return;
    }

    let visible_height = inner_area.height as usize;
    let scroll = if cursor >= visible_height {
        cursor - visible_height + 1
    } else {
        0
    };

    let mut y = inner_area.y;
    for (row_idx, row) in rows.iter().enumerate().skip(scroll) {
        if y >= inner_area.y + inner_area.height {
            break;
        }

        let is_selected = row_idx == cursor;
        let row_area = Rect::new(inner_area.x, y, inner_area.width, 1);

        match row {
            PkgRow::Header {
                manager_key,
                label,
                count,
                ..
            } => {
                let arrow = if expanded == Some(manager_key.as_str()) {
                    "v"
                } else {
                    ">"
                };
                let style = if is_selected {
                    Style::default().fg(Color::Cyan).bg(Color::DarkGray).bold()
                } else {
                    Style::default().fg(Color::Cyan).bold()
                };
                let bg_style = if is_selected {
                    Style::default().bg(Color::DarkGray)
                } else {
                    Style::default()
                };
                let line = Line::from(vec![
                    Span::styled(format!("  {} {} ", arrow, label), style),
                    Span::styled(
                        format!("({})", count),
                        if is_selected {
                            Style::default().fg(Color::DarkGray).bg(Color::DarkGray)
                        } else {
                            Style::default().fg(Color::DarkGray)
                        },
                    ),
                    Span::styled(" ".repeat(inner_area.width as usize), bg_style),
                ]);
                f.render_widget(Paragraph::new(line), row_area);
            }
            PkgRow::Package { name, .. } => {
                let style = if is_selected {
                    Style::default().fg(Color::White).bg(Color::DarkGray)
                } else {
                    Style::default().fg(Color::White)
                };
                let line = Line::from(vec![
                    Span::styled(format!("      {}", name), style),
                    Span::styled(" ".repeat(inner_area.width as usize), style),
                ]);
                f.render_widget(Paragraph::new(line), row_area);
            }
        }

        y += 1;
    }
}

/// Simple overview render (for the Overview tab) - shows manager summary
pub fn render_overview(f: &mut Frame, area: Rect, state: &DashboardState) {
    let current_machine_id = state
        .sync_state
        .as_ref()
        .map(|s| s.machine_id.as_str())
        .unwrap_or("");

    let machine = state
        .machines
        .iter()
        .find(|m| m.machine_id == current_machine_id);

    let items: Vec<ListItem> = match machine {
        Some(machine) => {
            let mut managers: Vec<_> = machine.packages.iter().collect();
            managers.sort_by(|a, b| a.0.cmp(b.0));

            if managers.is_empty() {
                vec![ListItem::new(Span::styled(
                    "  No packages tracked",
                    Style::default().fg(Color::DarkGray),
                ))]
            } else {
                managers
                    .into_iter()
                    .map(|(key, packages)| {
                        let label = manager_label(key);
                        ListItem::new(Line::from(vec![
                            Span::styled(format!(" {} ", label), Style::default().fg(Color::Cyan)),
                            Span::raw("  "),
                            Span::styled(
                                format!("{} packages", packages.len()),
                                Style::default().fg(Color::DarkGray),
                            ),
                        ]))
                    })
                    .collect()
            }
        }
        None => vec![ListItem::new(Span::styled(
            "  No package data",
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

use super::manager_label;
use crate::cli::output::relative_time;
use crate::dashboard::state::DashboardState;
use ratatui::{prelude::*, widgets::*};

/// Row in the flat machine list
pub enum MachineRow {
    Header {
        machine_id: String,
        is_current: bool,
        file_count: usize,
        pkg_count: usize,
        last_sync: String,
    },
    Detail {
        label: String,
        value: String,
    },
}

/// Build the flat list of rows from dashboard state
pub fn build_rows(state: &DashboardState, expanded: Option<&str>) -> Vec<MachineRow> {
    let current_machine_id = state
        .sync_state
        .as_ref()
        .map(|s| s.machine_id.as_str())
        .unwrap_or("");

    let mut rows = Vec::new();
    for m in &state.machines {
        let is_current = m.machine_id == current_machine_id;
        let file_count = m.files.len();
        let pkg_count: usize = m.packages.values().map(|v| v.len()).sum();

        rows.push(MachineRow::Header {
            machine_id: m.machine_id.clone(),
            is_current,
            file_count,
            pkg_count,
            last_sync: relative_time(m.last_sync),
        });

        if expanded == Some(m.machine_id.as_str()) {
            rows.push(MachineRow::Detail {
                label: "Hostname".to_string(),
                value: m.hostname.clone(),
            });
            if !m.os_version.is_empty() {
                rows.push(MachineRow::Detail {
                    label: "OS".to_string(),
                    value: m.os_version.clone(),
                });
            }
            if !m.dotfiles.is_empty() {
                for (i, dotfile) in m.dotfiles.iter().enumerate() {
                    rows.push(MachineRow::Detail {
                        label: if i == 0 {
                            "Dotfiles".to_string()
                        } else {
                            String::new()
                        },
                        value: dotfile.clone(),
                    });
                }
            }
            let mut managers: Vec<_> = m.packages.iter().collect();
            managers.sort_by(|a, b| a.0.cmp(b.0));
            for (key, packages) in &managers {
                rows.push(MachineRow::Detail {
                    label: manager_label(key).to_string(),
                    value: packages.len().to_string(),
                });
            }
            rows.push(MachineRow::Detail {
                label: "Files".to_string(),
                value: file_count.to_string(),
            });
            rows.push(MachineRow::Detail {
                label: "Last sync".to_string(),
                value: relative_time(m.last_sync),
            });
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
        .title(" Machines ")
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::DarkGray));
    let inner_area = block.inner(area);
    f.render_widget(block, area);

    if rows.is_empty() {
        let msg = Paragraph::new(Span::styled(
            "  No machines found",
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
            MachineRow::Header {
                machine_id,
                is_current,
                file_count,
                pkg_count,
                last_sync,
                ..
            } => {
                let is_expanded = expanded == Some(machine_id.as_str());
                let arrow = if is_expanded { "v" } else { ">" };
                let marker = if *is_current { "* " } else { "  " };

                let name_style = if is_selected {
                    if *is_current {
                        Style::default().fg(Color::White).bg(Color::DarkGray).bold()
                    } else {
                        Style::default().fg(Color::White).bg(Color::DarkGray)
                    }
                } else if *is_current {
                    Style::default().fg(Color::White).bold()
                } else {
                    Style::default().fg(Color::White)
                };

                let bg_style = if is_selected {
                    Style::default().bg(Color::DarkGray)
                } else {
                    Style::default()
                };

                let marker_style = if *is_current {
                    if is_selected {
                        Style::default().fg(Color::Green).bg(Color::DarkGray).bold()
                    } else {
                        Style::default().fg(Color::Green).bold()
                    }
                } else {
                    bg_style
                };

                let dim_style = if is_selected {
                    Style::default().fg(Color::DarkGray).bg(Color::DarkGray)
                } else {
                    Style::default().fg(Color::DarkGray)
                };

                let line = Line::from(vec![
                    Span::styled(format!("  {} ", arrow), name_style),
                    Span::styled(marker, marker_style),
                    Span::styled(machine_id, name_style),
                    Span::styled(format!("  {}f {}p", file_count, pkg_count), dim_style),
                    Span::styled(format!("  {}", last_sync), dim_style),
                    Span::styled(" ".repeat(inner_area.width as usize), bg_style),
                ]);
                f.render_widget(Paragraph::new(line), row_area);
            }
            MachineRow::Detail { label, value } => {
                let style = if is_selected {
                    Style::default().fg(Color::White).bg(Color::DarkGray)
                } else {
                    Style::default().fg(Color::White)
                };
                let label_style = if is_selected {
                    Style::default().fg(Color::DarkGray).bg(Color::DarkGray)
                } else {
                    Style::default().fg(Color::DarkGray)
                };
                let line = Line::from(vec![
                    Span::styled(format!("      {}: ", label), label_style),
                    Span::styled(value, style),
                    Span::styled(
                        " ".repeat(inner_area.width as usize),
                        if is_selected {
                            Style::default().bg(Color::DarkGray)
                        } else {
                            Style::default()
                        },
                    ),
                ]);
                f.render_widget(Paragraph::new(line), row_area);
            }
        }

        y += 1;
    }
}

/// Simple overview render (for the Overview tab) - shows machine summary
pub fn render_overview(f: &mut Frame, area: Rect, state: &DashboardState) {
    let current_machine_id = state
        .sync_state
        .as_ref()
        .map(|s| s.machine_id.as_str())
        .unwrap_or("");

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
                let is_current = m.machine_id == current_machine_id;

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

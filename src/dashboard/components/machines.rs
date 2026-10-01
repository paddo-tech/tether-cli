use super::profile_picker::ProfilePicker;
use super::{clamp_cursor, cursor_down, manager_label};
use crate::cli::output::relative_time;
use crate::dashboard::app::{App, Overlay};
use crate::dashboard::msg::KeyOutcome;
use crate::dashboard::state::DashboardState;
use crossterm::event::{KeyCode, KeyEvent};
use ratatui::{prelude::*, widgets::*};

#[derive(Default)]
pub struct MachinesTabState {
    pub cursor: usize,
    pub expanded: Option<String>,
}

pub fn handle_key(app: &mut App, key: KeyEvent) -> KeyOutcome {
    match key.code {
        KeyCode::Enter => toggle_expand(app),
        KeyCode::Char('p') => open_profile_picker(app),
        KeyCode::Char('j') | KeyCode::Down => {
            let len = build_rows(&app.state, app.machines.expanded.as_deref()).len();
            cursor_down(&mut app.machines.cursor, len);
        }
        KeyCode::Char('k') | KeyCode::Up => {
            app.machines.cursor = app.machines.cursor.saturating_sub(1);
        }
        _ => return KeyOutcome::Ignored,
    }
    KeyOutcome::Handled(None)
}

fn toggle_expand(app: &mut App) {
    let rows = build_rows(&app.state, app.machines.expanded.as_deref());
    let Some(MachineRow::Header { machine_id, .. }) = rows.get(app.machines.cursor) else {
        return;
    };
    if app.machines.expanded.as_deref() == Some(machine_id.as_str()) {
        app.machines.expanded = None;
    } else {
        app.machines.expanded = Some(machine_id.clone());
    }
    let len = build_rows(&app.state, app.machines.expanded.as_deref()).len();
    clamp_cursor(&mut app.machines.cursor, len);
}

/// Pick this machine's profile, starting on the current one.
fn open_profile_picker(app: &mut App) {
    let Some(ref config) = app.state.config else {
        return;
    };
    let mut names: Vec<&str> = config.profiles.keys().map(|s| s.as_str()).collect();
    names.sort();
    let cursor = app
        .state
        .sync_state
        .as_ref()
        .and_then(|s| config.machine_profiles.get(&s.machine_id))
        .and_then(|p| names.iter().position(|n| *n == p.as_str()))
        .unwrap_or(0);
    let options = names.iter().map(|s| s.to_string()).collect();
    app.overlays
        .push(Overlay::ProfilePicker(ProfilePicker { options, cursor }));
}

/// Row in the flat machine list
pub enum MachineRow {
    Header {
        machine_id: String,
        is_current: bool,
        file_count: usize,
        pkg_count: usize,
        last_sync: String,
        profile: Option<String>,
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
            profile: Some(
                m.profile
                    .clone()
                    .or_else(|| {
                        state
                            .config
                            .as_ref()
                            .map(|c| c.profile_name(&m.machine_id).to_string())
                    })
                    .unwrap_or_else(|| crate::config::DEFAULT_PROFILE.to_string()),
            ),
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

pub fn render(f: &mut Frame, area: Rect, app: &App) {
    let t = &app.theme;
    let expanded = app.machines.expanded.as_deref();
    let cursor = app.machines.cursor;
    let rows = build_rows(&app.state, expanded);

    let block = Block::default()
        .title(" Machines ")
        .borders(Borders::ALL)
        .border_style(Style::default().fg(t.border));
    let inner_area = block.inner(area);
    f.render_widget(block, area);

    if rows.is_empty() {
        let msg = Paragraph::new(Span::styled(
            "  No machines found",
            Style::default().fg(t.muted),
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

    for (y, (row_idx, row)) in
        (inner_area.y..inner_area.y + inner_area.height).zip(rows.iter().enumerate().skip(scroll))
    {
        let is_selected = row_idx == cursor;
        let row_area = Rect::new(inner_area.x, y, inner_area.width, 1);

        match row {
            MachineRow::Header {
                machine_id,
                is_current,
                file_count,
                pkg_count,
                last_sync,
                profile,
                ..
            } => {
                let is_expanded = expanded == Some(machine_id.as_str());
                let arrow = if is_expanded { "v" } else { ">" };
                let marker = if *is_current { "* " } else { "  " };

                let name_style = if is_selected {
                    if *is_current {
                        Style::default().fg(t.text).bg(t.selection).bold()
                    } else {
                        Style::default().fg(t.text).bg(t.selection)
                    }
                } else if *is_current {
                    Style::default().fg(t.text).bold()
                } else {
                    Style::default().fg(t.text)
                };

                let bg_style = if is_selected {
                    Style::default().bg(t.selection)
                } else {
                    Style::default()
                };

                let marker_style = if *is_current {
                    if is_selected {
                        Style::default().fg(t.ok).bg(t.selection).bold()
                    } else {
                        Style::default().fg(t.ok).bold()
                    }
                } else {
                    bg_style
                };

                let dim_style = if is_selected {
                    Style::default().fg(t.selection).bg(t.selection)
                } else {
                    Style::default().fg(t.muted)
                };

                let profile_span = if let Some(p) = profile {
                    Span::styled(format!(" [{}]", p), dim_style)
                } else {
                    Span::styled("", dim_style)
                };

                let line = Line::from(vec![
                    Span::styled(format!("  {} ", arrow), name_style),
                    Span::styled(marker, marker_style),
                    Span::styled(machine_id, name_style),
                    profile_span,
                    Span::styled(format!("  {}f {}p", file_count, pkg_count), dim_style),
                    Span::styled(format!("  {}", last_sync), dim_style),
                    Span::styled(" ".repeat(inner_area.width as usize), bg_style),
                ]);
                f.render_widget(Paragraph::new(line), row_area);
            }
            MachineRow::Detail { label, value } => {
                let style = if is_selected {
                    Style::default().fg(t.text).bg(t.selection)
                } else {
                    Style::default().fg(t.text)
                };
                let label_style = if is_selected {
                    Style::default().fg(t.selection).bg(t.selection)
                } else {
                    Style::default().fg(t.muted)
                };
                let line = Line::from(vec![
                    Span::styled(format!("      {}: ", label), label_style),
                    Span::styled(value, style),
                    Span::styled(
                        " ".repeat(inner_area.width as usize),
                        if is_selected {
                            Style::default().bg(t.selection)
                        } else {
                            Style::default()
                        },
                    ),
                ]);
                f.render_widget(Paragraph::new(line), row_area);
            }
        }
    }
}

/// Simple overview render (for the Overview tab) - shows machine summary
pub fn render_overview(f: &mut Frame, area: Rect, app: &App) {
    let t = &app.theme;
    let state = &app.state;
    let current_machine_id = state
        .sync_state
        .as_ref()
        .map(|s| s.machine_id.as_str())
        .unwrap_or("");

    let items: Vec<ListItem> = if state.machines.is_empty() {
        vec![ListItem::new(Span::styled(
            "  No machines found",
            Style::default().fg(t.muted),
        ))]
    } else {
        state
            .machines
            .iter()
            .map(|m| {
                let is_current = m.machine_id == current_machine_id;

                let marker = if is_current {
                    Span::styled(" * ", Style::default().fg(t.ok).bold())
                } else {
                    Span::styled("   ", Style::default())
                };

                let name = Span::styled(
                    &m.machine_id,
                    if is_current {
                        Style::default().fg(t.text).bold()
                    } else {
                        Style::default().fg(t.text)
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
                        Style::default().fg(t.muted),
                    ),
                    Span::raw("  "),
                    Span::styled(time, Style::default().fg(t.muted)),
                ]))
            })
            .collect()
    };

    let list = List::new(items).block(
        Block::default()
            .title(" Machines ")
            .borders(Borders::ALL)
            .border_style(Style::default().fg(t.border)),
    );
    f.render_widget(list, area);
}

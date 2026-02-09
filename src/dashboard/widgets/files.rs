use crate::cli::output::relative_time;
use crate::dashboard::state::DashboardState;
use ratatui::{prelude::*, widgets::*};
use std::collections::HashSet;

pub enum FileRow {
    SectionHeader {
        label: String,
        url: String,
        count: usize,
    },
    File {
        path: String,
        synced: bool,
        time: String,
    },
}

pub fn build_rows(state: &DashboardState) -> Vec<FileRow> {
    let mut rows = Vec::new();

    // Collect team symlink target paths (relative to home) so we can exclude them from personal
    let home = crate::home_dir().unwrap_or_default();
    let mut team_paths: HashSet<String> = HashSet::new();
    // team_name -> sorted vec of relative target paths
    let mut team_files: Vec<(String, Vec<String>)> = Vec::new();

    let mut sorted_teams: Vec<_> = state.team_manifest.symlinks.iter().collect();
    sorted_teams.sort_by_key(|(name, _)| name.as_str());

    for (team_name, symlink_map) in &sorted_teams {
        let mut paths: Vec<String> = symlink_map
            .keys()
            .map(|target| {
                let p = std::path::Path::new(target);
                p.strip_prefix(&home)
                    .unwrap_or(p)
                    .to_string_lossy()
                    .to_string()
            })
            .collect();
        paths.sort();
        for p in &paths {
            team_paths.insert(p.clone());
        }
        team_files.push((team_name.to_string(), paths));
    }

    // Personal section
    let personal_url = state
        .config
        .as_ref()
        .map(|c| c.backend.url.clone())
        .unwrap_or_default();

    let personal_files: Vec<_> = match &state.sync_state {
        Some(ss) => {
            let mut files: Vec<_> = ss
                .files
                .iter()
                .filter(|(path, _)| !team_paths.contains(path.as_str()))
                .collect();
            files.sort_by_key(|(path, _)| path.as_str());
            files
        }
        None => Vec::new(),
    };

    rows.push(FileRow::SectionHeader {
        label: "Personal".to_string(),
        url: personal_url,
        count: personal_files.len(),
    });
    for (path, file_state) in &personal_files {
        rows.push(FileRow::File {
            path: path.to_string(),
            synced: file_state.synced,
            time: relative_time(file_state.last_modified),
        });
    }

    // Team sections
    for (team_name, paths) in &team_files {
        let team_url = state
            .config
            .as_ref()
            .and_then(|c| c.teams.as_ref())
            .and_then(|t| t.teams.get(team_name.as_str()))
            .map(|tc| tc.url.clone())
            .unwrap_or_default();

        rows.push(FileRow::SectionHeader {
            label: format!("Team: {}", team_name),
            url: team_url,
            count: paths.len(),
        });
        for path in paths {
            rows.push(FileRow::File {
                path: path.clone(),
                synced: true,
                time: String::new(),
            });
        }
    }

    rows
}

pub fn render(f: &mut Frame, area: Rect, state: &DashboardState, scroll_offset: usize) {
    let rows = build_rows(state);

    let items: Vec<ListItem> = if rows.is_empty() {
        vec![ListItem::new(Span::styled(
            "  No sync state",
            Style::default().fg(Color::DarkGray),
        ))]
    } else {
        rows.into_iter()
            .skip(scroll_offset)
            .map(|row| match row {
                FileRow::SectionHeader { label, url, count } => {
                    let mut spans = vec![
                        Span::styled(
                            format!(" {} ", label),
                            Style::default().fg(Color::Cyan).bold(),
                        ),
                        Span::styled(format!("({})", count), Style::default().fg(Color::DarkGray)),
                    ];
                    if !url.is_empty() {
                        spans.push(Span::styled(
                            format!("  {}", url),
                            Style::default().fg(Color::DarkGray),
                        ));
                    }
                    ListItem::new(Line::from(spans))
                }
                FileRow::File { path, synced, time } => {
                    let badge = if synced {
                        Span::styled(" ok ", Style::default().fg(Color::Green))
                    } else {
                        Span::styled(" ** ", Style::default().fg(Color::Yellow))
                    };
                    let mut spans = vec![
                        Span::raw("  "),
                        badge,
                        Span::raw(" "),
                        Span::styled(path, Style::default().fg(Color::White)),
                    ];
                    if !time.is_empty() {
                        spans.push(Span::raw("  "));
                        spans.push(Span::styled(time, Style::default().fg(Color::DarkGray)));
                    }
                    ListItem::new(Line::from(spans))
                }
            })
            .collect()
    };

    let list = List::new(items).block(
        Block::default()
            .title(" Dotfiles ")
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::DarkGray)),
    );
    f.render_widget(list, area);
}

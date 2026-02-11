use crate::cli::output::relative_time;
use crate::dashboard::state::DashboardState;
use ratatui::{prelude::*, widgets::*};
use std::collections::{HashMap, HashSet};

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

    // Build org -> team_name mapping for project config grouping
    let org_to_team: HashMap<String, String> = state
        .config
        .as_ref()
        .and_then(|c| c.teams.as_ref())
        .map(|teams| {
            let mut map = HashMap::new();
            for (team_name, team_config) in &teams.teams {
                if team_config.enabled {
                    for org in &team_config.orgs {
                        map.insert(org.to_lowercase(), team_name.clone());
                    }
                }
            }
            map
        })
        .unwrap_or_default();

    // Split sync state files into personal dotfiles, personal projects, and team projects
    let mut personal_dotfiles = Vec::new();
    let mut personal_projects = Vec::new();
    // team_name -> vec of (display_path, synced, time)
    let mut team_project_files: HashMap<String, Vec<(String, bool, String)>> = HashMap::new();

    if let Some(ss) = &state.sync_state {
        let mut files: Vec<_> = ss.files.iter().collect();
        files.sort_by_key(|(path, _)| path.as_str());

        for (path, file_state) in files {
            if team_paths.contains(path.as_str()) {
                continue;
            }

            if let Some(rest) = path.strip_prefix("project:") {
                let display = rest.to_string();
                let entry = (
                    display.clone(),
                    file_state.synced,
                    relative_time(file_state.last_modified),
                );

                let team = crate::sync::extract_org_from_normalized_url(rest)
                    .and_then(|org| org_to_team.get(&org.to_lowercase()).cloned());

                if let Some(team_name) = team {
                    team_project_files.entry(team_name).or_default().push(entry);
                } else {
                    personal_projects.push(entry);
                }
            } else {
                personal_dotfiles.push((
                    path.to_string(),
                    file_state.synced,
                    relative_time(file_state.last_modified),
                ));
            }
        }
    }

    // Personal dotfiles section
    let personal_url = state
        .config
        .as_ref()
        .map(|c| c.backend.url.clone())
        .unwrap_or_default();

    let personal_count = personal_dotfiles.len() + personal_projects.len();
    rows.push(FileRow::SectionHeader {
        label: "Personal".to_string(),
        url: personal_url,
        count: personal_count,
    });
    for (path, synced, time) in &personal_dotfiles {
        rows.push(FileRow::File {
            path: path.clone(),
            synced: *synced,
            time: time.clone(),
        });
    }
    for (path, synced, time) in &personal_projects {
        rows.push(FileRow::File {
            path: path.clone(),
            synced: *synced,
            time: time.clone(),
        });
    }

    // Team dotfile sections (from symlinks)
    for (team_name, paths) in &team_files {
        let team_url = state
            .config
            .as_ref()
            .and_then(|c| c.teams.as_ref())
            .and_then(|t| t.teams.get(team_name.as_str()))
            .map(|tc| tc.url.clone())
            .unwrap_or_default();

        let project_count = team_project_files
            .get(team_name)
            .map(|v| v.len())
            .unwrap_or(0);

        rows.push(FileRow::SectionHeader {
            label: format!("Team: {}", team_name),
            url: team_url,
            count: paths.len() + project_count,
        });
        for path in paths {
            rows.push(FileRow::File {
                path: path.clone(),
                synced: true,
                time: String::new(),
            });
        }
        // Team project secrets
        if let Some(projects) = team_project_files.remove(team_name) {
            for (path, synced, time) in projects {
                rows.push(FileRow::File { path, synced, time });
            }
        }
    }

    // Any remaining team project files (team has projects but no symlinks)
    let mut remaining: Vec<_> = team_project_files.into_iter().collect();
    remaining.sort_by(|(a, _), (b, _)| a.cmp(b));
    for (team_name, projects) in remaining {
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
            count: projects.len(),
        });
        for (path, synced, time) in projects {
            rows.push(FileRow::File { path, synced, time });
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

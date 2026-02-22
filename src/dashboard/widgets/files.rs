use crate::cli::output::relative_time;
use crate::dashboard::state::DashboardState;
use crate::dashboard::FilesTabState;
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
        repo_path: String,
    },
    HistoryEntry {
        commit_hash: String,
        short_hash: String,
        date: String,
        machine_id: String,
        message: String,
    },
    DiffRow {
        line: String,
    },
    DeletedHeader {
        section: String,
        count: usize,
    },
    DeletedFile {
        path: String,
    },
}

struct SectionData {
    label: String,
    url: String,
    files: Vec<(String, bool, String, String)>, // (display_path, synced, time, repo_path)
}

fn collect_sections(state: &DashboardState) -> Vec<SectionData> {
    let mut sections = Vec::new();

    let home = crate::home_dir().unwrap_or_default();
    let mut team_paths: HashSet<String> = HashSet::new();
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

    let encrypted = state
        .config
        .as_ref()
        .map(|c| c.security.encrypt_dotfiles)
        .unwrap_or(false);

    let mut personal_dotfiles = Vec::new();
    let mut personal_projects = Vec::new();
    let mut team_project_files: HashMap<String, Vec<(String, bool, String, String)>> =
        HashMap::new();

    if let Some(ss) = &state.sync_state {
        let mut files: Vec<_> = ss.files.iter().collect();
        files.sort_by_key(|(path, _)| path.as_str());

        for (path, file_state) in files {
            if team_paths.contains(path.as_str()) {
                continue;
            }

            // Skip non-dotfile entries (team secrets, collab secrets, tether config, dirs)
            if path.starts_with("team-secret:")
                || path.starts_with("collab-secret:")
                || path.starts_with("~/")
                || path.starts_with(".tether/")
            {
                continue;
            }

            if let Some(rest) = path.strip_prefix("project:") {
                let display = rest.to_string();
                let repo_path = if encrypted {
                    format!("projects/{}.enc", rest)
                } else {
                    format!("projects/{}", rest)
                };
                let entry = (
                    display,
                    file_state.synced,
                    relative_time(file_state.last_modified),
                    repo_path,
                );

                let team = crate::sync::extract_org_from_normalized_url(rest)
                    .and_then(|org| org_to_team.get(&org.to_lowercase()).cloned());

                if let Some(team_name) = team {
                    team_project_files.entry(team_name).or_default().push(entry);
                } else {
                    personal_projects.push(entry);
                }
            } else {
                // Build repo path: use profile-aware path if possible, flat fallback
                let machine_id = state
                    .sync_state
                    .as_ref()
                    .map(|s| s.machine_id.as_str())
                    .unwrap_or("");
                let config_ref = state.config.as_ref();
                let profile = config_ref
                    .map(|c| c.profile_name(machine_id))
                    .unwrap_or("dev");
                let shared = config_ref
                    .map(|c| c.is_dotfile_shared(machine_id, path))
                    .unwrap_or(false);
                let sync_path = crate::sync::SyncEngine::sync_path().ok();
                let repo_path = if let Some(ref sp) = sync_path {
                    crate::sync::resolve_dotfile_repo_path(sp, path, encrypted, profile, shared)
                } else {
                    crate::sync::dotfile_to_repo_path(path, encrypted)
                };
                personal_dotfiles.push((
                    path.to_string(),
                    file_state.synced,
                    relative_time(file_state.last_modified),
                    repo_path,
                ));
            }
        }
    }

    // Personal section
    let personal_url = state
        .config
        .as_ref()
        .map(|c| c.backend.url.clone())
        .unwrap_or_default();

    let mut personal_files = personal_dotfiles;
    personal_files.extend(personal_projects);
    sections.push(SectionData {
        label: "Personal".to_string(),
        url: personal_url,
        files: personal_files,
    });

    // Team sections
    for (team_name, paths) in &team_files {
        let team_url = state
            .config
            .as_ref()
            .and_then(|c| c.teams.as_ref())
            .and_then(|t| t.teams.get(team_name.as_str()))
            .map(|tc| tc.url.clone())
            .unwrap_or_default();

        let mut files: Vec<(String, bool, String, String)> = paths
            .iter()
            .map(|p| (p.clone(), true, String::new(), String::new()))
            .collect();

        if let Some(projects) = team_project_files.remove(team_name) {
            files.extend(projects);
        }

        sections.push(SectionData {
            label: format!("Team: {}", team_name),
            url: team_url,
            files,
        });
    }

    // Remaining team project files
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

        sections.push(SectionData {
            label: format!("Team: {}", team_name),
            url: team_url,
            files: projects,
        });
    }

    sections
}

/// Build rows for the interactive Files tab
pub fn build_rows(state: &DashboardState, ft: &FilesTabState) -> Vec<FileRow> {
    let sections = collect_sections(state);
    let mut rows = Vec::new();

    for section in &sections {
        let is_collapsed = ft.collapsed.contains(&section.label);

        rows.push(FileRow::SectionHeader {
            label: section.label.clone(),
            url: section.url.clone(),
            count: section.files.len(),
        });

        if !is_collapsed {
            for (path, synced, time, repo_path) in &section.files {
                rows.push(FileRow::File {
                    path: path.clone(),
                    synced: *synced,
                    time: time.clone(),
                    repo_path: repo_path.clone(),
                });

                // Show history entries if this file is expanded
                if ft.expanded_file.as_deref() == Some(repo_path.as_str()) {
                    for entry in &ft.expanded_history {
                        let is_diff_expanded =
                            ft.expanded_commit.as_deref() == Some(entry.commit_hash.as_str());
                        rows.push(FileRow::HistoryEntry {
                            commit_hash: entry.commit_hash.clone(),
                            short_hash: entry.short_hash.clone(),
                            date: relative_time(entry.date),
                            machine_id: entry.machine_id.clone(),
                            message: entry.message.clone(),
                        });
                        if is_diff_expanded {
                            for line in &ft.expanded_diff {
                                rows.push(FileRow::DiffRow { line: line.clone() });
                            }
                        }
                    }
                }
            }

            // Deleted files footer
            if let Some(deleted) = ft.deleted.get(&section.label) {
                if !deleted.is_empty() {
                    rows.push(FileRow::DeletedHeader {
                        section: section.label.clone(),
                        count: deleted.len(),
                    });

                    if ft.show_deleted.contains(&section.label) {
                        for path in deleted {
                            rows.push(FileRow::DeletedFile { path: path.clone() });
                        }
                    }
                }
            }
        }
    }

    rows
}

/// Build simple rows for the Overview tab (no interactivity)
pub fn build_overview_rows(state: &DashboardState) -> Vec<FileRow> {
    let sections = collect_sections(state);
    let mut rows = Vec::new();

    for section in sections {
        rows.push(FileRow::SectionHeader {
            label: section.label,
            url: section.url,
            count: section.files.len(),
        });
        for (path, synced, time, repo_path) in section.files {
            rows.push(FileRow::File {
                path,
                synced,
                time,
                repo_path,
            });
        }
    }

    rows
}

/// Render the interactive Files tab with cursor, expand/collapse
pub fn render(f: &mut Frame, area: Rect, state: &DashboardState, ft: &FilesTabState) {
    let rows = build_rows(state, ft);
    let cursor = ft.cursor;

    let block = Block::default()
        .title(" Files ")
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::DarkGray));
    let inner_area = block.inner(area);
    f.render_widget(block, area);

    if rows.is_empty() {
        let msg = Paragraph::new(Span::styled(
            "  No sync state",
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

        let bg = if is_selected {
            Color::DarkGray
        } else {
            Color::Reset
        };

        match row {
            FileRow::SectionHeader { label, url, count } => {
                let is_collapsed = ft.collapsed.contains(label.as_str());
                let arrow = if is_collapsed { ">" } else { "v" };

                let mut spans = vec![
                    Span::styled(
                        format!(" {} ", arrow),
                        Style::default().fg(Color::Cyan).bg(bg),
                    ),
                    Span::styled(
                        format!("{} ", label),
                        Style::default().fg(Color::Cyan).bg(bg).bold(),
                    ),
                    Span::styled(
                        format!("({})", count),
                        Style::default().fg(Color::DarkGray).bg(bg),
                    ),
                ];
                if !url.is_empty() {
                    spans.push(Span::styled(
                        format!("  {}", url),
                        Style::default().fg(Color::DarkGray).bg(bg),
                    ));
                }
                spans.push(Span::styled(
                    " ".repeat(inner_area.width as usize),
                    Style::default().bg(bg),
                ));
                f.render_widget(Paragraph::new(Line::from(spans)), row_area);
            }
            FileRow::File {
                path,
                synced,
                time,
                repo_path,
            } => {
                let is_expanded = ft.expanded_file.as_deref() == Some(repo_path.as_str());
                let arrow = if !repo_path.is_empty() {
                    if is_expanded {
                        "v"
                    } else {
                        ">"
                    }
                } else {
                    " "
                };
                let badge = if *synced {
                    Span::styled(" ok ", Style::default().fg(Color::Green).bg(bg))
                } else {
                    Span::styled(" ** ", Style::default().fg(Color::Yellow).bg(bg))
                };
                let mut spans = vec![
                    Span::styled(
                        format!(" {}", arrow),
                        Style::default().fg(Color::DarkGray).bg(bg),
                    ),
                    badge,
                    Span::styled(" ", Style::default().bg(bg)),
                    Span::styled(path, Style::default().fg(Color::White).bg(bg)),
                ];
                if !time.is_empty() {
                    spans.push(Span::styled("  ", Style::default().bg(bg)));
                    spans.push(Span::styled(
                        time,
                        Style::default().fg(Color::DarkGray).bg(bg),
                    ));
                }
                spans.push(Span::styled(
                    " ".repeat(inner_area.width as usize),
                    Style::default().bg(bg),
                ));
                f.render_widget(Paragraph::new(Line::from(spans)), row_area);
            }
            FileRow::HistoryEntry {
                commit_hash,
                short_hash,
                date,
                machine_id,
                message,
            } => {
                let is_diff_expanded = ft.expanded_commit.as_deref() == Some(commit_hash.as_str());
                let arrow = if is_diff_expanded { "v" } else { ">" };
                let line = Line::from(vec![
                    Span::styled(
                        format!("     {} ", arrow),
                        Style::default().fg(Color::DarkGray).bg(bg),
                    ),
                    Span::styled(short_hash, Style::default().fg(Color::Yellow).bg(bg).bold()),
                    Span::styled(
                        format!("  {:>12}", date),
                        Style::default().fg(Color::DarkGray).bg(bg),
                    ),
                    Span::styled(
                        format!("  {:15}", machine_id),
                        Style::default().fg(Color::DarkGray).bg(bg),
                    ),
                    Span::styled(
                        format!("  {}", message),
                        Style::default().fg(Color::White).bg(bg),
                    ),
                    Span::styled(
                        " ".repeat(inner_area.width as usize),
                        Style::default().bg(bg),
                    ),
                ]);
                f.render_widget(Paragraph::new(line), row_area);
            }
            FileRow::DeletedHeader { section, count } => {
                let is_expanded = ft.show_deleted.contains(section.as_str());
                let arrow = if is_expanded { "v" } else { ">" };
                let line = Line::from(vec![
                    Span::styled(
                        format!("  {} ", arrow),
                        Style::default().fg(Color::DarkGray).bg(bg),
                    ),
                    Span::styled(
                        format!("Deleted ({})", count),
                        Style::default().fg(Color::DarkGray).bg(bg),
                    ),
                    Span::styled(
                        " ".repeat(inner_area.width as usize),
                        Style::default().bg(bg),
                    ),
                ]);
                f.render_widget(Paragraph::new(line), row_area);
            }
            FileRow::DeletedFile { path } => {
                let line = Line::from(vec![
                    Span::styled("      ", Style::default().bg(bg)),
                    Span::styled(path, Style::default().fg(Color::Red).bg(bg)),
                    Span::styled(
                        " ".repeat(inner_area.width as usize),
                        Style::default().bg(bg),
                    ),
                ]);
                f.render_widget(Paragraph::new(line), row_area);
            }
            FileRow::DiffRow { line: diff_line } => {
                let fg = if diff_line.starts_with("@@") {
                    Color::Cyan
                } else if diff_line.starts_with("+++") || diff_line.starts_with("---") {
                    Color::DarkGray
                } else if diff_line.starts_with('+') {
                    Color::Green
                } else if diff_line.starts_with('-') {
                    Color::Red
                } else {
                    Color::DarkGray
                };
                let line = Line::from(vec![
                    Span::styled("        ", Style::default().bg(bg)),
                    Span::styled(diff_line, Style::default().fg(fg).bg(bg)),
                    Span::styled(
                        " ".repeat(inner_area.width as usize),
                        Style::default().bg(bg),
                    ),
                ]);
                f.render_widget(Paragraph::new(line), row_area);
            }
        }

        y += 1;
    }
}

/// Render compact file list for the Overview tab (non-interactive, uses List widget)
pub fn render_overview(f: &mut Frame, area: Rect, state: &DashboardState, scroll_offset: usize) {
    let rows = build_overview_rows(state);

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
                FileRow::File {
                    path, synced, time, ..
                } => {
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
                _ => ListItem::new(Span::raw("")),
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

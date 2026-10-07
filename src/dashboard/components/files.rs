use super::confirm::Confirm;
use super::diff::{self, DiffLine};
use super::file_import::{FileImport, ImportItem};
use super::{clamp_cursor, cursor_down, list, panel, row, truncate};
use crate::cli::output::relative_time;
use crate::dashboard::app::{App, Overlay};
use crate::dashboard::config_edit;
use crate::dashboard::msg::KeyOutcome;
use crate::dashboard::repo;
use crate::dashboard::state::DashboardState;
use crossterm::event::{KeyCode, KeyEvent};
use ratatui::{prelude::*, widgets::Paragraph};
use std::collections::{HashMap, HashSet};

pub struct FilesTabState {
    pub cursor: usize,
    pub collapsed: HashSet<String>,
    pub expanded_file: Option<String>,
    pub expanded_history: Vec<crate::sync::FileLogEntry>,
    pub expanded_commit: Option<String>,
    pub expanded_diff: Vec<DiffLine>,
    pub deleted: HashMap<String, Vec<String>>,
    pub show_deleted: HashSet<String>,
}

impl FilesTabState {
    pub fn new(deleted: HashMap<String, Vec<String>>) -> Self {
        Self {
            cursor: 0,
            collapsed: HashSet::new(),
            expanded_file: None,
            expanded_history: Vec::new(),
            expanded_commit: None,
            expanded_diff: Vec::new(),
            deleted,
            show_deleted: HashSet::new(),
        }
    }
}

pub fn handle_key(app: &mut App, key: KeyEvent) -> KeyOutcome {
    match key.code {
        KeyCode::Enter => toggle_row(app),
        KeyCode::Esc => return collapse(app),
        KeyCode::Char('t') => toggle_shared(app),
        KeyCode::Char('R') => confirm_restore(app),
        KeyCode::Char('x') => confirm_remove(app),
        KeyCode::Char('i') => open_import(app),
        KeyCode::Char('j') | KeyCode::Down => {
            let len = build_rows(&app.state, &app.files).len();
            cursor_down(&mut app.files.cursor, len);
        }
        KeyCode::Char('k') | KeyCode::Up => {
            app.files.cursor = app.files.cursor.saturating_sub(1);
        }
        _ => return KeyOutcome::Ignored,
    }
    KeyOutcome::Handled(None)
}

/// Close the innermost open part: a history diff, then the file's history.
fn collapse(app: &mut App) -> KeyOutcome {
    let ft = &mut app.files;
    if ft.expanded_commit.is_some() {
        ft.expanded_commit = None;
        ft.expanded_diff.clear();
    } else if ft.expanded_file.is_some() {
        ft.expanded_file = None;
        ft.expanded_history.clear();
    } else {
        return KeyOutcome::Ignored;
    }
    let len = build_rows(&app.state, &app.files).len();
    clamp_cursor(&mut app.files.cursor, len);
    KeyOutcome::Handled(None)
}

/// Expand/collapse sections, files, history diffs and deleted lists.
fn toggle_row(app: &mut App) {
    let rows = build_rows(&app.state, &app.files);
    if app.files.cursor >= rows.len() {
        return;
    }
    let encrypted = app.encrypted();
    match &rows[app.files.cursor] {
        FileRow::SectionHeader { label, .. } => {
            let label = label.clone();
            if !app.files.collapsed.remove(&label) {
                app.files.collapsed.insert(label);
            }
        }
        FileRow::File { repo_path, .. } => {
            if app.files.expanded_file.as_deref() == Some(repo_path.as_str()) {
                app.files.expanded_file = None;
                app.files.expanded_history.clear();
                app.files.expanded_commit = None;
                app.files.expanded_diff.clear();
            } else {
                app.files.expanded_history = repo::file_history(repo_path, encrypted);
                app.files.expanded_file = Some(repo_path.clone());
            }
        }
        FileRow::HistoryEntry { commit_hash, .. } => {
            if app.files.expanded_commit.as_deref() == Some(commit_hash.as_str()) {
                app.files.expanded_commit = None;
                app.files.expanded_diff.clear();
            } else {
                app.files.expanded_diff = app
                    .files
                    .expanded_file
                    .as_ref()
                    .map(|repo_path| {
                        let dotfile = repo::repo_path_to_dotfile(
                            repo_path,
                            encrypted,
                            app.state.config.as_ref(),
                        );
                        diff::annotate(&repo::file_diff(
                            commit_hash,
                            repo_path,
                            &dotfile,
                            encrypted,
                        ))
                    })
                    .unwrap_or_default();
                app.files.expanded_commit = Some(commit_hash.clone());
            }
        }
        FileRow::DeletedHeader { section, .. } => {
            let section = section.clone();
            if !app.files.show_deleted.remove(&section) {
                app.files.show_deleted.insert(section);
            }
        }
        _ => {}
    }
    let len = build_rows(&app.state, &app.files).len();
    clamp_cursor(&mut app.files.cursor, len);
}

fn toggle_shared(app: &mut App) {
    let rows = build_rows(&app.state, &app.files);
    let Some(FileRow::File { path, .. }) = rows.get(app.files.cursor) else {
        return;
    };
    let (Some(config), Some(ss)) = (&mut app.state.config, &app.state.sync_state) else {
        return;
    };
    if config_edit::toggle_profile_dotfile_shared(config, &ss.machine_id, path) {
        let shared = if config.is_dotfile_shared(&ss.machine_id, path) {
            "on"
        } else {
            "off"
        };
        app.flash_success(format!("{} shared: {}", path, shared));
        app.reload_state();
    }
}

fn confirm_restore(app: &mut App) {
    let rows = build_rows(&app.state, &app.files);
    let Some(FileRow::HistoryEntry {
        commit_hash,
        short_hash,
        ..
    }) = rows.get(app.files.cursor)
    else {
        return;
    };
    let Some(repo_path) = app.files.expanded_file.clone() else {
        return;
    };
    let dotfile =
        repo::repo_path_to_dotfile(&repo_path, app.encrypted(), app.state.config.as_ref());
    app.overlays.push(Overlay::Confirm(Confirm::Restore {
        repo_path,
        dotfile,
        commit: commit_hash.clone(),
        short_hash: short_hash.clone(),
        arming: Default::default(),
    }));
}

/// Only personal dotfiles can be removed from the profile.
fn confirm_remove(app: &mut App) {
    let rows = build_rows(&app.state, &app.files);
    let Some(FileRow::File { path, .. }) = rows.get(app.files.cursor) else {
        return;
    };
    let is_personal = rows[..=app.files.cursor]
        .iter()
        .rev()
        .find_map(|r| match r {
            FileRow::SectionHeader { label, .. } => Some(label.starts_with("Personal")),
            _ => None,
        })
        .unwrap_or(false);
    if is_personal {
        app.overlays.push(Overlay::Confirm(Confirm::RemoveFile {
            path: path.clone(),
            arming: Default::default(),
        }));
    }
}

/// Offer dotfiles that other profiles track and this machine's profile does not.
pub fn open_import(app: &mut App) {
    let (Some(config), Some(ss)) = (&app.state.config, &app.state.sync_state) else {
        return;
    };
    let current_profile = config.profile_name(&ss.machine_id).to_string();
    let current_paths: HashSet<String> = config
        .profiles
        .get(&current_profile)
        .map(|p| p.dotfiles.iter().map(|e| e.path().to_string()).collect())
        .unwrap_or_default();
    let mut seen = HashSet::new();
    let mut items = Vec::new();
    let mut profiles: Vec<_> = config.profiles.keys().collect();
    profiles.sort();
    for name in profiles {
        if *name == current_profile {
            continue;
        }
        if let Some(profile) = config.profiles.get(name) {
            for entry in &profile.dotfiles {
                let path = entry.path().to_string();
                if !current_paths.contains(&path) && seen.insert(path.clone()) {
                    items.push(ImportItem {
                        path,
                        source_profile: name.clone(),
                    });
                }
            }
        }
    }
    if !items.is_empty() {
        app.overlays
            .push(Overlay::FileImport(FileImport { items, cursor: 0 }));
    }
}

/// Reload the expanded file's history after a state reload.
pub fn refresh_expanded(app: &mut App) {
    if let Some(ref repo_path) = app.files.expanded_file {
        app.files.expanded_history = repo::file_history(repo_path, app.encrypted());
        app.files.expanded_commit = None;
        app.files.expanded_diff.clear();
    }
    let len = build_rows(&app.state, &app.files).len();
    clamp_cursor(&mut app.files.cursor, len);
}

pub enum FileRow {
    SectionHeader {
        label: String,
        url: String,
        count: usize,
    },
    File {
        path: String,
        shared: bool,
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
        line: DiffLine,
    },
    DeletedHeader {
        section: String,
        count: usize,
    },
    DeletedFile {
        path: String,
    },
}

type FileEntry = (String, bool, bool, String, String);

struct SectionData {
    label: String,
    url: String,
    files: Vec<FileEntry>, // (display_path, shared, synced, time, repo_path)
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
    let mut team_project_files: HashMap<String, Vec<FileEntry>> = HashMap::new();

    if let Some(ss) = &state.sync_state {
        let mut files: Vec<_> = ss.files.iter().collect();
        files.sort_by_key(|(path, _)| path.as_str());

        for (path, file_state) in files {
            if team_paths.contains(path.as_str()) {
                continue;
            }

            // Skip non-dotfile entries (team secrets, collab secrets, tether config)
            if path.starts_with("team-secret:")
                || path.starts_with("collab-secret:")
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
                    false,
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
            } else if let Some(rel) = path.strip_prefix("~/") {
                let repo_path = if encrypted {
                    format!("configs/{}.enc", rel)
                } else {
                    format!("configs/{}", rel)
                };
                personal_dotfiles.push((
                    path.to_string(),
                    false,
                    file_state.synced,
                    relative_time(file_state.last_modified),
                    repo_path,
                ));
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
                    .unwrap_or(crate::config::DEFAULT_PROFILE);
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
                    shared,
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

        let mut files: Vec<FileEntry> = paths
            .iter()
            .map(|p| (p.clone(), false, true, String::new(), String::new()))
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
            for (path, shared, synced, time, repo_path) in &section.files {
                rows.push(FileRow::File {
                    path: path.clone(),
                    shared: *shared,
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
        for (path, shared, synced, time, repo_path) in section.files {
            rows.push(FileRow::File {
                path,
                shared,
                synced,
                time,
                repo_path,
            });
        }
    }

    rows
}

/// Render the interactive Files tab with cursor, expand/collapse
pub fn render(f: &mut Frame, area: Rect, app: &App) {
    let t = &app.theme;
    let ft = &app.files;
    let rows = build_rows(&app.state, ft);
    let (total, pending) = rows.iter().fold((0, 0), |(n, p), r| match r {
        FileRow::File { synced, .. } => (n + 1, p + usize::from(!synced)),
        _ => (n, p),
    });
    let mut summary = vec![Span::styled(
        format!(" {} shown ", total),
        Style::default().fg(t.muted),
    )];
    if pending > 0 {
        summary.push(Span::styled(
            format!("● {} pending ", pending),
            Style::default().fg(t.warn),
        ));
    }
    let block = panel(" Files ", true, t).title_top(Line::from(summary).right_aligned());
    let inner = block.inner(area);
    f.render_widget(block, area);

    if rows.is_empty() {
        f.render_widget(
            Paragraph::new(Span::styled("No sync state", Style::default().fg(t.dim))),
            inner,
        );
        return;
    }

    list(
        f,
        app,
        area,
        inner,
        &rows,
        ft.cursor,
        |f, r, file_row, selected| draw_row(f, r, file_row, selected, app),
    );
}

fn draw_row(f: &mut Frame, area: Rect, file_row: &FileRow, selected: bool, app: &App) {
    let t = &app.theme;
    let ft = &app.files;
    let chevron = |open: bool| if open { "▾ " } else { "▸ " };
    match file_row {
        FileRow::SectionHeader { label, url, count } => {
            let open = !ft.collapsed.contains(label.as_str());
            let color = if label.starts_with("Team") {
                t.team
            } else {
                t.accent
            };
            row(
                f,
                area,
                Line::from(vec![
                    Span::styled(chevron(open), Style::default().fg(color)),
                    Span::styled(label.as_str(), Style::default().fg(color).bold()),
                    Span::styled(format!("  {}", count), Style::default().fg(t.dim)),
                ]),
                Line::from(Span::styled(
                    truncate(url, (area.width as usize) / 2),
                    Style::default().fg(t.dim),
                )),
            );
        }
        FileRow::File {
            path,
            shared,
            synced,
            time,
            repo_path,
        } => {
            let expandable = !repo_path.is_empty();
            let open = ft.expanded_file.as_deref() == Some(repo_path.as_str());
            let (dot, dot_color) = if *synced {
                ("●", t.ok)
            } else {
                ("◐", t.warn)
            };
            let mut left = vec![
                Span::styled(
                    if expandable { chevron(open) } else { "  " },
                    Style::default().fg(t.dim),
                ),
                Span::styled(format!("  {} ", dot), Style::default().fg(dot_color)),
                Span::styled(
                    path.as_str(),
                    if selected || open {
                        Style::default().fg(t.text).bold()
                    } else {
                        Style::default().fg(t.text)
                    },
                ),
            ];
            if *shared {
                left.push(Span::styled("  shared", Style::default().fg(t.info)));
            }
            let mut right = Vec::new();
            if !synced {
                right.push(Span::styled("pending  ", Style::default().fg(t.warn)));
            }
            right.push(Span::styled(time.as_str(), Style::default().fg(t.dim)));
            row(f, area, Line::from(left), Line::from(right));
        }
        FileRow::HistoryEntry {
            commit_hash,
            short_hash,
            date,
            machine_id,
            message,
        } => {
            let open = ft.expanded_commit.as_deref() == Some(commit_hash.as_str());
            history_row(f, area, open, short_hash, date, machine_id, message, app);
        }
        FileRow::DeletedHeader { section, count } => {
            let open = ft.show_deleted.contains(section.as_str());
            f.render_widget(
                Line::from(vec![
                    Span::styled(format!("  {}", chevron(open)), Style::default().fg(t.dim)),
                    Span::styled(
                        format!("Deleted  {}", count),
                        Style::default().fg(t.muted).italic(),
                    ),
                ]),
                area,
            );
        }
        FileRow::DeletedFile { path } => {
            f.render_widget(
                Line::from(vec![
                    Span::styled("      ✗ ", Style::default().fg(t.error)),
                    Span::styled(path.as_str(), Style::default().fg(t.muted).crossed_out()),
                ]),
                area,
            );
        }
        FileRow::DiffRow { line } => diff::render_line(f, area, line, selected, t),
    }
}

/// A commit in a file or manifest history: hash, machine, message, and age on the right.
#[allow(clippy::too_many_arguments)]
pub fn history_row(
    f: &mut Frame,
    area: Rect,
    open: bool,
    short_hash: &str,
    date: &str,
    machine_id: &str,
    message: &str,
    app: &App,
) {
    let t = &app.theme;
    row(
        f,
        area,
        Line::from(vec![
            Span::styled(
                if open { "    ▾ " } else { "    ▸ " },
                Style::default().fg(t.dim),
            ),
            Span::styled(short_hash.to_string(), Style::default().fg(t.hash)),
            Span::styled(
                format!("  {}", truncate(machine_id, 24)),
                Style::default().fg(t.team),
            ),
            Span::styled(format!("  {}", message), Style::default().fg(t.muted)),
        ]),
        Line::from(Span::styled(date.to_string(), Style::default().fg(t.dim))),
    );
}

/// Compact file list for the Overview tab.
pub fn render_overview(f: &mut Frame, area: Rect, app: &App) {
    let t = &app.theme;
    let rows = build_overview_rows(&app.state);
    let files = rows
        .iter()
        .filter(|r| matches!(r, FileRow::File { .. }))
        .count();
    let block = panel(" Dotfiles ", false, t).title_top(
        Line::from(Span::styled(
            format!(" {} ", files),
            Style::default().fg(t.dim),
        ))
        .right_aligned(),
    );
    let inner = block.inner(area);
    f.render_widget(block, area);
    if rows.is_empty() {
        f.render_widget(
            Paragraph::new(Span::styled("No sync state", Style::default().fg(t.dim))),
            inner,
        );
        return;
    }
    let visible = inner.height as usize;
    let scroll = app.overview_scroll.min(rows.len().saturating_sub(visible));
    for (i, r) in rows.iter().skip(scroll).take(visible).enumerate() {
        let rect = Rect::new(inner.x, inner.y + i as u16, inner.width, 1);
        match r {
            FileRow::SectionHeader { label, count, .. } => {
                let color = if label.starts_with("Team") {
                    t.team
                } else {
                    t.accent
                };
                f.render_widget(
                    Line::from(vec![
                        Span::styled(label.as_str(), Style::default().fg(color).bold()),
                        Span::styled(format!("  {}", count), Style::default().fg(t.dim)),
                    ]),
                    rect,
                );
            }
            FileRow::File {
                path, synced, time, ..
            } => {
                let (dot, color) = if *synced {
                    ("●", t.ok)
                } else {
                    ("◐", t.warn)
                };
                row(
                    f,
                    rect,
                    Line::from(vec![
                        Span::styled(format!(" {} ", dot), Style::default().fg(color)),
                        Span::styled(path.as_str(), Style::default().fg(t.text)),
                    ]),
                    Line::from(Span::styled(time.as_str(), Style::default().fg(t.dim))),
                );
            }
            _ => {}
        }
    }
    super::scrollbar(f, area, rows.len(), scroll, visible, t);
}

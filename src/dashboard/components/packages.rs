use super::confirm::Confirm;
use super::pkg_import::{PkgImport, PkgImportItem};
use super::{clamp_cursor, cursor_down, manager_label};
use crate::cli::output::relative_time;
use crate::dashboard::app::{App, Overlay};
use crate::dashboard::msg::KeyOutcome;
use crate::dashboard::repo;
use crate::dashboard::state::DashboardState;
use crossterm::event::{KeyCode, KeyEvent};
use ratatui::{prelude::*, widgets::*};
use std::collections::{HashMap, HashSet};

pub struct PackagesTabState {
    pub cursor: usize,
    /// Manager whose installed-package list is expanded.
    pub expanded: Option<String>,
    /// Manager whose manifest history is open.
    pub history_manager: Option<String>,
    pub history: Vec<crate::sync::FileLogEntry>,
    /// History entry whose diff is expanded.
    pub history_commit: Option<String>,
    pub history_diff: Vec<String>,
}

impl PackagesTabState {
    pub fn new() -> Self {
        Self {
            cursor: 0,
            expanded: None,
            history_manager: None,
            history: Vec::new(),
            history_commit: None,
            history_diff: Vec::new(),
        }
    }
}

pub fn handle_key(app: &mut App, key: KeyEvent) -> KeyOutcome {
    match key.code {
        KeyCode::Enter => toggle_row(app),
        KeyCode::Char('R') => {
            if app.uninstalling.is_none() && app.installing.is_none() {
                confirm_rollback(app);
            }
        }
        KeyCode::Char('i') => {
            if app.installing.is_none() {
                open_import(app);
            }
        }
        KeyCode::Char('h') => toggle_history(app),
        KeyCode::Char('j') | KeyCode::Down => {
            let len = build_rows(&app.state, &app.packages).len();
            cursor_down(&mut app.packages.cursor, len);
        }
        KeyCode::Char('k') | KeyCode::Up => {
            app.packages.cursor = app.packages.cursor.saturating_sub(1);
        }
        _ => return KeyOutcome::Ignored,
    }
    KeyOutcome::Handled(None)
}

/// Expand a manager's package list, ask to uninstall a package, or toggle a history diff.
fn toggle_row(app: &mut App) {
    let rows = build_rows(&app.state, &app.packages);
    match rows.get(app.packages.cursor) {
        Some(PkgRow::Header { manager_key, .. }) => {
            if app.packages.expanded.as_deref() == Some(manager_key.as_str()) {
                app.packages.expanded = None;
            } else {
                app.packages.expanded = Some(manager_key.clone());
            }
            let len = build_rows(&app.state, &app.packages).len();
            clamp_cursor(&mut app.packages.cursor, len);
        }
        Some(PkgRow::Package { manager_key, name }) => {
            if app.uninstalling.is_none() && app.running.is_none() && manager_key != "brew_taps" {
                app.overlays.push(Overlay::Confirm(Confirm::Uninstall {
                    manager_key: manager_key.clone(),
                    name: name.clone(),
                }));
            }
        }
        Some(PkgRow::HistoryEntry { commit_hash, .. }) => {
            if app.packages.history_commit.as_deref() == Some(commit_hash.as_str()) {
                app.packages.history_commit = None;
                app.packages.history_diff.clear();
            } else {
                let manager = app.packages.history_manager.clone().unwrap_or_default();
                app.packages.history_diff = repo::pkg_diff(&manager, commit_hash);
                app.packages.history_commit = Some(commit_hash.clone());
            }
            let len = build_rows(&app.state, &app.packages).len();
            clamp_cursor(&mut app.packages.cursor, len);
        }
        Some(PkgRow::DiffRow { .. }) | None => {}
    }
}

fn confirm_rollback(app: &mut App) {
    let rows = build_rows(&app.state, &app.packages);
    let Some(PkgRow::HistoryEntry {
        commit_hash,
        short_hash,
        ..
    }) = rows.get(app.packages.cursor)
    else {
        return;
    };
    let Some(manager) = app.packages.history_manager.clone() else {
        return;
    };
    if crate::packages::manager_for_key(&manager).is_none() {
        app.flash_error(format!("Rollback is not supported for {}", manager));
    } else if let Some(plan) = repo::rollback_plan(&app.state, &manager, commit_hash, short_hash) {
        app.overlays.push(Overlay::Confirm(Confirm::Rollback(plan)));
    } else {
        app.flash_error(format!(
            "Could not read the {} manifest at {}",
            manager, short_hash
        ));
    }
}

/// Offer packages that other machines have and this machine neither has nor removed.
fn open_import(app: &mut App) {
    let current_machine_id = app.machine_id().to_string();
    let current_machine = app
        .state
        .machines
        .iter()
        .find(|m| m.machine_id == current_machine_id);
    let current_pkgs: HashMap<String, HashSet<String>> = current_machine
        .map(|m| {
            m.packages
                .iter()
                .map(|(k, v)| (k.clone(), v.iter().cloned().collect()))
                .collect()
        })
        .unwrap_or_default();
    let removed: HashMap<String, HashSet<String>> = current_machine
        .map(|m| {
            m.removed_packages
                .iter()
                .map(|(k, v)| (k.clone(), v.iter().cloned().collect()))
                .collect()
        })
        .unwrap_or_default();

    let mut pkg_map: HashMap<(String, String), Vec<String>> = HashMap::new();
    for machine in &app.state.machines {
        if machine.machine_id == current_machine_id {
            continue;
        }
        for (manager_key, packages) in &machine.packages {
            if manager_key == "brew_taps" {
                continue;
            }
            let current_set = current_pkgs.get(manager_key);
            let removed_set = removed.get(manager_key);
            for pkg in packages {
                let has = current_set.map(|s| s.contains(pkg)).unwrap_or(false);
                let was_removed = removed_set.map(|s| s.contains(pkg)).unwrap_or(false);
                if !has && !was_removed {
                    pkg_map
                        .entry((manager_key.clone(), pkg.clone()))
                        .or_default()
                        .push(machine.machine_id.clone());
                }
            }
        }
    }

    let mut items: Vec<PkgImportItem> = pkg_map
        .into_iter()
        .map(|((manager_key, name), sources)| PkgImportItem {
            manager_key,
            name,
            sources,
        })
        .collect();
    items.sort_by(|a, b| a.manager_key.cmp(&b.manager_key).then(a.name.cmp(&b.name)));

    if !items.is_empty() {
        app.overlays.push(Overlay::PkgImport(PkgImport {
            items,
            cursor: 0,
            confirm: None,
        }));
    }
}

fn toggle_history(app: &mut App) {
    let rows = build_rows(&app.state, &app.packages);
    let Some(PkgRow::Header { manager_key, .. }) = rows.get(app.packages.cursor) else {
        return;
    };
    let manager_key = manager_key.clone();
    let was_open = app.packages.history_manager.as_deref() == Some(manager_key.as_str());
    app.packages.history_commit = None;
    app.packages.history_diff.clear();
    if was_open {
        app.packages.history_manager = None;
        app.packages.history.clear();
    } else {
        app.packages.history = repo::pkg_history(&manager_key);
        app.packages.history_manager = Some(manager_key.clone());
    }
    // Rows above the cursor may have vanished, so re-find the header.
    app.packages.cursor = build_rows(&app.state, &app.packages)
        .iter()
        .position(|r| matches!(r, PkgRow::Header { manager_key: k, .. } if *k == manager_key))
        .unwrap_or(0);
}

/// Reload open manifest history and clamp the cursor after a state reload.
pub fn refresh_expanded(app: &mut App) {
    if let Some(ref manager) = app.packages.history_manager {
        app.packages.history = repo::pkg_history(manager);
        if let Some(ref commit) = app.packages.history_commit {
            app.packages.history_diff = repo::pkg_diff(manager, commit);
        }
    }
    let len = build_rows(&app.state, &app.packages).len();
    clamp_cursor(&mut app.packages.cursor, len);
}

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
}

/// Build the flat list of rows from machine state
pub fn build_rows(state: &DashboardState, pt: &PackagesTabState) -> Vec<PkgRow> {
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

        if pt.history_manager.as_deref() == Some(key.as_str()) {
            for entry in &pt.history {
                rows.push(PkgRow::HistoryEntry {
                    commit_hash: entry.commit_hash.clone(),
                    short_hash: entry.short_hash.clone(),
                    date: relative_time(entry.date),
                    machine_id: entry.machine_id.clone(),
                    message: entry.message.clone(),
                });
                if pt.history_commit.as_deref() == Some(entry.commit_hash.as_str()) {
                    for line in &pt.history_diff {
                        rows.push(PkgRow::DiffRow { line: line.clone() });
                    }
                }
            }
        }

        if pt.expanded.as_deref() == Some(key.as_str()) {
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

pub fn render(f: &mut Frame, area: Rect, app: &App) {
    let t = &app.theme;
    let pt = &app.packages;
    let rows = build_rows(&app.state, pt);
    let cursor = pt.cursor;

    let block = Block::default()
        .title(" Packages ")
        .borders(Borders::ALL)
        .border_style(Style::default().fg(t.border));
    let inner_area = block.inner(area);
    f.render_widget(block, area);

    if rows.is_empty() {
        let msg = Paragraph::new(Span::styled(
            "  No package data for this machine",
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
            PkgRow::Header {
                manager_key,
                label,
                count,
                ..
            } => {
                let arrow = if pt.expanded.as_deref() == Some(manager_key.as_str()) {
                    "v"
                } else {
                    ">"
                };
                let style = if is_selected {
                    Style::default().fg(t.accent).bg(t.selection).bold()
                } else {
                    Style::default().fg(t.accent).bold()
                };
                let bg_style = if is_selected {
                    Style::default().bg(t.selection)
                } else {
                    Style::default()
                };
                let line = Line::from(vec![
                    Span::styled(format!("  {} {} ", arrow, label), style),
                    Span::styled(
                        format!("({})", count),
                        if is_selected {
                            Style::default().fg(t.selection).bg(t.selection)
                        } else {
                            Style::default().fg(t.muted)
                        },
                    ),
                    Span::styled(" ".repeat(inner_area.width as usize), bg_style),
                ]);
                f.render_widget(Paragraph::new(line), row_area);
            }
            PkgRow::Package { name, .. } => {
                let style = if is_selected {
                    Style::default().fg(t.text).bg(t.selection)
                } else {
                    Style::default().fg(t.text)
                };
                let line = Line::from(vec![
                    Span::styled(format!("      {}", name), style),
                    Span::styled(" ".repeat(inner_area.width as usize), style),
                ]);
                f.render_widget(Paragraph::new(line), row_area);
            }
            PkgRow::HistoryEntry {
                commit_hash,
                short_hash,
                date,
                machine_id,
                message,
            } => {
                let bg = if is_selected { t.selection } else { t.base_bg };
                let arrow = if pt.history_commit.as_deref() == Some(commit_hash.as_str()) {
                    "v"
                } else {
                    ">"
                };
                let line = Line::from(vec![
                    Span::styled(
                        format!("     {} ", arrow),
                        Style::default().fg(t.muted).bg(bg),
                    ),
                    Span::styled(
                        short_hash.clone(),
                        Style::default().fg(t.hash).bg(bg).bold(),
                    ),
                    Span::styled(
                        format!("  {:>12}", date),
                        Style::default().fg(t.muted).bg(bg),
                    ),
                    Span::styled(
                        format!("  {:15}", machine_id),
                        Style::default().fg(t.muted).bg(bg),
                    ),
                    Span::styled(format!("  {}", message), Style::default().fg(t.text).bg(bg)),
                    Span::styled(
                        " ".repeat(inner_area.width as usize),
                        Style::default().bg(bg),
                    ),
                ]);
                f.render_widget(Paragraph::new(line), row_area);
            }
            PkgRow::DiffRow { line: diff_line } => {
                let bg = if is_selected { t.selection } else { t.base_bg };
                let line = Line::from(vec![
                    Span::styled("        ", Style::default().bg(bg)),
                    Span::styled(
                        diff_line.clone(),
                        Style::default().fg(t.diff_fg(diff_line)).bg(bg),
                    ),
                    Span::styled(
                        " ".repeat(inner_area.width as usize),
                        Style::default().bg(bg),
                    ),
                ]);
                f.render_widget(Paragraph::new(line), row_area);
            }
        }
    }
}

/// Simple overview render (for the Overview tab) - shows manager summary
pub fn render_overview(f: &mut Frame, area: Rect, app: &App) {
    let t = &app.theme;
    let state = &app.state;
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
                    Style::default().fg(t.muted),
                ))]
            } else {
                managers
                    .into_iter()
                    .map(|(key, packages)| {
                        let label = manager_label(key);
                        ListItem::new(Line::from(vec![
                            Span::styled(format!(" {} ", label), Style::default().fg(t.accent)),
                            Span::raw("  "),
                            Span::styled(
                                format!("{} packages", packages.len()),
                                Style::default().fg(t.muted),
                            ),
                        ]))
                    })
                    .collect()
            }
        }
        None => vec![ListItem::new(Span::styled(
            "  No package data",
            Style::default().fg(t.muted),
        ))],
    };

    let list = List::new(items).block(
        Block::default()
            .title(" Packages ")
            .borders(Borders::ALL)
            .border_style(Style::default().fg(t.border)),
    );
    f.render_widget(list, area);
}

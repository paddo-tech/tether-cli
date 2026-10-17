use super::confirm::Confirm;
use super::machines::{
    command_refusal, confirm_remove, confirm_trust, confirm_trust_fleet, display_name,
    is_old_build, is_old_id, key_state, profile_of, FleetTarget,
};
use super::pkg_import::{PkgImport, PkgImportItem};
use super::profile_picker::ProfilePicker;
use super::{clamp_cursor, cursor_down, manager_label, panel};
use crate::cli::output::relative_time;
use crate::dashboard::app::{App, Overlay};
use crate::dashboard::msg::KeyOutcome;
use crate::sync::commands::{Action, Pending};
use crate::sync::state::{CommandResult, CommandStatus};
use crate::sync::MachineState;
use crossterm::event::{KeyCode, KeyEvent};
use ratatui::{
    prelude::*,
    widgets::{Paragraph, Wrap},
};

/// The other machine whose details are open. The keys then act on it and its detail list.
pub fn open_detail(app: &App) -> Option<&MachineState> {
    let id = app.machines.expanded.as_deref()?;
    if id == app.machine_id() {
        return None;
    }
    app.state.machines.iter().find(|m| m.machine_id == id)
}

/// One row of another machine's details.
pub enum DetailRow {
    Pending(Pending),
    Result(CommandResult),
    Manager { key: String, count: usize },
    Package { manager_key: String, name: String },
}

/// The commands that wait for `m`, the results it reported, newest first, then its
/// packages by manager.
pub fn detail_rows(app: &App, m: &MachineState) -> Vec<DetailRow> {
    let mut rows: Vec<DetailRow> = app
        .state
        .pending_commands
        .iter()
        .filter(|(id, _)| *id == m.machine_id)
        .flat_map(|(_, pending)| pending.iter().cloned().map(DetailRow::Pending))
        .collect();
    let mut results: Vec<CommandResult> = m.commands_applied.values().flatten().cloned().collect();
    results.sort_by_key(|r| std::cmp::Reverse(r.at));
    rows.extend(results.into_iter().map(DetailRow::Result));
    let mut managers: Vec<_> = m.packages.iter().filter(|(_, p)| !p.is_empty()).collect();
    managers.sort_by(|a, b| a.0.cmp(b.0));
    for (key, packages) in managers {
        rows.push(DetailRow::Manager {
            key: key.clone(),
            count: packages.len(),
        });
        let mut names = packages.clone();
        names.sort();
        rows.extend(names.into_iter().map(|name| DetailRow::Package {
            manager_key: key.clone(),
            name,
        }));
    }
    rows
}

/// Keys while another machine's details are open. Up and down move in its list, left and
/// right move to the next card and open its details.
pub fn detail_key(app: &mut App, key: KeyEvent) -> KeyOutcome {
    let len = open_detail(app).map_or(0, |m| detail_rows(app, m).len());
    match key.code {
        KeyCode::Enter | KeyCode::Esc => app.machines.expanded = None,
        KeyCode::Char('j') | KeyCode::Down => cursor_down(&mut app.machines.detail_cursor, len),
        KeyCode::Char('k') | KeyCode::Up => {
            app.machines.detail_cursor = app.machines.detail_cursor.saturating_sub(1)
        }
        KeyCode::Char('l') | KeyCode::Right => move_card(app, true),
        KeyCode::Char('h') | KeyCode::Left => move_card(app, false),
        KeyCode::Char('x') => confirm_remote_uninstall(app),
        KeyCode::Char('i') => open_remote_import(app),
        KeyCode::Char('p') => {
            if let Some(m) = open_detail(app).cloned() {
                open_remote_profile(app, &m);
            }
        }
        KeyCode::Char('c') => confirm_cancel(app),
        KeyCode::Char('a') => confirm_trust(app),
        KeyCode::Char('A') => confirm_trust_fleet(app),
        KeyCode::Char('D') => {
            if let Some(id) = app.machines.expanded.clone() {
                confirm_remove(app, &id);
            }
        }
        _ => return KeyOutcome::Ignored,
    }
    KeyOutcome::Handled(None)
}

/// Select the next or previous card and open its details.
fn move_card(app: &mut App, forward: bool) {
    let len = app.state.machines.len();
    let c = &mut app.machines.cursor;
    if forward && *c + 1 < len {
        *c += 1;
    } else if !forward {
        *c = c.saturating_sub(1);
    }
    show_details_of_cursor(app);
}

/// Open the details of the card at the cursor, at the top of its list.
pub fn show_details_of_cursor(app: &mut App) {
    clamp_cursor(&mut app.machines.cursor, app.state.machines.len());
    app.machines.expanded = app
        .state
        .machines
        .get(app.machines.cursor)
        .map(|m| m.machine_id.clone());
    app.machines.detail_cursor = 0;
}

/// The detail row at the cursor, with the machine whose details are open.
fn detail_selection(app: &App) -> Option<(MachineState, Option<DetailRow>)> {
    let m = open_detail(app)?;
    let row = detail_rows(app, m)
        .into_iter()
        .nth(app.machines.detail_cursor);
    Some((m.clone(), row))
}

/// Ask before uninstalling the selected package on the machine whose details are open.
fn confirm_remote_uninstall(app: &mut App) {
    let Some((m, row)) = detail_selection(app) else {
        return;
    };
    let label = display_name(&m);
    match row {
        Some(DetailRow::Package { manager_key, name }) => {
            if manager_key == "brew_taps" {
                app.flash_info("Tether does not uninstall taps");
            } else if let Some(why) = command_refusal(app, &m) {
                app.flash_error(why);
            } else {
                app.overlays.push(Overlay::Confirm(Confirm::Fleet {
                    action: Action::Uninstall {
                        manager: manager_key,
                        name,
                    },
                    local: false,
                    targets: vec![FleetTarget::of(&m)],
                    arming: Default::default(),
                }));
            }
        }
        Some(DetailRow::Pending(_)) => app.flash_info("Press c to cancel a pending command"),
        _ => app.flash_info(format!("Select a package to uninstall it on {}", label)),
    }
}

/// Offer the packages other machines have and the machine whose details are open lacks.
fn open_remote_import(app: &mut App) {
    let Some(m) = open_detail(app).cloned() else {
        return;
    };
    if let Some(why) = command_refusal(app, &m) {
        app.flash_error(why);
        return;
    }
    let mut sources: std::collections::BTreeMap<(String, String), Vec<String>> = Default::default();
    for other in app
        .state
        .machines
        .iter()
        .filter(|o| o.machine_id != m.machine_id)
    {
        for (manager_key, packages) in &other.packages {
            if manager_key == "brew_taps" {
                continue;
            }
            let has: std::collections::HashSet<String> = m
                .packages
                .get(manager_key)
                .into_iter()
                .flatten()
                .map(|n| crate::sync::membership::canonical_id(manager_key, n))
                .collect();
            for name in packages
                .iter()
                .filter(|n| !has.contains(&crate::sync::membership::canonical_id(manager_key, n)))
            {
                sources
                    .entry((manager_key.clone(), name.clone()))
                    .or_default()
                    .push(other.hostname.clone());
            }
        }
    }
    if sources.is_empty() {
        app.flash_info(format!(
            "{} has every package that other machines list",
            display_name(&m)
        ));
        return;
    }
    let items = sources
        .into_iter()
        .map(|((manager_key, name), sources)| PkgImportItem {
            manager_key,
            name,
            sources,
        })
        .collect();
    app.overlays.push(Overlay::PkgImport(PkgImport {
        items,
        cursor: 0,
        confirm: None,
        target: Some(FleetTarget::of(&m)),
    }));
}

/// Pick a profile for another machine, starting on its current one.
pub fn open_remote_profile(app: &mut App, m: &MachineState) {
    if let Some(why) = command_refusal(app, m) {
        app.flash_error(why);
        return;
    }
    let Some(config) = &app.state.config else {
        return;
    };
    let mut options: Vec<String> = config.profiles.keys().cloned().collect();
    options.sort();
    if options.is_empty() {
        app.flash_info("No profiles. Create one: tether machines profile create <name>");
        return;
    }
    let current = profile_of(app, m);
    let cursor = options.iter().position(|o| *o == current).unwrap_or(0);
    app.overlays.push(Overlay::ProfilePicker(ProfilePicker {
        options,
        cursor,
        target: Some(FleetTarget::of(m)),
    }));
}

/// Ask before cancelling the selected pending command, which this machine sent.
fn confirm_cancel(app: &mut App) {
    let Some((m, row)) = detail_selection(app) else {
        return;
    };
    let Some(DetailRow::Pending(p)) = row else {
        app.flash_info("Select a pending command to cancel it");
        return;
    };
    if p.command.signer != app.machine_id() {
        app.flash_info(format!("Only {} can cancel this command", p.command.signer));
        return;
    }
    app.overlays.push(Overlay::Confirm(Confirm::CancelCommand {
        target: FleetTarget::of(&m),
        seq: p.command.seq,
        action: p.command.action.describe(),
        arming: Default::default(),
    }));
}

pub fn render_detail(f: &mut Frame, area: Rect, app: &App, m: &MachineState) {
    let t = &app.theme;
    let block = panel(format!(" {} ", display_name(m)), true, t);
    let mut inner = block.inner(area);
    f.render_widget(block, area);
    let note = if is_old_id(app, &m.machine_id) {
        Some(format!(
            "May be an old id of this machine. Tether guesses: it matches this hostname, comes \
             from an old build and has not synced for over {} days. Check that no other machine \
             uses this hostname. Its packages still count for every machine. Press D to remove it.",
            crate::sync::state::OLD_ID_SILENT_DAYS
        ))
    } else if is_old_build(app, &m.machine_id) {
        Some(format!(
            "This machine is {}.",
            crate::sync::signing::OLD_BUILD_NOTE
        ))
    } else {
        None
    };
    if let Some(note) = note {
        // A blank line under the note, which word wrap may take
        let h = (note.chars().count().div_ceil(inner.width.max(1) as usize) as u16 + 1)
            .min(inner.height);
        f.render_widget(
            Paragraph::new(Span::styled(note, Style::default().fg(t.warn)))
                .wrap(Wrap { trim: true }),
            Rect { height: h, ..inner },
        );
        inner.y += h;
        inner.height -= h;
    }
    let kv = |k: &str, v: String| {
        Line::from(vec![
            Span::styled(format!("{:<14}", k), Style::default().fg(t.dim)),
            Span::styled(v, Style::default().fg(t.text)),
        ])
    };

    // The whole fingerprint, so it can be compared with the one that machine shows
    let (word, color, fingerprint) = key_state(app, m);
    let mut key = vec![
        Span::styled(format!("{:<14}", "Key"), Style::default().fg(t.dim)),
        Span::styled(word, Style::default().fg(color)),
    ];
    if let Some(fp) = fingerprint {
        key.push(Span::raw("  "));
        key.push(Span::styled(fp, Style::default().fg(t.hash)));
    }
    let key = Line::from(key);
    let key_h = (key.width().div_ceil(inner.width.max(1) as usize) as u16 + 1).min(inner.height);
    f.render_widget(
        Paragraph::new(key).wrap(Wrap { trim: false }),
        Rect {
            height: key_h,
            ..inner
        },
    );
    inner.y += key_h;
    inner.height -= key_h;

    let [info, right] =
        Layout::horizontal([Constraint::Percentage(45), Constraint::Percentage(55)])
            .spacing(2)
            .areas(inner);
    let mut lines = vec![
        kv("Hostname", m.hostname.clone()),
        kv("Machine ID", m.machine_id.clone()),
        kv("Last sync", relative_time(m.last_sync)),
        kv("Files", m.files.len().to_string()),
    ];
    let mut spans = vec![Span::styled(
        format!("Dotfiles ({})  ", m.dotfiles.len()),
        Style::default().fg(t.accent).bold(),
    )];
    for d in &m.dotfiles {
        spans.push(Span::styled(d.as_str(), Style::default().fg(t.muted)));
        spans.push(Span::styled("  ", Style::default()));
    }
    // Another machine's details list its commands and packages, which the keys act on
    if m.machine_id != app.machine_id() {
        lines.push(kv("Profile", profile_of(app, m)));
        let [kvs, dots] = Layout::vertical([
            Constraint::Length(lines.len() as u16 + 1),
            Constraint::Min(0),
        ])
        .areas(info);
        f.render_widget(Paragraph::new(lines), kvs);
        f.render_widget(
            Paragraph::new(Line::from(spans)).wrap(Wrap { trim: true }),
            dots,
        );
        render_detail_list(f, right, app, m);
        return;
    }
    let mut managers: Vec<_> = m.packages.iter().collect();
    managers.sort_by(|a, b| a.0.cmp(b.0));
    for (key, pkgs) in managers {
        lines.push(kv(manager_label(key), pkgs.len().to_string()));
    }
    f.render_widget(Paragraph::new(lines), info);
    f.render_widget(
        Paragraph::new(Line::from(spans)).wrap(Wrap { trim: true }),
        right,
    );
}

/// Another machine's pending commands, its results and its packages, with the cursor row
/// kept in view.
fn render_detail_list(f: &mut Frame, area: Rect, app: &App, m: &MachineState) {
    let t = &app.theme;
    let rows = detail_rows(app, m);
    if rows.is_empty() {
        f.render_widget(
            Paragraph::new(Span::styled(
                "No commands and no packages",
                Style::default().fg(t.dim),
            )),
            area,
        );
        return;
    }
    let cursor = app.machines.detail_cursor.min(rows.len() - 1);
    let visible = area.height.max(1) as usize;
    let first = (cursor + 1).saturating_sub(visible);
    let dim = |s: String| Span::styled(s, Style::default().fg(t.dim));
    for (i, r) in rows.iter().enumerate().skip(first).take(visible) {
        let line = match r {
            DetailRow::Pending(p) => {
                let mut spans = vec![
                    Span::styled("◷ pending  ", Style::default().fg(t.info)),
                    Span::styled(p.command.action.describe(), Style::default().fg(t.text)),
                    dim(format!(
                        "  from {} · expires in {}",
                        p.command.signer,
                        time_left(p.command.expires_at)
                    )),
                ];
                if !p.verified {
                    spans.push(Span::styled(
                        " · not signed by a trusted key",
                        Style::default().fg(t.warn),
                    ));
                }
                Line::from(spans)
            }
            DetailRow::Result(r) => {
                let (mark, color) = match r.status {
                    CommandStatus::Applied => ("✓", t.ok),
                    CommandStatus::Failed => ("✗", t.error),
                    CommandStatus::Expired => ("–", t.warn),
                };
                let mut spans = vec![
                    Span::styled(
                        format!("{} {}  ", mark, r.status.label()),
                        Style::default().fg(color),
                    ),
                    Span::styled(r.action.clone(), Style::default().fg(t.text)),
                    dim(format!(" · {}", relative_time(r.at))),
                ];
                if let Some(error) = &r.error {
                    spans.push(Span::styled(
                        format!(": {}", error),
                        Style::default().fg(t.error),
                    ));
                }
                Line::from(spans)
            }
            DetailRow::Manager { key, count } => Line::from(Span::styled(
                format!("{} ({})", manager_label(key), count),
                Style::default().fg(t.accent).bold(),
            )),
            DetailRow::Package { name, .. } => Line::from(Span::styled(
                format!("  {}", name),
                Style::default().fg(t.text),
            )),
        };
        let rect = Rect::new(area.x, area.y + (i - first) as u16, area.width, 1);
        let mut p = Paragraph::new(line);
        if i == cursor {
            p = p.style(Style::default().bg(t.selection));
        }
        f.render_widget(p, rect);
    }
}

/// Time until `at`, in hours under a day, else in days.
fn time_left(at: chrono::DateTime<chrono::Utc>) -> String {
    let left = at.signed_duration_since(chrono::Utc::now());
    if left.num_hours() < 24 {
        format!("{}h", left.num_hours().max(0))
    } else {
        format!("{}d", left.num_days())
    }
}

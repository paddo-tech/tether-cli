use super::confirm::Confirm;
use super::profile_picker::ProfilePicker;
use super::{clamp_cursor, manager_label, panel, row, security::pill, truncate};
use crate::cli::output::relative_time;
use crate::dashboard::app::{App, Hit, Overlay};
use crate::dashboard::msg::KeyOutcome;
use crate::packages::inbox::{Kind, Reason};
use crate::sync::signing::RecordStatus;
use crate::sync::MachineState;
use crossterm::event::{KeyCode, KeyEvent};
use ratatui::{
    prelude::*,
    widgets::{Block, BorderType, Borders, Padding, Paragraph, Wrap},
};

const CARD_MIN_W: u16 = 46;
/// Borders and six info lines.
const CARD_H: u16 = 8;

#[derive(Default)]
pub struct MachinesTabState {
    /// Index into `state.machines`.
    pub cursor: usize,
    pub expanded: Option<String>,
}

/// How recently a machine checked in.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Presence {
    Online,
    Idle,
    Stale,
}

/// A running daemon rewrites an unchanged record only every hour (the heartbeat), so a
/// record up to 90 minutes old (heartbeat, one 5-minute tick and push delay) is online,
/// and a day without one means the machine is likely off or uninstalled.
pub fn presence(age: chrono::Duration) -> Presence {
    if age <= chrono::Duration::minutes(crate::sync::state::RECORD_HEARTBEAT_MINUTES + 30) {
        Presence::Online
    } else if age <= chrono::Duration::hours(24) {
        Presence::Idle
    } else {
        Presence::Stale
    }
}

/// Cards per grid row for a body this wide.
pub fn grid_cols(width: u16) -> usize {
    (width.saturating_add(1) / (CARD_MIN_W + 1)).max(1) as usize
}

pub fn handle_key(app: &mut App, key: KeyEvent) -> KeyOutcome {
    let len = app.state.machines.len();
    let cols = grid_cols(app.viewport.width.saturating_sub(2));
    let c = &mut app.machines.cursor;
    match key.code {
        KeyCode::Enter => toggle_expand(app),
        KeyCode::Esc if app.machines.expanded.is_some() => app.machines.expanded = None,
        KeyCode::Char('p') => open_profile_picker(app),
        KeyCode::Char('a') => confirm_trust(app),
        KeyCode::Char('x') => confirm_untrust(app),
        KeyCode::Char('D') => {
            clamp_cursor(&mut app.machines.cursor, len);
            if let Some(id) = app
                .state
                .machines
                .get(app.machines.cursor)
                .map(|m| m.machine_id.clone())
            {
                confirm_remove(app, &id);
            }
        }
        KeyCode::Char('j') | KeyCode::Down => {
            if *c + cols < len {
                *c += cols;
            }
        }
        KeyCode::Char('k') | KeyCode::Up => *c = c.saturating_sub(cols),
        KeyCode::Char('l') | KeyCode::Right => {
            if *c + 1 < len {
                *c += 1;
            }
        }
        KeyCode::Char('h') | KeyCode::Left => *c = c.saturating_sub(1),
        _ => return KeyOutcome::Ignored,
    }
    KeyOutcome::Handled(None)
}

fn toggle_expand(app: &mut App) {
    clamp_cursor(&mut app.machines.cursor, app.state.machines.len());
    let Some(m) = app.state.machines.get(app.machines.cursor) else {
        return;
    };
    if app.machines.expanded.as_deref() == Some(m.machine_id.as_str()) {
        app.machines.expanded = None;
    } else {
        app.machines.expanded = Some(m.machine_id.clone());
    }
}

/// The machine at the cursor, with its display name, unless it is this machine.
fn other_machine(app: &mut App, this: &str) -> Option<(String, String)> {
    clamp_cursor(&mut app.machines.cursor, app.state.machines.len());
    let m = app.state.machines.get(app.machines.cursor)?;
    let found = (m.machine_id.clone(), display_name(m));
    if found.0 == app.machine_id() {
        app.flash_info(this);
        return None;
    }
    Some(found)
}

/// Ask before trusting the key that signs the selected machine's record, with its full
/// fingerprint. An Inbox item for that key is answered as the Security tab answers it.
fn confirm_trust(app: &mut App) {
    let Some((id, label)) = other_machine(app, "This machine trusts its own key") else {
        return;
    };
    let pending = app
        .state
        .inbox
        .items
        .iter()
        .find(|i| i.name == id && matches!(i.kind, Kind::TrustMachine { .. }));
    let confirm = match pending {
        Some(item) => {
            let Kind::TrustMachine { fingerprint, .. } = &item.kind else {
                return;
            };
            Confirm::Trust {
                machine_id: id,
                label,
                fingerprint: fingerprint.clone(),
                changed: item.reasons.contains(&Reason::KeyChanged),
                item: Some(Box::new(item.clone())),
                arming: Default::default(),
            }
        }
        None if app.state.trusted.iter().any(|k| k.machine_id == id) => {
            app.flash_info(format!("{} is trusted already", label));
            return;
        }
        None => {
            let fingerprint = crate::sync::SyncEngine::sync_path()
                .ok()
                .and_then(|p| crate::packages::inbox::signing_fingerprint(&p, &id));
            let Some(fingerprint) = fingerprint else {
                app.flash_info(format!("{} has no signed machine record", label));
                return;
            };
            Confirm::Trust {
                machine_id: id,
                label,
                fingerprint,
                changed: false,
                item: None,
                arming: Default::default(),
            }
        }
    };
    app.overlays.push(Overlay::Confirm(confirm));
}

/// Ask before removing the selected machine's key from the trust store.
fn confirm_untrust(app: &mut App) {
    let Some((id, label)) = other_machine(app, "This machine cannot untrust itself") else {
        return;
    };
    let Some(key) = app.state.trusted.iter().find(|k| k.machine_id == id) else {
        app.flash_info(format!("{} is not trusted", label));
        return;
    };
    let confirm = Confirm::Untrust {
        machine_id: id,
        label,
        fingerprint: key.fingerprint.clone(),
        arming: Default::default(),
    };
    app.overlays.push(Overlay::Confirm(confirm));
}

/// Pick this machine's profile, starting on the current one.
pub fn open_profile_picker(app: &mut App) {
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

/// Ask before removing a record that looks like an old id of this machine. Other records
/// stay: only the CLI removes another machine.
pub fn confirm_remove(app: &mut App, machine_id: &str) {
    if machine_id == app.machine_id() {
        app.flash_error("Cannot remove this machine's current record");
        return;
    }
    let Some(digest) = app
        .state
        .old_ids
        .iter()
        .find(|o| o.machine_id == machine_id)
        .map(|o| o.digest.clone())
    else {
        app.flash_info(format!(
            "{} does not look like an old id of this machine. Remove it with 'tether machines remove {}'",
            machine_id, machine_id
        ));
        return;
    };
    let Some(m) = app
        .state
        .machines
        .iter()
        .find(|m| m.machine_id == machine_id)
    else {
        return;
    };
    let confirm = Confirm::RemoveMachine {
        machine_id: m.machine_id.clone(),
        hostname: m.hostname.clone(),
        last_sync: m.last_sync,
        packages: m.packages.values().map(|v| v.len()).sum(),
        digest,
        arming: Default::default(),
    };
    app.overlays.push(Overlay::Confirm(confirm));
}

pub fn is_old_id(app: &App, machine_id: &str) -> bool {
    app.state.old_ids.iter().any(|o| o.machine_id == machine_id)
}

pub fn is_old_build(app: &App, machine_id: &str) -> bool {
    app.state.old_builds.iter().any(|id| id == machine_id)
}

pub fn display_name(m: &MachineState) -> String {
    let host = m.hostname.trim_end_matches(".local");
    if host.is_empty() {
        m.machine_id.clone()
    } else {
        host.to_string()
    }
}

fn profile_of(app: &App, m: &MachineState) -> String {
    m.profile
        .clone()
        .or_else(|| {
            app.state
                .config
                .as_ref()
                .map(|c| c.profile_name(&m.machine_id).to_string())
        })
        .unwrap_or_else(|| crate::config::DEFAULT_PROFILE.to_string())
}

/// Grid of machine cards; Enter opens a detail panel for the selected card.
pub fn render(f: &mut Frame, area: Rect, app: &App) {
    let t = &app.theme;
    let machines = &app.state.machines;
    if machines.is_empty() {
        let block = panel(" Machines ", true, t);
        let inner = block.inner(area);
        f.render_widget(block, area);
        f.render_widget(
            Paragraph::new(Span::styled(
                "No machines found",
                Style::default().fg(t.dim),
            )),
            inner,
        );
        return;
    }
    let cursor = app.machines.cursor.min(machines.len() - 1);
    let detail = app
        .machines
        .expanded
        .as_deref()
        .and_then(|id| machines.iter().find(|m| m.machine_id == id));

    // The detail panel sits right under the cards and takes the remaining height.
    let (grid, detail_area) = match detail {
        Some(_) if area.height >= CARD_H + 6 => {
            let needed = machines.len().div_ceil(grid_cols(area.width)) as u16 * CARD_H;
            // Whole card rows only, so no blank band sits above the detail panel.
            let grid_h = (needed.min(area.height - 6) / CARD_H).max(1) * CARD_H;
            let [g, d] =
                Layout::vertical([Constraint::Length(grid_h), Constraint::Min(6)]).areas(area);
            (g, Some(d))
        }
        _ => (area, None),
    };

    let cols = grid_cols(grid.width);
    let gap = 1u16;
    let card_w = (grid.width.saturating_sub(gap * (cols as u16 - 1))) / cols as u16;
    // The last column takes the rounding remainder so the grid fills the width.
    let last_w = grid
        .width
        .saturating_sub((card_w + gap) * (cols as u16 - 1));
    let visible_rows = (grid.height / CARD_H).max(1) as usize;
    let first_row = (cursor / cols + 1).saturating_sub(visible_rows);
    let now = chrono::Utc::now();

    for (i, m) in machines.iter().enumerate().skip(first_row * cols) {
        let (r, c) = (i / cols - first_row, i % cols);
        if r >= visible_rows {
            break;
        }
        let rect = Rect::new(
            grid.x + c as u16 * (card_w + gap),
            grid.y + r as u16 * CARD_H,
            if c == cols - 1 { last_w } else { card_w },
            CARD_H.min(grid.bottom() - (grid.y + r as u16 * CARD_H)),
        );
        card(f, rect, app, m, i == cursor, now);
        app.add_hit(rect, Hit::Row(i));
    }

    if let (Some(m), Some(d)) = (detail, detail_area) {
        render_detail(f, d, app, m);
    }
}

fn card(
    f: &mut Frame,
    rect: Rect,
    app: &App,
    m: &MachineState,
    selected: bool,
    now: chrono::DateTime<chrono::Utc>,
) {
    let t = &app.theme;
    let is_current = m.machine_id == app.machine_id();
    let state = presence(now.signed_duration_since(m.last_sync));
    let (word, color) = match state {
        Presence::Online => ("online", t.ok),
        Presence::Idle => ("idle", t.warn),
        Presence::Stale => ("stale", t.error),
    };
    let border = if selected { t.border_focus } else { t.border };
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(border))
        .padding(Padding::horizontal(1))
        .title(Line::from(vec![
            Span::styled(" ● ", Style::default().fg(color)),
            Span::styled(
                truncate(&display_name(m), rect.width.saturating_sub(7) as usize),
                Style::default()
                    .fg(if selected { t.accent } else { t.text })
                    .bold(),
            ),
            Span::raw(" "),
        ]));
    let inner = block.inner(rect);
    f.render_widget(block, rect);
    if inner.height == 0 {
        return;
    }
    if selected {
        f.buffer_mut().set_style(
            rect.inner(Margin {
                horizontal: 1,
                vertical: 1,
            }),
            Style::default().bg(t.selection),
        );
    }

    let os = if m.os_version.is_empty() {
        "unknown OS".to_string()
    } else {
        m.os_version.clone()
    };
    let version = if m.cli_version.is_empty() {
        "—".to_string()
    } else {
        format!("v{}", m.cli_version)
    };
    let dim = |s: &str| Span::styled(s.to_string(), Style::default().fg(t.dim));
    let text = |s: String| Span::styled(s, Style::default().fg(t.text));
    let sep = || Span::styled(" · ", Style::default().fg(t.border));

    let old_id = is_old_id(app, &m.machine_id);
    let mut first = Vec::new();
    if is_current {
        first.push(pill("this", t.accent, t));
        first.push(Span::raw(" "));
    } else if old_id {
        first.push(pill("may be old id", t.warn, t));
        first.push(Span::raw(" "));
    } else if is_old_build(app, &m.machine_id) {
        first.push(pill("on 1.x", t.warn, t));
        first.push(Span::raw(" "));
    }
    first.push(Span::styled(
        os,
        Style::default().fg(if m.os_version.is_empty() {
            t.dim
        } else {
            t.muted
        }),
    ));

    let pkgs: usize = m.packages.values().map(|v| v.len()).sum();
    let mut managers: Vec<_> = m.packages.iter().filter(|(_, p)| !p.is_empty()).collect();
    managers.sort_by(|a, b| b.1.len().cmp(&a.1.len()).then(a.0.cmp(b.0)));
    let mut breakdown = Vec::new();
    for (key, list) in managers {
        if !breakdown.is_empty() {
            breakdown.push(sep());
        }
        breakdown.push(dim(manager_label(key)));
        breakdown.push(text(format!(" {}", list.len())));
    }
    if breakdown.is_empty() {
        breakdown.push(dim("no packages"));
    }
    if old_id {
        breakdown = vec![
            Span::styled(
                "no longer syncs, packages still count ",
                Style::default().fg(t.warn),
            ),
            Span::styled("D", t.key_hint()),
            dim(" remove"),
        ];
    }

    let (key_word, key_color, fingerprint) = key_state(app, m);
    let key_line = Line::from(vec![
        dim("key "),
        Span::styled(key_word, Style::default().fg(key_color)),
    ]);
    let room = (inner.width as usize).saturating_sub(key_line.width() + 2);
    let fingerprint = fingerprint
        .filter(|_| room >= 12)
        .map(|fp| truncate(&fp, room))
        .unwrap_or_default();

    let lines = [
        (
            Line::from(first),
            Line::from(Span::styled(version, Style::default().fg(t.info))),
        ),
        (
            Line::from(vec![
                dim("id "),
                Span::styled(m.machine_id.clone(), Style::default().fg(t.muted)),
            ]),
            Line::from(Span::styled(word, Style::default().fg(color).bold())),
        ),
        (
            Line::from(vec![dim("synced "), text(relative_time(m.last_sync))]),
            Line::from(vec![
                dim("profile "),
                Span::styled(profile_of(app, m), Style::default().fg(t.team)),
            ]),
        ),
        (
            Line::from(vec![
                text(m.files.len().to_string()),
                dim(" files"),
                sep(),
                text(pkgs.to_string()),
                dim(" packages"),
            ]),
            Line::from(vec![text(m.dotfiles.len().to_string()), dim(" dotfiles")]),
        ),
        (Line::from(breakdown), Line::default()),
        (
            key_line,
            Line::from(Span::styled(fingerprint, Style::default().fg(t.hash))),
        ),
    ];
    for (i, (l, r)) in lines.into_iter().enumerate().take(inner.height as usize) {
        row(
            f,
            Rect::new(inner.x, inner.y + i as u16, inner.width, 1),
            l,
            r,
        );
    }
}

/// How this machine treats the machine's signing key: a word, its color, and the key's
/// fingerprint when known.
fn key_state(app: &App, m: &MachineState) -> (&'static str, Color, Option<String>) {
    let t = &app.theme;
    if m.machine_id == app.machine_id() {
        return ("this machine", t.accent, None);
    }
    let pending = app.state.inbox.items.iter().find_map(|i| match &i.kind {
        Kind::TrustMachine { fingerprint, .. } if i.name == m.machine_id => {
            Some((i.reasons.contains(&Reason::KeyChanged), fingerprint.clone()))
        }
        _ => None,
    });
    let status = app
        .state
        .record_status
        .iter()
        .find(|(id, _)| *id == m.machine_id)
        .map(|(_, status)| *status);
    match (pending, status) {
        (Some((true, fp)), _) => ("changed, review in Security", t.error, Some(fp)),
        (Some((false, fp)), _) => ("new, not trusted yet", t.info, Some(fp)),
        (None, Some(status @ (RecordStatus::Replayed | RecordStatus::SignatureFailed))) => {
            (status.label(), t.error, None)
        }
        (None, _) => match app
            .state
            .trusted
            .iter()
            .find(|k| k.machine_id == m.machine_id)
        {
            Some(k) => ("trusted", t.ok, Some(k.fingerprint.clone())),
            None => ("not trusted", t.dim, None),
        },
    }
}

fn render_detail(f: &mut Frame, area: Rect, app: &App, m: &MachineState) {
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
    let [info, dots] = Layout::horizontal([Constraint::Percentage(45), Constraint::Percentage(55)])
        .spacing(2)
        .areas(inner);

    let kv = |k: &str, v: String| {
        Line::from(vec![
            Span::styled(format!("{:<14}", k), Style::default().fg(t.dim)),
            Span::styled(v, Style::default().fg(t.text)),
        ])
    };
    let mut lines = vec![
        kv("Hostname", m.hostname.clone()),
        kv("Machine ID", m.machine_id.clone()),
        kv("Last sync", relative_time(m.last_sync)),
        kv("Files", m.files.len().to_string()),
    ];
    let mut managers: Vec<_> = m.packages.iter().collect();
    managers.sort_by(|a, b| a.0.cmp(b.0));
    for (key, pkgs) in managers {
        lines.push(kv(manager_label(key), pkgs.len().to_string()));
    }
    f.render_widget(Paragraph::new(lines), info);

    let mut spans = vec![Span::styled(
        format!("Dotfiles ({})  ", m.dotfiles.len()),
        Style::default().fg(t.accent).bold(),
    )];
    for d in &m.dotfiles {
        spans.push(Span::styled(d.as_str(), Style::default().fg(t.muted)));
        spans.push(Span::styled("  ", Style::default()));
    }
    f.render_widget(
        Paragraph::new(Line::from(spans)).wrap(Wrap { trim: true }),
        dots,
    );
}

/// Machine list for the Overview tab: presence dot, name, last seen.
pub fn render_overview(f: &mut Frame, area: Rect, app: &App) {
    let t = &app.theme;
    let machines = &app.state.machines;
    let now = chrono::Utc::now();
    let online = machines
        .iter()
        .filter(|m| presence(now.signed_duration_since(m.last_sync)) == Presence::Online)
        .count();
    let block = panel(" Machines ", false, t).title_top(
        Line::from(vec![
            Span::styled(format!(" {}", online), Style::default().fg(t.ok)),
            Span::styled(
                format!("/{} online ", machines.len()),
                Style::default().fg(t.dim),
            ),
        ])
        .right_aligned(),
    );
    let inner = block.inner(area);
    f.render_widget(block, area);
    if machines.is_empty() {
        f.render_widget(
            Paragraph::new(Span::styled(
                "No machines found",
                Style::default().fg(t.dim),
            )),
            inner,
        );
        return;
    }
    for (i, m) in machines.iter().take(inner.height as usize).enumerate() {
        let color = match presence(now.signed_duration_since(m.last_sync)) {
            Presence::Online => t.ok,
            Presence::Idle => t.warn,
            Presence::Stale => t.error,
        };
        let current = m.machine_id == app.machine_id();
        // The accent marks this machine; a "this" label would cut the hostname short.
        let left = vec![
            Span::styled("● ", Style::default().fg(color)),
            Span::styled(
                display_name(m),
                if current {
                    Style::default().fg(t.accent).bold()
                } else {
                    Style::default().fg(t.text)
                },
            ),
        ];
        let mut right = Vec::new();
        if is_old_id(app, &m.machine_id) {
            right.push(Span::styled("old id? ", Style::default().fg(t.warn)));
        } else if is_old_build(app, &m.machine_id) {
            right.push(Span::styled("on 1.x ", Style::default().fg(t.warn)));
        }
        right.push(Span::styled(
            relative_time(m.last_sync),
            Style::default().fg(t.dim),
        ));
        row(
            f,
            Rect::new(inner.x, inner.y + i as u16, inner.width, 1),
            Line::from(left),
            Line::from(right),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn presence_thresholds() {
        assert_eq!(presence(chrono::Duration::minutes(3)), Presence::Online);
        assert_eq!(presence(chrono::Duration::minutes(75)), Presence::Online);
        assert_eq!(presence(chrono::Duration::minutes(91)), Presence::Idle);
        assert_eq!(presence(chrono::Duration::days(2)), Presence::Stale);
    }

    #[test]
    fn grid_fits_cards() {
        assert_eq!(grid_cols(20), 1);
        assert_eq!(grid_cols(78), 1);
        assert_eq!(grid_cols(98), 2);
        assert_eq!(grid_cols(158), 3);
    }
}

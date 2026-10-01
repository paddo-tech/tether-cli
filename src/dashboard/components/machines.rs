use super::profile_picker::ProfilePicker;
use super::{clamp_cursor, manager_label, panel, row, truncate};
use crate::cli::output::relative_time;
use crate::dashboard::app::{App, Hit, Overlay};
use crate::dashboard::msg::KeyOutcome;
use crate::sync::MachineState;
use crossterm::event::{KeyCode, KeyEvent};
use ratatui::{
    prelude::*,
    widgets::{Block, BorderType, Borders, Padding, Paragraph, Wrap},
};

const CARD_MIN_W: u16 = 36;
const CARD_H: u16 = 5;

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

/// Daemons sync every few minutes, so 15 quiet minutes means the machine is away,
/// and a day means it is likely off or uninstalled.
pub fn presence(age: chrono::Duration) -> Presence {
    if age <= chrono::Duration::minutes(15) {
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
        KeyCode::Char('p') => open_profile_picker(app),
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

fn display_name(m: &MachineState) -> String {
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
            let grid_h = needed.min(area.height - 6).max(CARD_H);
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
    let mut block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(border))
        .padding(Padding::horizontal(1))
        .title(Line::from(vec![
            Span::styled(" ● ", Style::default().fg(color)),
            Span::styled(
                truncate(
                    &display_name(m),
                    rect.width.saturating_sub(if is_current { 14 } else { 8 }) as usize,
                ),
                Style::default()
                    .fg(if selected { t.accent } else { t.text })
                    .bold(),
            ),
            Span::raw(" "),
        ]));
    if is_current {
        block = block.title_top(
            Line::from(Span::styled(" this ", Style::default().fg(t.accent))).right_aligned(),
        );
    }
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
    let pkgs: usize = m.packages.values().map(|v| v.len()).sum();
    let lines = [
        (
            Line::from(Span::styled(
                os,
                Style::default().fg(if m.os_version.is_empty() {
                    t.dim
                } else {
                    t.muted
                }),
            )),
            Line::from(Span::styled(version, Style::default().fg(t.info))),
        ),
        (
            Line::from(vec![
                Span::styled("seen ", Style::default().fg(t.dim)),
                Span::styled(relative_time(m.last_sync), Style::default().fg(t.text)),
            ]),
            Line::from(Span::styled(word, Style::default().fg(color).bold())),
        ),
        (
            Line::from(vec![
                Span::styled(format!("{}", m.files.len()), Style::default().fg(t.text)),
                Span::styled(" files  ", Style::default().fg(t.dim)),
                Span::styled(format!("{}", pkgs), Style::default().fg(t.text)),
                Span::styled(" pkgs", Style::default().fg(t.dim)),
            ]),
            Line::from(Span::styled(
                profile_of(app, m),
                Style::default().fg(t.team),
            )),
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

fn render_detail(f: &mut Frame, area: Rect, app: &App, m: &MachineState) {
    let t = &app.theme;
    let block = panel(format!(" {} ", display_name(m)), true, t);
    let inner = block.inner(area);
    f.render_widget(block, area);
    let [info, dots] = Layout::horizontal([Constraint::Percentage(40), Constraint::Percentage(60)])
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
        let mut left = vec![
            Span::styled("● ", Style::default().fg(color)),
            Span::styled(
                display_name(m),
                if current {
                    Style::default().fg(t.text).bold()
                } else {
                    Style::default().fg(t.text)
                },
            ),
        ];
        if current {
            left.push(Span::styled("  this", Style::default().fg(t.accent)));
        }
        row(
            f,
            Rect::new(inner.x, inner.y + i as u16, inner.width, 1),
            Line::from(left),
            Line::from(Span::styled(
                relative_time(m.last_sync),
                Style::default().fg(t.dim),
            )),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn presence_thresholds() {
        assert_eq!(presence(chrono::Duration::minutes(3)), Presence::Online);
        assert_eq!(presence(chrono::Duration::minutes(16)), Presence::Idle);
        assert_eq!(presence(chrono::Duration::days(2)), Presence::Stale);
    }

    #[test]
    fn grid_fits_cards() {
        assert_eq!(grid_cols(20), 1);
        assert_eq!(grid_cols(78), 2);
        assert_eq!(grid_cols(158), 4);
    }
}

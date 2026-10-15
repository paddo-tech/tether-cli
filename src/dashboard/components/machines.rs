use super::confirm::Confirm;
use super::pkg_import::{PkgImport, PkgImportItem};
use super::profile_picker::ProfilePicker;
use super::{clamp_cursor, cursor_down, manager_label, panel, row, security::pill, truncate};
use crate::cli::output::relative_time;
use crate::dashboard::app::{App, Hit, Overlay};
use crate::dashboard::msg::KeyOutcome;
use crate::packages::inbox::{InboxItem, Kind, Reason};
use crate::sync::commands::{Action, Pending};
use crate::sync::signing::RecordStatus;
use crate::sync::state::{CommandResult, CommandStatus};
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
    /// Row in the open details of another machine: its commands, then its packages.
    pub detail_cursor: usize,
}

/// A machine that a fleet command goes to, as a confirm shows it.
#[derive(Clone, Debug, PartialEq)]
pub struct FleetTarget {
    pub id: String,
    pub label: String,
    pub last_sync: chrono::DateTime<chrono::Utc>,
}

impl FleetTarget {
    pub fn of(m: &MachineState) -> Self {
        Self {
            id: m.machine_id.clone(),
            label: display_name(m),
            last_sync: m.last_sync,
        }
    }

    pub fn pair(&self) -> (String, String) {
        (self.id.clone(), self.label.clone())
    }
}

/// Why this machine cannot send fleet commands to `m`, as the CLI refuses it.
pub fn command_refusal(app: &App, m: &MachineState) -> Option<String> {
    let trusted = app
        .state
        .record_status
        .iter()
        .any(|(id, status)| *id == m.machine_id && *status == RecordStatus::Trusted);
    crate::sync::commands::refusal(app.machine_id(), &m.machine_id, Some(m), trusted)
}

/// The other machines that run fleet commands from this machine, except `except`.
pub fn fleet_targets(app: &App, except: &str) -> Vec<FleetTarget> {
    app.state
        .machines
        .iter()
        .filter(|m| m.machine_id != except && command_refusal(app, m).is_none())
        .map(FleetTarget::of)
        .collect()
}

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
    if open_detail(app).is_some() {
        return detail_key(app, key);
    }
    let len = app.state.machines.len();
    let cols = grid_cols(app.viewport.width.saturating_sub(2));
    let c = &mut app.machines.cursor;
    match key.code {
        KeyCode::Enter => toggle_expand(app),
        KeyCode::Esc => app.machines.expanded = None,
        KeyCode::Char('p') => {
            clamp_cursor(&mut app.machines.cursor, len);
            match app.state.machines.get(app.machines.cursor) {
                Some(m) if m.machine_id != app.machine_id() => {
                    let m = m.clone();
                    open_remote_profile(app, &m);
                }
                _ => open_profile_picker(app),
            }
        }
        KeyCode::Char('a') => confirm_trust(app),
        KeyCode::Char('A') => confirm_trust_fleet(app),
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

/// Keys while another machine's details are open. Up and down move in its list, left and
/// right move to the next card and open its details.
fn detail_key(app: &mut App, key: KeyEvent) -> KeyOutcome {
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
            let has = m
                .packages
                .get(manager_key)
                .map(|p| p.as_slice())
                .unwrap_or_default();
            for name in packages.iter().filter(|n| !has.contains(n)) {
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
fn open_remote_profile(app: &mut App, m: &MachineState) {
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

fn toggle_expand(app: &mut App) {
    clamp_cursor(&mut app.machines.cursor, app.state.machines.len());
    let Some(m) = app.state.machines.get(app.machines.cursor) else {
        return;
    };
    if app.machines.expanded.as_deref() == Some(m.machine_id.as_str()) {
        app.machines.expanded = None;
    } else {
        app.machines.expanded = Some(m.machine_id.clone());
        app.machines.detail_cursor = 0;
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

/// The machine that trust keys act on: the one whose details are open, else the card at
/// the cursor. `None`, with `this` as a toast, when it is this machine.
fn trust_subject(app: &mut App, this: &str) -> Option<(String, String)> {
    if let Some(m) = open_detail(app) {
        return Some((m.machine_id.clone(), display_name(m)));
    }
    other_machine(app, this)
}

/// The key to trust for a machine: the one an Inbox item asks for, the one this machine
/// trusts already, or the one that signs its record now.
enum TrustKey {
    Asked(Box<InboxItem>),
    Trusted(String),
    Signing(String),
}

fn trust_key(app: &App, id: &str) -> Option<TrustKey> {
    let pending = app
        .state
        .inbox
        .items
        .iter()
        .find(|i| i.name == id && matches!(i.kind, Kind::TrustMachine { .. }));
    if let Some(item) = pending {
        return Some(TrustKey::Asked(Box::new(item.clone())));
    }
    if let Some(k) = app.state.trusted.iter().find(|k| k.machine_id == id) {
        return Some(TrustKey::Trusted(k.fingerprint.clone()));
    }
    crate::sync::SyncEngine::sync_path()
        .ok()
        .and_then(|p| crate::packages::inbox::signing_fingerprint(&p, id))
        .map(TrustKey::Signing)
}

/// A trust confirm for `key`, sent also to `fleet`. An Inbox item for the key is answered
/// as the Security tab answers it.
fn trust_confirm(
    app: &App,
    id: String,
    label: String,
    key: TrustKey,
    fleet: Vec<FleetTarget>,
) -> Confirm {
    let (fingerprint, changed, item, trusted_here) = match key {
        TrustKey::Asked(item) => {
            let fingerprint = match &item.kind {
                Kind::TrustMachine { fingerprint, .. } => fingerprint.clone(),
                Kind::Package => String::new(),
            };
            let changed = item.reasons.contains(&Reason::KeyChanged);
            (fingerprint, changed, Some(item), false)
        }
        TrustKey::Trusted(fingerprint) => (fingerprint, false, None, true),
        TrustKey::Signing(fingerprint) => (fingerprint, false, None, false),
    };
    Confirm::Trust {
        ignored: ignored_record(app, &id),
        machine_id: id,
        label,
        fingerprint,
        changed,
        item,
        fleet,
        trusted_here,
        arming: Default::default(),
    }
}

/// Ask before trusting the key that signs the selected machine's record, with its full
/// fingerprint.
fn confirm_trust(app: &mut App) {
    let Some((id, label)) = trust_subject(app, "This machine trusts its own key") else {
        return;
    };
    match trust_key(app, &id) {
        Some(TrustKey::Trusted(_)) => app.flash_info(format!("{} is trusted already", label)),
        None => app.flash_info(format!("{} has no signed machine record", label)),
        Some(key) => {
            let confirm = trust_confirm(app, id, label, key, Vec::new());
            app.overlays.push(Overlay::Confirm(confirm));
        }
    }
}

/// Ask before trusting the selected machine's key here, unless this machine trusts it
/// already, and on every other machine that runs fleet commands.
fn confirm_trust_fleet(app: &mut App) {
    let Some((id, label)) = trust_subject(app, "This machine trusts its own key") else {
        return;
    };
    let Some(key) = trust_key(app, &id) else {
        app.flash_info(format!("{} has no signed machine record", label));
        return;
    };
    let fleet = fleet_targets(app, &id);
    if fleet.is_empty() {
        app.flash_info(match key {
            TrustKey::Trusted(_) => format!(
                "{} is trusted here already, and no other machine runs fleet commands",
                label
            ),
            _ => format!(
                "No other machine runs fleet commands. Press a to trust {} here",
                label
            ),
        });
        return;
    }
    let confirm = trust_confirm(app, id, label, key, fleet);
    app.overlays.push(Overlay::Confirm(confirm));
}

/// The status of a machine's record when a sync ignores it, as replayed or failing its
/// signature. Trusting a key for such a machine gets the loud warning.
pub fn ignored_record(app: &App, machine_id: &str) -> Option<RecordStatus> {
    app.state
        .record_status
        .iter()
        .find(|(id, _)| id == machine_id)
        .map(|(_, status)| *status)
        .filter(|s| matches!(s, RecordStatus::Replayed | RecordStatus::SignatureFailed))
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
    app.overlays.push(Overlay::ProfilePicker(ProfilePicker {
        options,
        cursor,
        target: None,
    }));
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

pub fn profile_of(app: &App, m: &MachineState) -> String {
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
    let pending = pending_count(app, &m.machine_id);
    if pending > 0 {
        first.push(pill(&format!("{} pending", pending), t.info, t));
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

/// Fleet commands that wait in the repo for a machine.
fn pending_count(app: &App, machine_id: &str) -> usize {
    app.state
        .pending_commands
        .iter()
        .find(|(id, _)| id == machine_id)
        .map_or(0, |(_, p)| p.len())
}

/// How this machine treats the machine's signing key: a word, its color, and the key's
/// fingerprint when known.
fn key_state(app: &App, m: &MachineState) -> (&'static str, Color, Option<String>) {
    let t = &app.theme;
    let trusted = app
        .state
        .trusted
        .iter()
        .find(|k| k.machine_id == m.machine_id)
        .map(|k| k.fingerprint.clone());
    // The key file, not the trust store: a record signed by another key must not show as ours
    if m.machine_id == app.machine_id() {
        return ("this machine", t.accent, app.state.own_fingerprint.clone());
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
        (None, _) => match trusted {
            Some(fp) => ("trusted", t.ok, Some(fp)),
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

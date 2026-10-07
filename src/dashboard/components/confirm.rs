use super::{centered, clamp_cursor, files, manager_label, popup, security};
use crate::dashboard::app::{App, DaemonOp, Hit, Job, Overlay};
use crate::dashboard::config_edit;
use crate::dashboard::msg::Cmd;
use crate::dashboard::repo::RollbackPlan;
use crate::sync::membership::Edit;
use crossterm::event::{KeyCode, KeyEvent};
use ratatui::{prelude::*, widgets::*};
use std::cell::Cell;
use std::collections::BTreeSet;
use std::time::{Duration, Instant};

/// How long a confirm that can open under the user's typing ignores keys after its first draw.
pub const ARM_DELAY: Duration = Duration::from_millis(400);

/// When a confirm was first drawn. These confirms can open after a background result, while
/// the user types elsewhere, so a key typed for something else must not answer them.
#[derive(Default)]
pub struct Arming {
    drawn: Cell<Option<Instant>>,
    /// A draw showed the armed buttons. Until then the loop keeps drawing, so the screen
    /// never shows a countdown that has ended.
    shown_armed: Cell<bool>,
}

impl Arming {
    pub fn drawn(&self, now: Instant) {
        if self.drawn.get().is_none() {
            self.drawn.set(Some(now));
        }
        if self.armed(now) {
            self.shown_armed.set(true);
        }
    }

    pub fn shown_armed(&self) -> bool {
        self.shown_armed.get()
    }

    /// Time left before keys count, or `None` once armed. A confirm never drawn is not armed.
    pub fn remaining(&self, now: Instant) -> Option<Duration> {
        match self.drawn.get() {
            None => Some(ARM_DELAY),
            Some(at) => ARM_DELAY
                .checked_sub(now.saturating_duration_since(at))
                .filter(|d| !d.is_zero()),
        }
    }

    pub fn armed(&self, now: Instant) -> bool {
        self.remaining(now).is_none()
    }

    #[cfg(test)]
    pub fn drawn_long_ago(&self) {
        self.drawn.set(Some(Instant::now() - ARM_DELAY));
    }
}

/// A yes/no question about one action. Every one changes this machine or what it trusts,
/// so only `y` accepts, once armed; Enter and a click on the tab underneath never do.
pub enum Confirm {
    Uninstall {
        manager_key: String,
        name: String,
        arming: Arming,
    },
    Restore {
        repo_path: String,
        dotfile: String,
        commit: String,
        short_hash: String,
        arming: Arming,
    },
    /// Copy a dotfile's backup over the file, as `tether restore` does.
    RestoreBackup {
        path: String,
        timestamp: String,
        arming: Arming,
    },
    Rollback {
        plan: RollbackPlan,
        arming: Arming,
    },
    RemoveFile {
        path: String,
        arming: Arming,
    },
    /// Install a package that OSV could not check.
    InstallWithoutOsv {
        manager_key: String,
        name: String,
        error: String,
        arming: Arming,
    },
    /// Approve and install inbox items that OSV could not check.
    /// Each item comes with its own OSV error, because one may warn of malicious releases
    /// while another only timed out.
    ApproveWithoutOsv {
        items: Vec<(crate::packages::inbox::InboxItem, String)>,
        arming: Arming,
    },
    /// Remove another machine's record that looks like an old id of this machine.
    RemoveMachine {
        machine_id: String,
        hostname: String,
        last_sync: chrono::DateTime<chrono::Utc>,
        packages: usize,
        /// SHA-256 of the record file shown
        digest: String,
        arming: Arming,
    },
    /// Approve and install these inbox items, as displayed when the confirm opened. It lists
    /// every one of them, because `y` approves exactly this list.
    ApproveAll {
        items: Vec<crate::packages::inbox::InboxItem>,
        /// The machine whose packages these are, for "approve all from <machine>"
        from: Option<String>,
        /// Packages that stay held: malicious, or from a record that fails its signature
        held: usize,
        /// First listed item
        scroll: usize,
        /// Items the last draw had room for
        rows: std::cell::Cell<usize>,
        arming: Arming,
    },
    /// Approve and install a package whose source machine's record fails its signature.
    ApproveSignatureFailed {
        item: Box<crate::packages::inbox::InboxItem>,
        arming: Arming,
    },
    /// Reject an Inbox item, as displayed.
    Reject {
        item: Box<crate::packages::inbox::InboxItem>,
        arming: Arming,
    },
    /// Trust the key with this fingerprint for a machine. `item` is the Inbox item that asks
    /// for it, when the user answers that item.
    Trust {
        machine_id: String,
        label: String,
        fingerprint: String,
        /// The machine was trusted with another key
        changed: bool,
        item: Option<Box<crate::packages::inbox::InboxItem>>,
        arming: Arming,
    },
    /// Remove an item from a Config list, as shown at `index`.
    RemoveListItem {
        list: &'static str,
        index: usize,
        /// The row as shown, to check that the list still has it at `index`
        item: String,
        /// The item without its options
        name: String,
        arming: Arming,
    },
    /// Stop the daemon, which syncs this machine every few minutes.
    StopDaemon {
        arming: Arming,
    },
    /// Stop trusting a machine's key.
    Untrust {
        machine_id: String,
        label: String,
        fingerprint: String,
        arming: Arming,
    },
}

impl Confirm {
    pub fn approve_all(
        items: Vec<crate::packages::inbox::InboxItem>,
        held: usize,
        from: Option<String>,
    ) -> Self {
        Confirm::ApproveAll {
            items,
            from,
            held,
            scroll: 0,
            rows: std::cell::Cell::new(1),
            arming: Arming::default(),
        }
    }

    pub fn approve_signature_failed(item: crate::packages::inbox::InboxItem) -> Self {
        Confirm::ApproveSignatureFailed {
            item: Box::new(item),
            arming: Arming::default(),
        }
    }

    pub fn reject(item: crate::packages::inbox::InboxItem) -> Self {
        Confirm::Reject {
            item: Box::new(item),
            arming: Arming::default(),
        }
    }

    pub fn arming(&self) -> &Arming {
        match self {
            Confirm::Uninstall { arming, .. }
            | Confirm::Restore { arming, .. }
            | Confirm::Rollback { arming, .. }
            | Confirm::RestoreBackup { arming, .. }
            | Confirm::RemoveFile { arming, .. }
            | Confirm::InstallWithoutOsv { arming, .. }
            | Confirm::ApproveWithoutOsv { arming, .. }
            | Confirm::RemoveMachine { arming, .. }
            | Confirm::ApproveAll { arming, .. }
            | Confirm::ApproveSignatureFailed { arming, .. }
            | Confirm::Reject { arming, .. }
            | Confirm::Trust { arming, .. }
            | Confirm::Untrust { arming, .. }
            | Confirm::StopDaemon { arming }
            | Confirm::RemoveListItem { arming, .. } => arming,
        }
    }
}

/// The overlay was popped off the stack; push it back to keep it open.
pub fn handle_key(app: &mut App, confirm: Confirm, key: KeyEvent) -> Option<Cmd> {
    let scroll = matches!(
        key.code,
        KeyCode::Char('j')
            | KeyCode::Char('k')
            | KeyCode::Down
            | KeyCode::Up
            | KeyCode::PageDown
            | KeyCode::PageUp
    );
    // Scrolling is harmless, so it works before the confirm is armed
    if !scroll && !confirm.arming().armed(Instant::now()) {
        app.overlays.push(Overlay::Confirm(confirm));
        return None;
    }
    match key.code {
        KeyCode::Char('y') => accept(app, confirm),
        KeyCode::Char('n') | KeyCode::Esc | KeyCode::Enter => cancel(app, confirm),
        KeyCode::Char('j')
        | KeyCode::Char('k')
        | KeyCode::Down
        | KeyCode::Up
        | KeyCode::PageDown
        | KeyCode::PageUp => {
            let mut confirm = confirm;
            if let Confirm::ApproveAll {
                items,
                scroll,
                rows,
                ..
            } = &mut confirm
            {
                let page = rows.get().max(1);
                let last = items.len().saturating_sub(page);
                *scroll = match key.code {
                    KeyCode::Char('j') | KeyCode::Down => *scroll + 1,
                    KeyCode::PageDown => *scroll + page,
                    KeyCode::PageUp => scroll.saturating_sub(page),
                    _ => scroll.saturating_sub(1),
                }
                .min(last);
            }
            app.overlays.push(Overlay::Confirm(confirm));
            None
        }
        _ => {
            app.overlays.push(Overlay::Confirm(confirm));
            None
        }
    }
}

/// The packages approved before OSV became unreachable sync once the user declines the rest.
fn cancel(app: &mut App, confirm: Confirm) -> Option<Cmd> {
    match confirm {
        Confirm::ApproveWithoutOsv { .. } => app.follow_up_sync(),
        _ => None,
    }
}

fn accept(app: &mut App, confirm: Confirm) -> Option<Cmd> {
    match confirm {
        Confirm::Uninstall {
            manager_key, name, ..
        } => {
            // Other member profiles keep the package; this profile leaves it
            let leave = app.state.membership.as_ref().and_then(|m| {
                let members = m.members(&manager_key, &name);
                (members.contains(&m.profile) && members.len() > 1).then(|| Edit {
                    remove: BTreeSet::from([m.profile.clone()]),
                    ..Edit::default()
                })
            });
            app.uninstalling = Some((manager_key.clone(), name.clone()));
            Some(Cmd::Uninstall {
                manager_key,
                name,
                leave,
            })
        }
        Confirm::Restore {
            repo_path,
            dotfile,
            commit,
            short_hash,
            ..
        } => Some(Cmd::Restore {
            repo_path,
            dotfile,
            commit,
            short_hash,
        }),
        Confirm::RestoreBackup {
            path, timestamp, ..
        } => Some(Cmd::RestoreBackup { path, timestamp }),
        Confirm::Rollback { plan, .. } => {
            if app.running.is_some() {
                app.flash_error("Another tether command is still running");
                return None;
            }
            Some(Cmd::Run(Job::Rollback {
                manager: plan.manager,
                commit: plan.commit,
                short_hash: plan.short_hash,
            }))
        }
        Confirm::RemoveFile { path, .. } => {
            let (Some(config), Some(ss)) = (&mut app.state.config, &app.state.sync_state) else {
                return None;
            };
            if let Err(e) = config_edit::remove_profile_dotfile(config, &ss.machine_id, &path) {
                app.flash_error(e);
                app.reload_state();
                return None;
            }
            app.flash_success(format!("removed {}", path));
            app.reload_state();
            let len = files::build_rows(&app.state, &app.files).len();
            clamp_cursor(&mut app.files.cursor, len);
            app.follow_up_sync()
        }
        Confirm::RemoveMachine {
            machine_id, digest, ..
        } => {
            if machine_id == app.machine_id() {
                app.flash_error("Cannot remove this machine's current record");
                return None;
            }
            Some(Cmd::RemoveMachine { machine_id, digest })
        }
        Confirm::ApproveAll { items, .. } => security::approve_all(app, items, true),
        Confirm::ApproveSignatureFailed { item, .. } => security::approve(app, *item),
        Confirm::Reject { item, .. } => Some(Cmd::Reject(item)),
        Confirm::RemoveListItem { index, item, .. } => {
            super::config::remove_list_item(app, index, &item);
            None
        }
        Confirm::StopDaemon { .. } => {
            (app.daemon_op == DaemonOp::None).then_some(Cmd::Daemon(DaemonOp::Stopping))
        }
        Confirm::Trust {
            machine_id,
            label,
            fingerprint,
            item,
            ..
        } => Some(match item {
            Some(item) => Cmd::TrustKey { item, label },
            None => Cmd::TrustMachine {
                machine_id,
                fingerprint,
                label,
            },
        }),
        Confirm::Untrust {
            machine_id, label, ..
        } => Some(Cmd::Untrust { machine_id, label }),
        Confirm::ApproveWithoutOsv { items, .. } => {
            security::approve_all(app, items.into_iter().map(|(i, _)| i).collect(), false)
        }
        Confirm::InstallWithoutOsv {
            manager_key, name, ..
        } => app.start_install(manager_key, name, false),
    }
}

pub fn render(f: &mut Frame, app: &App, confirm: &Confirm) {
    let t = &app.theme;
    let now = Instant::now();
    confirm.arming().drawn(now);
    let wait = confirm.arming().remaining(now);
    match confirm {
        Confirm::Uninstall {
            manager_key, name, ..
        } => {
            let leaving = app.state.membership.as_ref().and_then(|m| {
                let mut members = m.members(manager_key, name);
                (members.remove(&m.profile) && !members.is_empty()).then(|| {
                    (
                        m.profile.clone(),
                        members.into_iter().collect::<Vec<_>>().join(", "),
                    )
                })
            });
            let question = match leaving {
                None => format!("Uninstall {} ({})?", name, manager_label(manager_key)),
                // Machines that have the package keep it; only new installs stop
                Some((profile, keep)) => format!(
                    "Uninstall {} ({}) here? Other machines in profile {} stop installing it \
                     but keep any copy they have. Profiles {} keep it",
                    name,
                    manager_label(manager_key),
                    profile,
                    keep
                ),
            };
            render_popup(f, app, wait, "Uninstall", &question, t.error)
        }
        Confirm::Restore {
            dotfile,
            short_hash,
            ..
        } => render_popup(
            f,
            app,
            wait,
            "Restore",
            &format!("Restore {} to {}?", dotfile, short_hash),
            t.warn,
        ),
        Confirm::RestoreBackup {
            path, timestamp, ..
        } => render_popup(
            f,
            app,
            wait,
            "Restore from backup",
            &format!(
                "Overwrite ~/{} with its backup from {}? The next sync pushes it to your \
                 other machines.",
                path, timestamp
            ),
            t.warn,
        ),
        Confirm::Rollback { plan, .. } => render_popup(
            f,
            app,
            wait,
            "Roll back packages",
            &format!(
                "Roll back {} to {} (+{} install, -{} uninstall)?",
                manager_label(&plan.manager),
                plan.short_hash,
                plan.to_install.len(),
                plan.uninstall
            ),
            t.warn,
        ),
        Confirm::RemoveFile { path, .. } => render_popup(
            f,
            app,
            wait,
            "Remove",
            &format!("Remove {} from profile?", path),
            t.error,
        ),
        Confirm::InstallWithoutOsv {
            manager_key,
            name,
            error,
            ..
        } => render_popup(
            f,
            app,
            wait,
            "OSV unreachable",
            &format!(
                "OSV could not check {} ({}): {}. Install it without the malicious-package check?",
                name,
                manager_label(manager_key),
                error
            ),
            t.error,
        ),
        Confirm::ApproveWithoutOsv { items, .. } => render_popup(
            f,
            app,
            wait,
            "OSV unreachable",
            &format!(
                "OSV could not check {}. Approve and install without the malicious-package check?",
                items
                    .iter()
                    .map(|(i, error)| format!(
                        "{} ({}): {}",
                        i.name,
                        manager_label(&i.manager),
                        error
                    ))
                    .collect::<Vec<_>>()
                    .join("; "),
            ),
            t.error,
        ),
        Confirm::RemoveMachine {
            machine_id,
            hostname,
            last_sync,
            packages,
            ..
        } => {
            let lines = [
                format!("id         {}", machine_id),
                format!("hostname   {}", hostname),
                format!(
                    "last sync  {} ({} days ago)",
                    last_sync
                        .with_timezone(&chrono::Local)
                        .format("%Y-%m-%d %H:%M"),
                    chrono::Utc::now()
                        .signed_duration_since(last_sync)
                        .num_days()
                ),
                format!("packages   {}", packages),
            ];
            render_list_popup(
                f,
                app,
                wait,
                "Remove old record",
                "This record may be an old id of this machine. Tether guesses from its hostname \
                 and age, so check that no other machine uses this hostname. An old id no \
                 longer syncs, but its packages still count for every machine. Remove it and \
                 commit the removal?",
                &lines,
                0,
                &std::cell::Cell::new(lines.len()),
                t.error,
            )
        }
        Confirm::ApproveSignatureFailed { item, .. } => render_popup(
            f,
            app,
            wait,
            "Signature failed",
            &format!(
                "{} comes from {}, whose record fails its signature. Someone may have edited \
                 it in the repo. Approve and install {} anyway?",
                item.name,
                item.source_machine.as_deref().unwrap_or("another machine"),
                approve_all_line(item)
            ),
            t.error,
        ),
        Confirm::RemoveListItem { list, name, .. } => render_popup(
            f,
            app,
            wait,
            "Remove",
            &format!("Remove {} from {}?", name, list),
            t.error,
        ),
        Confirm::StopDaemon { .. } => render_popup(
            f,
            app,
            wait,
            "Stop daemon",
            "Stop the daemon? This machine then syncs only when you sync it.",
            t.warn,
        ),
        Confirm::Reject { item, .. } => {
            let what = match &item.kind {
                crate::packages::inbox::Kind::TrustMachine { fingerprint, .. } => {
                    format!("the key {} of machine {}", fingerprint, item.name)
                }
                crate::packages::inbox::Kind::Package => approve_all_line(item),
            };
            render_popup(
                f,
                app,
                wait,
                "Reject",
                &format!(
                    "Reject {}? It leaves the Inbox and comes back only if it changes.",
                    what
                ),
                t.error,
            )
        }
        Confirm::Trust {
            machine_id,
            label,
            fingerprint,
            changed,
            ..
        } => {
            let mut msg = String::new();
            if *changed {
                msg.push_str(
                    "THE KEY CHANGED. This machine was trusted with another key. If you did not \
                     set it up again, someone may be signing as it. ",
                );
            }
            msg.push_str(
                "Compare this fingerprint with the one 'tether machines show' shows on that \
                 machine. Trust the key? Package changes it signs then install without approval.",
            );
            let lines = [
                format!("machine  {}", machine_label(label, machine_id)),
                format!("key      {}", fingerprint),
            ];
            render_list_popup(
                f,
                app,
                wait,
                "Trust machine key",
                &msg,
                &lines,
                0,
                &std::cell::Cell::new(lines.len()),
                if *changed { t.error } else { t.warn },
            )
        }
        Confirm::Untrust {
            machine_id,
            label,
            fingerprint,
            ..
        } => {
            let lines = [
                format!("machine  {}", machine_label(label, machine_id)),
                format!("key      {}", fingerprint),
            ];
            render_list_popup(
                f,
                app,
                wait,
                "Untrust machine key",
                "Stop trusting this key? Package changes from this machine then wait in the \
                 Inbox, and its key waits there after the next sync.",
                &lines,
                0,
                &std::cell::Cell::new(lines.len()),
                t.error,
            )
        }
        Confirm::ApproveAll {
            items,
            from,
            held,
            scroll,
            rows,
            ..
        } => {
            let mut msg = format!(
                "Approve and install these {} package{}{}?",
                items.len(),
                if items.len() == 1 { "" } else { "s" },
                from.as_deref()
                    .map(|m| format!(" from {}", m))
                    .unwrap_or_default()
            );
            let advised = items.iter().filter(|i| !i.advisories.is_empty()).count();
            if advised > 0 {
                msg.push_str(&format!(
                    " {} {} OSV advisories (marked ▲).",
                    advised,
                    if advised == 1 { "has" } else { "have" }
                ));
            }
            if *held > 0 {
                msg.push_str(&format!(
                    " {} malicious or with a failed signature stay held.",
                    held
                ));
            }
            let lines: Vec<String> = items
                .iter()
                .map(|i| match i.advisories.len() {
                    0 => approve_all_line(i),
                    1 => format!("{}  ▲ 1 advisory", approve_all_line(i)),
                    n => format!("{}  ▲ {} advisories", approve_all_line(i), n),
                })
                .collect();
            render_list_popup(
                f,
                app,
                wait,
                "Approve all",
                &msg,
                &lines,
                *scroll,
                rows,
                t.ok,
            )
        }
    }
}

/// A machine's hostname with its id, or the id alone when it has no other name.
fn machine_label(label: &str, machine_id: &str) -> String {
    if label == machine_id {
        machine_id.to_string()
    } else {
        format!("{} ({})", label, machine_id)
    }
}

/// One package as the approve-all question lists it: name, version, manager and tap.
pub fn approve_all_line(item: &crate::packages::inbox::InboxItem) -> String {
    let mut line = format!(
        "{} {} ({})",
        item.name,
        item.version.as_deref().unwrap_or("unpinned"),
        manager_label(&item.manager)
    );
    if let Some(tap) = &item.tap {
        line.push_str(&format!(" from {}", tap));
    }
    line
}

/// A question over a scrollable list, with the buttons of [`render_popup`]. `rows` gets the
/// number of list lines that fit, so scrolling stops at the last page.
#[allow(clippy::too_many_arguments)]
fn render_list_popup(
    f: &mut Frame,
    app: &App,
    wait: Option<Duration>,
    title: &str,
    msg: &str,
    lines: &[String],
    scroll: usize,
    rows: &std::cell::Cell<usize>,
    color: Color,
) {
    let t = &app.theme;
    let area = f.area();
    let widest = lines
        .iter()
        .map(|l| l.chars().count() + 2)
        // A long question wraps rather than stretching the popup across the screen
        .chain([msg.chars().count().min(64)])
        .max()
        .unwrap_or(0);
    let width = (widest as u16 + 8)
        .max(36)
        .min(area.width.saturating_sub(4));
    let inner_w = width.saturating_sub(4).max(1) as usize;
    let msg_lines = msg.chars().count().div_ceil(inner_w).max(1) as u16;
    // Border, gap, question, gap, list, gap, buttons, border
    let chrome = msg_lines + 6;
    let height = (chrome + lines.len() as u16).min(area.height.saturating_sub(2));
    let rect = centered(area, width, height);
    let block = popup(f, rect, title, color, t);
    let inner = block.inner(rect);
    f.render_widget(block, rect);
    f.render_widget(
        Paragraph::new(msg)
            .style(Style::default().fg(t.text))
            .wrap(Wrap { trim: true }),
        Rect {
            y: inner.y + 1,
            height: msg_lines,
            ..inner
        },
    );
    let visible = height.saturating_sub(chrome) as usize;
    rows.set(visible.max(1));
    let start = scroll.min(lines.len().saturating_sub(visible));
    let list = Rect {
        y: inner.y + msg_lines + 2,
        height: visible as u16,
        ..inner
    };
    f.render_widget(
        Paragraph::new(
            lines[start..]
                .iter()
                .take(visible)
                .map(|l| {
                    Line::from(vec![
                        Span::styled("• ", Style::default().fg(t.dim)),
                        Span::raw(l.as_str()),
                    ])
                })
                .collect::<Vec<_>>(),
        )
        .style(Style::default().fg(t.text)),
        list,
    );
    super::scrollbar(
        f,
        Rect {
            y: list.y.saturating_sub(1),
            height: list.height + 2,
            ..rect
        },
        lines.len(),
        start,
        visible,
        t,
    );
    buttons(f, app, wait, inner, color);
}

/// A question with clickable confirm and cancel buttons.
pub fn render_popup(
    f: &mut Frame,
    app: &App,
    wait: Option<Duration>,
    title: &str,
    msg: &str,
    color: Color,
) {
    let t = &app.theme;
    let area = f.area();
    let width = (msg.chars().count() as u16 + 8)
        .max(36)
        .min(area.width.saturating_sub(4));
    let inner_w = width.saturating_sub(4).max(1) as usize;
    let msg_lines = msg.chars().count().div_ceil(inner_w).max(1) as u16;
    let height = (msg_lines + 5).min(area.height.saturating_sub(2));
    let rect = centered(area, width, height);
    let block = popup(f, rect, title, color, t);
    let inner = block.inner(rect);
    f.render_widget(block, rect);
    f.render_widget(
        Paragraph::new(msg)
            .style(Style::default().fg(t.text))
            .wrap(Wrap { trim: true }),
        Rect {
            y: inner.y + 1,
            height: msg_lines,
            ..inner
        },
    );

    buttons(f, app, wait, inner, color);
}

/// Clickable confirm and cancel buttons on the last line of `inner`. A confirm that is not
/// armed yet shows dim buttons and a countdown.
fn buttons(f: &mut Frame, app: &App, wait: Option<Duration>, inner: Rect, color: Color) {
    let t = &app.theme;
    let yes = match wait {
        // Rounded up, so the last tenth of a second does not read 0.0s
        Some(left) => format!(
            " y  wait {:.1}s ",
            left.as_millis().div_ceil(100) as f32 / 10.0
        ),
        None => " y  confirm ".to_string(),
    };
    let yes = yes.as_str();
    let no = " n  cancel ";
    let by = inner.bottom().saturating_sub(1);
    let yes_w = yes.chars().count() as u16;
    let no_w = no.chars().count() as u16;
    let x = inner.right().saturating_sub(yes_w + no_w + 2);
    let yes_rect = Rect::new(x, by, yes_w, 1);
    let no_rect = Rect::new(x + yes_w + 2, by, no_w, 1);
    if no_rect.right() <= inner.right() {
        let (yes_style, no_style) = match wait {
            Some(_) => {
                let dim = Style::default().fg(t.dim).bg(t.selection);
                (dim, dim)
            }
            None => (
                Style::default().fg(t.brand_fg).bg(color).bold(),
                Style::default().fg(t.text).bg(t.selection),
            ),
        };
        f.render_widget(Paragraph::new(yes).style(yes_style), yes_rect);
        f.render_widget(Paragraph::new(no).style(no_style), no_rect);
        app.add_hit(yes_rect, Hit::Key(KeyEvent::from(KeyCode::Char('y'))));
        app.add_hit(no_rect, Hit::Key(KeyEvent::from(KeyCode::Char('n'))));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn arming_waits_for_the_first_draw_and_the_delay() {
        let arming = Arming::default();
        let t0 = Instant::now();
        assert!(!arming.armed(t0 + Duration::from_secs(60)));
        arming.drawn(t0);
        assert_eq!(arming.remaining(t0), Some(ARM_DELAY));
        assert!(!arming.armed(t0 + ARM_DELAY - Duration::from_millis(1)));
        assert!(!arming.shown_armed());
        // Later draws keep the first draw's time
        arming.drawn(t0 + Duration::from_secs(1));
        assert!(arming.armed(t0 + ARM_DELAY));
        // The loop draws until a draw shows the armed buttons
        assert!(arming.shown_armed());
    }
}

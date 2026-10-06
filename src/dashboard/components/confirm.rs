use super::{centered, clamp_cursor, files, manager_label, popup, security};
use crate::dashboard::app::{App, Hit, Job, Overlay};
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
}

impl Arming {
    pub fn drawn(&self, now: Instant) {
        if self.drawn.get().is_none() {
            self.drawn.set(Some(now));
        }
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

/// A yes/no question about one action.
pub enum Confirm {
    Uninstall {
        manager_key: String,
        name: String,
    },
    Restore {
        repo_path: String,
        dotfile: String,
        commit: String,
        short_hash: String,
    },
    Rollback(RollbackPlan),
    RemoveFile {
        path: String,
    },
    /// Install a package that OSV could not check. Only `y` accepts.
    InstallWithoutOsv {
        manager_key: String,
        name: String,
        error: String,
        arming: Arming,
    },
    /// Approve and install inbox items that OSV could not check. Only `y` accepts.
    /// Each item comes with its own OSV error, because one may warn of malicious releases
    /// while another only timed out.
    ApproveWithoutOsv {
        items: Vec<(crate::packages::inbox::InboxItem, String)>,
        arming: Arming,
    },
    /// Remove another machine's record that looks like an old id of this machine. Only `y`
    /// accepts.
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
    /// Only `y` accepts.
    ApproveSignatureFailed {
        item: Box<crate::packages::inbox::InboxItem>,
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

    /// Confirms that install without the malicious-package check, approve, or delete a
    /// record wait to be armed. Only `y` accepts them.
    pub fn arming(&self) -> Option<&Arming> {
        match self {
            Confirm::InstallWithoutOsv { arming, .. }
            | Confirm::ApproveWithoutOsv { arming, .. }
            | Confirm::RemoveMachine { arming, .. }
            | Confirm::ApproveAll { arming, .. }
            | Confirm::ApproveSignatureFailed { arming, .. } => Some(arming),
            _ => None,
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
    if !scroll && confirm.arming().is_some_and(|a| !a.armed(Instant::now())) {
        app.overlays.push(Overlay::Confirm(confirm));
        return None;
    }
    match key.code {
        // Installing without the malicious-package check, approving, or deleting a record is
        // never the default answer
        KeyCode::Enter if confirm.arming().is_some() => cancel(app, confirm),
        KeyCode::Char('y') | KeyCode::Enter => accept(app, confirm),
        KeyCode::Char('n') | KeyCode::Esc => cancel(app, confirm),
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
        Confirm::Uninstall { manager_key, name } => {
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
        } => Some(Cmd::Restore {
            repo_path,
            dotfile,
            commit,
            short_hash,
        }),
        Confirm::Rollback(plan) => {
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
        Confirm::RemoveFile { path } => {
            let (Some(config), Some(ss)) = (&mut app.state.config, &app.state.sync_state) else {
                return None;
            };
            if !config_edit::remove_profile_dotfile(config, &ss.machine_id, &path) {
                app.flash_error("remove failed");
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
    if let Some(arming) = confirm.arming() {
        arming.drawn(now);
    }
    let wait = confirm.arming().and_then(|a| a.remaining(now));
    match confirm {
        Confirm::Uninstall { manager_key, name } => {
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
        Confirm::Rollback(plan) => render_popup(
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
        Confirm::RemoveFile { path } => render_popup(
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
            if *held > 0 {
                msg.push_str(&format!(
                    " {} malicious or with a failed signature stay held.",
                    held
                ));
            }
            let lines: Vec<String> = items.iter().map(approve_all_line).collect();
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
        Some(left) => format!(" y  wait {:.1}s ", left.as_secs_f32()),
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
        // Later draws keep the first draw's time
        arming.drawn(t0 + Duration::from_secs(1));
        assert!(arming.armed(t0 + ARM_DELAY));
    }
}

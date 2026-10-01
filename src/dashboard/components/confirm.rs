use super::{centered, clamp_cursor, files, manager_label, popup, security};
use crate::dashboard::app::{App, Hit, Overlay};
use crate::dashboard::config_edit;
use crate::dashboard::msg::Cmd;
use crate::dashboard::repo::RollbackPlan;
use crossterm::event::{KeyCode, KeyEvent};
use ratatui::{prelude::*, widgets::*};

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
    /// Approve and install these inbox items, as displayed when the confirm opened.
    ApproveAll {
        items: Vec<crate::packages::inbox::InboxItem>,
        malicious: usize,
    },
}

/// The overlay was popped off the stack; push it back to keep it open.
pub fn handle_key(app: &mut App, confirm: Confirm, key: KeyEvent) -> Option<Cmd> {
    match key.code {
        KeyCode::Char('y') | KeyCode::Enter => accept(app, confirm),
        KeyCode::Char('n') | KeyCode::Esc => None,
        _ => {
            app.overlays.push(Overlay::Confirm(confirm));
            None
        }
    }
}

fn accept(app: &mut App, confirm: Confirm) -> Option<Cmd> {
    match confirm {
        Confirm::Uninstall { manager_key, name } => {
            app.uninstalling = Some((manager_key.clone(), name.clone()));
            Some(Cmd::Uninstall { manager_key, name })
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
            Some(Cmd::CheckRollback(plan))
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
        Confirm::ApproveAll { items, .. } => security::approve_all(app, items),
    }
}

pub fn render(f: &mut Frame, app: &App, confirm: &Confirm) {
    let t = &app.theme;
    match confirm {
        Confirm::Uninstall { manager_key, name } => render_popup(
            f,
            app,
            "Uninstall",
            &format!("Uninstall {} ({})?", name, manager_label(manager_key)),
            t.error,
        ),
        Confirm::Restore {
            dotfile,
            short_hash,
            ..
        } => render_popup(
            f,
            app,
            "Restore",
            &format!("Restore {} to {}?", dotfile, short_hash),
            t.warn,
        ),
        Confirm::Rollback(plan) => render_popup(
            f,
            app,
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
            "Remove",
            &format!("Remove {} from profile?", path),
            t.error,
        ),
        Confirm::ApproveAll { items, malicious } => {
            let mut msg = format!(
                "Approve and install {} package{}?",
                items.len(),
                if items.len() == 1 { "" } else { "s" }
            );
            if *malicious > 0 {
                msg.push_str(&format!(" {} malicious stay held.", malicious));
            }
            render_popup(f, app, "Approve all", &msg, t.ok)
        }
    }
}

/// A question with clickable confirm and cancel buttons.
pub fn render_popup(f: &mut Frame, app: &App, title: &str, msg: &str, color: Color) {
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

    let yes = " y  confirm ";
    let no = " n  cancel ";
    let by = inner.bottom().saturating_sub(1);
    let yes_w = yes.chars().count() as u16;
    let no_w = no.chars().count() as u16;
    let x = inner.right().saturating_sub(yes_w + no_w + 2);
    let yes_rect = Rect::new(x, by, yes_w, 1);
    let no_rect = Rect::new(x + yes_w + 2, by, no_w, 1);
    if no_rect.right() <= inner.right() {
        f.render_widget(
            Paragraph::new(yes).style(Style::default().fg(t.brand_fg).bg(color).bold()),
            yes_rect,
        );
        f.render_widget(
            Paragraph::new(no).style(Style::default().fg(t.text).bg(t.selection)),
            no_rect,
        );
        app.add_hit(yes_rect, Hit::Key(KeyEvent::from(KeyCode::Char('y'))));
        app.add_hit(no_rect, Hit::Key(KeyEvent::from(KeyCode::Char('n'))));
    }
}

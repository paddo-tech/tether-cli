use super::{centered, clamp_cursor, files, manager_label};
use crate::dashboard::app::{App, Job, Overlay};
use crate::dashboard::config_edit;
use crate::dashboard::msg::Cmd;
use crate::dashboard::repo::RollbackPlan;
use crate::dashboard::theme::Theme;
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
            app.sync_cmd()
        }
    }
}

pub fn render(f: &mut Frame, confirm: &Confirm, t: &Theme) {
    match confirm {
        Confirm::Uninstall { manager_key, name } => render_popup(
            f,
            "Uninstall",
            &format!("Uninstall {} ({})?", name, manager_label(manager_key)),
            t.error,
            t,
        ),
        Confirm::Restore {
            dotfile,
            short_hash,
            ..
        } => render_popup(
            f,
            "Restore",
            &format!("Restore {} to {}?", dotfile, short_hash),
            t.warn,
            t,
        ),
        Confirm::Rollback(plan) => render_popup(
            f,
            "Roll back packages",
            &format!(
                "Roll back {} to {} (+{} install, -{} uninstall)?",
                manager_label(&plan.manager),
                plan.short_hash,
                plan.install,
                plan.uninstall
            ),
            t.warn,
            t,
        ),
        Confirm::RemoveFile { path } => render_popup(
            f,
            "Remove",
            &format!("Remove {} from profile?", path),
            t.error,
            t,
        ),
    }
}

pub fn render_popup(f: &mut Frame, title: &str, msg: &str, border: Color, t: &Theme) {
    let area = f.area();
    let width = (msg.len() as u16 + 8).min(area.width.saturating_sub(4));
    let height = 5u16.min(area.height.saturating_sub(2));
    let popup_area = centered(area, width, height);

    f.render_widget(Clear, popup_area);

    let text = vec![
        Line::from(""),
        Line::from(Span::styled(
            format!("  {}", msg),
            Style::default().fg(t.text),
        )),
        Line::from(""),
        Line::from(vec![
            Span::styled("  y", t.key_hint()),
            Span::styled(" confirm    ", Style::default().fg(t.muted)),
            Span::styled("n/Esc", t.key_hint()),
            Span::styled(" cancel", Style::default().fg(t.muted)),
        ]),
    ];

    let paragraph = Paragraph::new(text).block(
        Block::default()
            .title(format!(" {} ", title))
            .borders(Borders::ALL)
            .border_style(Style::default().fg(border)),
    );
    f.render_widget(paragraph, popup_area);
}

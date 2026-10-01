use crate::cli::output::relative_time;
use crate::dashboard::app::{App, DaemonOp, Job};
use ratatui::{prelude::*, widgets::*};

pub fn render(f: &mut Frame, area: Rect, app: &App) {
    let t = &app.theme;
    let state = &app.state;
    let mut spans = vec![Span::styled(
        " Tether ",
        Style::default().fg(t.brand_fg).bg(t.brand_bg).bold(),
    )];

    // Machine name
    if let Some(ref sync_state) = state.sync_state {
        spans.push(Span::raw("  "));
        spans.push(Span::styled(
            &sync_state.machine_id,
            Style::default().fg(t.text).bold(),
        ));
    }

    spans.push(Span::raw("  "));

    // Daemon status
    match app.daemon_op {
        DaemonOp::Starting => {
            spans.push(Span::styled(
                "daemon: starting...",
                Style::default().fg(t.warn),
            ));
        }
        DaemonOp::Stopping => {
            spans.push(Span::styled(
                "daemon: stopping...",
                Style::default().fg(t.warn),
            ));
        }
        DaemonOp::None => {
            if state.daemon_running {
                let pid_info = state
                    .daemon_pid
                    .map(|p| format!("daemon: running ({})", p))
                    .unwrap_or_else(|| "daemon: running".to_string());
                spans.push(Span::styled(pid_info, Style::default().fg(t.ok)));
            } else {
                spans.push(Span::styled(
                    "daemon: stopped",
                    Style::default().fg(t.error),
                ));
            }
        }
    }

    spans.push(Span::raw("  "));

    // Sync status
    if let Some(job) = &app.running {
        let label = match job {
            Job::Sync => "syncing",
            Job::Rollback { .. } => "rolling back",
        };
        spans.push(Span::styled(
            format!("{}...", label),
            Style::default().fg(t.warn),
        ));
    } else if let Some(ref sync_state) = state.sync_state {
        spans.push(Span::styled(
            format!("last sync: {}", relative_time(sync_state.last_sync)),
            Style::default().fg(t.muted),
        ));
    }

    // Conflicts
    if state.conflicts.has_conflicts() {
        spans.push(Span::raw("  "));
        spans.push(Span::styled(
            format!("{} conflict(s)", state.conflicts.conflicts.len()),
            Style::default().fg(t.error).bold(),
        ));
    }

    if let Some((_, pkg_name)) = &app.uninstalling {
        spans.push(Span::raw("  "));
        spans.push(Span::styled(
            format!("uninstalling {}...", pkg_name),
            Style::default().fg(t.warn),
        ));
    }

    if let Some((_, pkg_name)) = &app.installing {
        spans.push(Span::raw("  "));
        spans.push(Span::styled(
            format!("installing {}...", pkg_name),
            Style::default().fg(t.warn),
        ));
    }

    // Flash message: an error hides a concurrent success message
    let flash = app
        .flash_error
        .as_ref()
        .map(|(_, m)| (m, t.error))
        .or_else(|| app.flash_message.as_ref().map(|(_, m)| (m, t.ok)));
    if let Some((msg, color)) = flash {
        spans.push(Span::raw("  "));
        spans.push(Span::styled(msg, Style::default().fg(color).bold()));
    }

    // Features from config
    if let Some(ref config) = state.config {
        if config.features.team_dotfiles {
            spans.push(Span::raw("  "));
            spans.push(Span::styled("team", Style::default().fg(t.team)));
        }
    }

    let paragraph = Paragraph::new(Line::from(spans)).block(
        Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(t.border)),
    );
    f.render_widget(paragraph, area);
}

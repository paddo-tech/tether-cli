use crate::cli::output::relative_time;
use crate::dashboard::state::DashboardState;
use crate::dashboard::DaemonOp;
use ratatui::{prelude::*, widgets::*};

pub enum FlashMessage<'a> {
    Error(&'a str),
    Success(&'a str),
}

pub fn render(
    f: &mut Frame,
    area: Rect,
    state: &DashboardState,
    syncing: bool,
    daemon_op: DaemonOp,
    flash: Option<FlashMessage>,
    uninstalling: Option<&(String, String)>,
) {
    let mut spans = vec![Span::styled(
        " Tether ",
        Style::default().fg(Color::Black).bg(Color::Cyan).bold(),
    )];

    // Machine name
    if let Some(ref sync_state) = state.sync_state {
        spans.push(Span::raw("  "));
        spans.push(Span::styled(
            &sync_state.machine_id,
            Style::default().fg(Color::White).bold(),
        ));
    }

    spans.push(Span::raw("  "));

    // Daemon status
    match daemon_op {
        DaemonOp::Starting => {
            spans.push(Span::styled(
                "daemon: starting...",
                Style::default().fg(Color::Yellow),
            ));
        }
        DaemonOp::Stopping => {
            spans.push(Span::styled(
                "daemon: stopping...",
                Style::default().fg(Color::Yellow),
            ));
        }
        DaemonOp::None => {
            if state.daemon_running {
                let pid_info = state
                    .daemon_pid
                    .map(|p| format!("daemon: running ({})", p))
                    .unwrap_or_else(|| "daemon: running".to_string());
                spans.push(Span::styled(pid_info, Style::default().fg(Color::Green)));
            } else {
                spans.push(Span::styled(
                    "daemon: stopped",
                    Style::default().fg(Color::Red),
                ));
            }
        }
    }

    spans.push(Span::raw("  "));

    // Sync status
    if syncing {
        spans.push(Span::styled(
            "syncing...",
            Style::default().fg(Color::Yellow),
        ));
    } else if let Some(ref sync_state) = state.sync_state {
        spans.push(Span::styled(
            format!("last sync: {}", relative_time(sync_state.last_sync)),
            Style::default().fg(Color::DarkGray),
        ));
    }

    // Conflicts
    if state.conflicts.has_conflicts() {
        spans.push(Span::raw("  "));
        spans.push(Span::styled(
            format!("{} conflict(s)", state.conflicts.conflicts.len()),
            Style::default().fg(Color::Red).bold(),
        ));
    }

    // Uninstalling
    if let Some((_, pkg_name)) = uninstalling {
        spans.push(Span::raw("  "));
        spans.push(Span::styled(
            format!("uninstalling {}...", pkg_name),
            Style::default().fg(Color::Yellow),
        ));
    }

    // Flash message
    if let Some(flash_msg) = flash {
        let (msg, color) = match flash_msg {
            FlashMessage::Error(m) => (m, Color::Red),
            FlashMessage::Success(m) => (m, Color::Green),
        };
        spans.push(Span::raw("  "));
        spans.push(Span::styled(msg, Style::default().fg(color).bold()));
    }

    // Features from config
    if let Some(ref config) = state.config {
        if config.features.team_dotfiles {
            spans.push(Span::raw("  "));
            spans.push(Span::styled("team", Style::default().fg(Color::Magenta)));
        }
    }

    let paragraph = Paragraph::new(Line::from(spans)).block(
        Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::DarkGray)),
    );
    f.render_widget(paragraph, area);
}

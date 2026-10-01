use super::{pulse, spinner};
use crate::cli::output::relative_time;
use crate::dashboard::app::{App, DaemonOp, Hit, Job, Tab};
use crate::dashboard::theme::mix;
use ratatui::prelude::*;

/// One-line header: logo, machine, then daemon and sync status on the right.
pub fn render(f: &mut Frame, area: Rect, app: &App) {
    let t = &app.theme;
    let state = &app.state;
    let ms = app.clock_ms();
    let busy = app.running.is_some();

    // The logo pulses while a job runs.
    let brand_bg = if busy && t.rgb {
        mix(t.brand_bg, t.info, pulse(ms, 1400))
    } else {
        t.brand_bg
    };
    let mut left = vec![
        Span::styled(
            " ◆ tether ",
            Style::default().fg(t.brand_fg).bg(brand_bg).bold(),
        ),
        Span::styled(
            format!(" v{}", env!("CARGO_PKG_VERSION")),
            Style::default().fg(t.dim),
        ),
    ];
    let machine_id = app.machine_id();
    if !machine_id.is_empty() {
        let host = state
            .machines
            .iter()
            .find(|m| m.machine_id == machine_id)
            .map(|m| m.hostname.trim_end_matches(".local").to_string())
            .filter(|h| !h.is_empty())
            .unwrap_or_else(|| machine_id.to_string());
        left.push(Span::styled("  ", Style::default()));
        left.push(Span::styled(host, Style::default().fg(t.text).bold()));
    }
    if let Some(profile) = state
        .config
        .as_ref()
        .map(|c| c.profile_name(machine_id).to_string())
    {
        left.push(Span::styled(
            format!("  {}", profile),
            Style::default().fg(t.dim),
        ));
    }

    let sep = || Span::styled("  │  ", Style::default().fg(t.border));
    let mut right: Vec<Span> = Vec::new();

    // Leftmost on the right side, so its click region starts where the line does.
    let (pending, malicious) = super::security::pending(app);
    let badge = format!(" ⚑ {} pending ", pending);
    let badge_w = badge.chars().count() as u16;
    if pending > 0 {
        let bg = if malicious { t.error } else { t.warn };
        right.push(Span::styled(
            badge,
            Style::default().fg(t.brand_fg).bg(bg).bold(),
        ));
        right.push(Span::raw("  "));
    }

    if let Some(op) = &app.installing {
        right.push(Span::styled(
            format!("{} installing {}", spinner(ms), op.name),
            Style::default().fg(t.info),
        ));
        right.push(sep());
    }
    if let Some((_, name)) = &app.uninstalling {
        right.push(Span::styled(
            format!("{} uninstalling {}", spinner(ms), name),
            Style::default().fg(t.warn),
        ));
        right.push(sep());
    }
    if state.conflicts.has_conflicts() {
        right.push(Span::styled(
            format!("▲ {} conflict(s)", state.conflicts.conflicts.len()),
            Style::default().fg(t.error).bold(),
        ));
        right.push(sep());
    }
    if state
        .config
        .as_ref()
        .is_some_and(|c| c.features.team_dotfiles)
    {
        right.push(Span::styled("team", Style::default().fg(t.team)));
        right.push(sep());
    }

    match app.daemon_op {
        DaemonOp::Starting | DaemonOp::Stopping => {
            let verb = if app.daemon_op == DaemonOp::Starting {
                "starting"
            } else {
                "stopping"
            };
            right.push(Span::styled(
                format!("{} daemon {}", spinner(ms), verb),
                Style::default().fg(t.warn),
            ));
        }
        DaemonOp::None if state.daemon_running => {
            right.push(Span::styled("● ", Style::default().fg(t.ok)));
            right.push(Span::styled("daemon", Style::default().fg(t.text)));
            if let Some(pid) = state.daemon_pid {
                right.push(Span::styled(
                    format!(" {}", pid),
                    Style::default().fg(t.dim),
                ));
            }
        }
        DaemonOp::None => {
            right.push(Span::styled("○ ", Style::default().fg(t.error)));
            right.push(Span::styled("daemon stopped", Style::default().fg(t.muted)));
        }
    }
    right.push(sep());

    if let Some(job) = &app.running {
        let label = match job {
            Job::Sync => "syncing",
            Job::Rollback { .. } => "rolling back",
        };
        let color = if t.rgb {
            mix(t.accent, t.info, pulse(ms, 1400))
        } else {
            t.accent
        };
        right.push(Span::styled(
            format!("{} {}", spinner(ms), label),
            Style::default().fg(color).bold(),
        ));
    } else if let Some(ss) = &state.sync_state {
        right.push(Span::styled("✓ ", Style::default().fg(t.ok)));
        right.push(Span::styled(
            format!("synced {}", relative_time(ss.last_sync)),
            Style::default().fg(t.muted),
        ));
    } else {
        right.push(Span::styled("not initialized", Style::default().fg(t.warn)));
    }
    right.push(Span::raw(" "));

    let right = Line::from(right);
    let right_w = right.width() as u16;
    if pending > 0 && right_w <= area.width {
        app.add_hit(
            Rect::new(area.right() - right_w, area.y, badge_w, 1),
            Hit::Tab(Tab::Security),
        );
    }
    super::row(f, area, Line::from(left), right);
}

use super::{activity, cursor_down, files, machines, packages, panel, pulse, sparkline};
use crate::dashboard::app::{App, Job, Overlay};
use crate::dashboard::msg::KeyOutcome;
use crate::dashboard::theme::mix;
use crossterm::event::{KeyCode, KeyEvent};
use ratatui::prelude::*;

pub fn handle_key(app: &mut App, key: KeyEvent) -> KeyOutcome {
    match key.code {
        KeyCode::Char('j') | KeyCode::Down => {
            let len = files::build_overview_rows(&app.state).len();
            cursor_down(&mut app.overview_scroll, len);
        }
        KeyCode::Char('k') | KeyCode::Up => {
            app.overview_scroll = app.overview_scroll.saturating_sub(1);
        }
        KeyCode::Enter => {
            app.overlays
                .push(Overlay::Log(super::log_view::LogView::open()));
        }
        _ => return KeyOutcome::Ignored,
    }
    KeyOutcome::Handled(None)
}

pub fn render(f: &mut Frame, area: Rect, app: &App) {
    let spark_h = if area.height >= 30 { 7 } else { 5 };
    // The top row needs only the package bars; the rest goes to machines and the log.
    let managers = app
        .state
        .machines
        .iter()
        .find(|m| m.machine_id == app.machine_id())
        .map_or(0, |m| m.packages.len()) as u16;
    let top_h = (managers + 2).max(area.height.saturating_sub(spark_h) * 2 / 5);
    let [spark, top, bottom] = Layout::vertical([
        Constraint::Length(spark_h),
        Constraint::Length(top_h),
        Constraint::Min(0),
    ])
    .areas(area);
    let [left, right] =
        Layout::horizontal([Constraint::Percentage(55), Constraint::Percentage(45)])
            .spacing(1)
            .areas(top);
    let [machines_area, activity_area] =
        Layout::horizontal([Constraint::Percentage(45), Constraint::Percentage(55)])
            .spacing(1)
            .areas(bottom);

    render_activity_chart(f, spark, app);
    files::render_overview(f, left, app);
    packages::render_overview(f, right, app);
    machines::render_overview(f, machines_area, app);
    activity::render(f, activity_area, &app.state.activity_lines, &app.theme);
}

/// Sync commits per day from the sync repo's history, newest at the right.
fn render_activity_chart(f: &mut Frame, area: Rect, app: &App) {
    let t = &app.theme;
    let values = &app.sync_activity;
    let inner_w = area.width.saturating_sub(4);
    let days = values.len().min(inner_w.div_ceil(2) as usize);
    let shown = &values[values.len() - days..];
    let total: u64 = shown.iter().sum();
    let peak = shown.iter().copied().max().unwrap_or(0);
    let stats = Line::from(vec![
        Span::styled(format!(" {}", total), Style::default().fg(t.text).bold()),
        Span::styled(" commits  ", Style::default().fg(t.dim)),
        Span::styled(format!("{}", peak), Style::default().fg(t.text).bold()),
        Span::styled(" peak/day ", Style::default().fg(t.dim)),
    ]);
    let block = panel(format!(" Sync activity · {} days ", days), false, t)
        .title_top(stats.right_aligned());
    let inner = block.inner(area);
    f.render_widget(block, area);
    if inner.height < 2 {
        return;
    }
    if values.is_empty() {
        f.render_widget(
            Line::from(Span::styled("No sync history", Style::default().fg(t.dim))),
            inner,
        );
        return;
    }

    let bars = Rect {
        height: inner.height - 1,
        ..inner
    };
    // Today's bar pulses while a sync runs.
    let live = matches!(app.running, Some(Job::Sync)).then(|| {
        if t.rgb {
            mix(t.ok, t.accent, pulse(app.clock_ms(), 1000))
        } else {
            t.ok
        }
    });
    sparkline::render(f, bars, shown, live, t);

    let start = chrono::Local::now().date_naive() - chrono::Duration::days(days as i64 - 1);
    let axis = Rect::new(inner.x, inner.bottom() - 1, inner.width, 1);
    super::row(
        f,
        axis,
        Line::from(Span::styled(
            start.format("%b %-d").to_string(),
            Style::default().fg(t.dim),
        )),
        Line::from(Span::styled("today", Style::default().fg(t.dim))),
    );
}

use super::keymap::{self, Binding};
use super::{centered, popup};
use crate::dashboard::app::{App, Hit, Tab};
use ratatui::{prelude::*, widgets::Paragraph};

fn hint_width(b: &Binding) -> u16 {
    (b.short_key().chars().count() + b.hint.chars().count() + 3) as u16
}

/// Key hints from the keymap: the active tab's on the left, global ones on the right, and
/// `? more` last. The most important keys that fit show; help lists the rest. Each hint
/// is clickable.
pub fn render_bar(f: &mut Frame, area: Rect, app: &App) {
    let t = &app.theme;
    let (_, tab) = keymap::active(app);
    let tab: &[Binding] = if app.active_tab == Tab::Security && app.state.inbox.items.is_empty() {
        &[]
    } else {
        tab
    };
    // Each candidate is (global, index in its table, binding)
    let mut candidates: Vec<(bool, usize, &Binding)> = tab
        .iter()
        .enumerate()
        .map(|(i, b)| (false, i, b))
        .chain(keymap::GLOBAL.iter().enumerate().map(|(i, b)| (true, i, b)))
        .filter(|(_, _, b)| b.prio > 0)
        .collect();
    candidates.sort_by_key(|(_, _, b)| b.prio);
    let mut room = area.width.saturating_sub(hint_width(&keymap::HELP) + 1);
    let mut shown: Vec<(bool, usize)> = Vec::new();
    for (global, i, b) in candidates {
        let w = hint_width(b);
        if w <= room {
            room -= w;
            shown.push((global, i));
        }
    }

    let draw = |f: &mut Frame, b: &Binding, x: u16| {
        let w = hint_width(b);
        let rect = Rect::new(x, area.y, w.min(area.right().saturating_sub(x)), 1);
        f.render_widget(
            Line::from(vec![
                Span::styled(format!(" {}", b.short_key()), t.key_hint()),
                Span::styled(format!(" {} ", b.hint), Style::default().fg(t.muted)),
            ]),
            rect,
        );
        if let Some(event) = b.event() {
            app.add_hit(rect, Hit::Key(event));
        }
        w
    };
    let mut x = area.x;
    for (i, b) in tab.iter().enumerate() {
        if shown.contains(&(false, i)) {
            x += draw(f, b, x);
        }
    }
    let global: Vec<&Binding> = keymap::GLOBAL
        .iter()
        .enumerate()
        .filter(|(i, _)| shown.contains(&(true, *i)))
        .map(|(_, b)| b)
        .collect();
    let global_w: u16 =
        global.iter().map(|b| hint_width(b)).sum::<u16>() + hint_width(&keymap::HELP);
    let mut x = area.right().saturating_sub(global_w + 1).max(x);
    for b in global {
        x += draw(f, b, x);
    }
    if x + hint_width(&keymap::HELP) <= area.right() {
        draw(f, &keymap::HELP, x);
    }
}

/// Every global key and every key of what is on screen, from the keymap.
pub fn render_overlay(f: &mut Frame, app: &App) {
    let t = &app.theme;
    let area = f.area();
    app.add_hit(area, Hit::CloseHelp);
    if area.height < 10 || area.width < 30 {
        let y = area.height.saturating_sub(2);
        f.render_widget(
            Paragraph::new(Span::styled(" Press ? to close help ", t.key_hint())),
            Rect::new(0, y, area.width, 1),
        );
        return;
    }

    let section = |s: &str| {
        Line::from(Span::styled(
            s.to_string(),
            Style::default().fg(t.accent).bold(),
        ))
    };
    let lines = |bindings: &[Binding]| -> Vec<Line<'static>> {
        let key_w = bindings
            .iter()
            .map(|b| b.key.chars().count())
            .max()
            .unwrap_or(0)
            + 2;
        bindings
            .iter()
            .map(|b| {
                Line::from(vec![
                    Span::styled(format!("  {:<key_w$}", b.key), t.key_hint()),
                    Span::styled(b.help, Style::default().fg(t.text)),
                ])
            })
            .collect()
    };
    let (title, active) = keymap::active(app);
    let mut left = vec![section("Global")];
    left.extend(lines(keymap::GLOBAL));
    let mut right = vec![section(title)];
    right.extend(lines(active));

    let widest = |col: &[Line]| col.iter().map(Line::width).max().unwrap_or(0) as u16;
    let (lw, rw) = (widest(&left), widest(&right));
    let two_col = lw + rw + 8 <= area.width.saturating_sub(4);
    let (width, rows) = if two_col {
        (lw + rw + 8, left.len().max(right.len()))
    } else {
        (lw.max(rw) + 4, left.len() + right.len() + 1)
    };
    // One blank row above and below the content
    let height = (rows as u16 + 4).min(area.height.saturating_sub(2));
    let rect = centered(area, width.min(area.width.saturating_sub(4)), height);
    app.add_hit(rect, Hit::Block);
    let block = popup(f, rect, "Keys", t.accent, t);
    let inner = block.inner(rect);
    f.render_widget(block, rect);
    let inner = Rect {
        y: inner.y + 1,
        height: inner.height.saturating_sub(1),
        ..inner
    };
    if two_col {
        let [l, r] = Layout::horizontal([Constraint::Length(lw), Constraint::Min(0)])
            .spacing(4)
            .areas(inner);
        f.render_widget(Paragraph::new(left), l);
        f.render_widget(Paragraph::new(right), r);
    } else {
        left.push(Line::from(""));
        left.extend(right);
        f.render_widget(Paragraph::new(left), inner);
    }
}

use super::{centered, popup};
use crate::dashboard::app::{App, Hit, Tab};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::{prelude::*, widgets::Paragraph};

type Hint = (&'static str, &'static str, KeyEvent);

const fn k(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}

const FILES: &[Hint] = &[
    ("⏎", "expand/diff", k(KeyCode::Enter)),
    ("i", "import", k(KeyCode::Char('i'))),
    ("t", "shared", k(KeyCode::Char('t'))),
    ("R", "restore", k(KeyCode::Char('R'))),
    ("x", "remove", k(KeyCode::Char('x'))),
];
const PACKAGES: &[Hint] = &[
    ("⏎", "expand", k(KeyCode::Enter)),
    ("x", "uninstall", k(KeyCode::Char('x'))),
    ("i", "import", k(KeyCode::Char('i'))),
    ("t", "profiles", k(KeyCode::Char('t'))),
    ("h", "history", k(KeyCode::Char('h'))),
    ("R", "rollback", k(KeyCode::Char('R'))),
];
const MACHINES: &[Hint] = &[
    ("⏎", "details", k(KeyCode::Enter)),
    ("p", "profile", k(KeyCode::Char('p'))),
    ("D", "remove old id", k(KeyCode::Char('D'))),
];
const CONFIG: &[Hint] = &[("⏎", "edit", k(KeyCode::Enter))];
const SECURITY: &[Hint] = &[
    ("a", "approve", k(KeyCode::Char('a'))),
    ("x", "reject", k(KeyCode::Char('x'))),
    ("A", "approve all", k(KeyCode::Char('A'))),
    ("M", "approve machine", k(KeyCode::Char('M'))),
    ("⏎", "details", k(KeyCode::Enter)),
];

fn tab_hints(tab: Tab) -> &'static [Hint] {
    match tab {
        Tab::Overview => &[],
        Tab::Files => FILES,
        Tab::Packages => PACKAGES,
        Tab::Machines => MACHINES,
        Tab::Config => CONFIG,
        Tab::Security => SECURITY,
    }
}

const GLOBAL: &[Hint] = &[
    ("s", "sync", k(KeyCode::Char('s'))),
    ("d", "daemon", k(KeyCode::Char('d'))),
    ("r", "refresh", k(KeyCode::Char('r'))),
    (
        "^K",
        "commands",
        KeyEvent::new(KeyCode::Char('k'), KeyModifiers::CONTROL),
    ),
    ("?", "help", k(KeyCode::Char('?'))),
    ("q", "quit", k(KeyCode::Char('q'))),
];

/// Key hints: the active tab's on the left, global ones on the right. Each hint is clickable.
pub fn render_bar(f: &mut Frame, area: Rect, app: &App) {
    let t = &app.theme;
    let draw = |f: &mut Frame, hints: &[Hint], mut x: u16, limit: u16| {
        for (key, desc, code) in hints {
            let w = (key.chars().count() + desc.len() + 3) as u16;
            if x + w > limit {
                break;
            }
            let rect = Rect::new(x, area.y, w, 1);
            f.render_widget(
                Line::from(vec![
                    Span::styled(format!(" {}", key), t.key_hint()),
                    Span::styled(format!(" {} ", desc), Style::default().fg(t.muted)),
                ]),
                rect,
            );
            app.add_hit(rect, Hit::Key(*code));
            x += w;
        }
    };
    let global_w: u16 = GLOBAL
        .iter()
        .map(|(k, d, _)| (k.chars().count() + d.len() + 3) as u16)
        .sum();
    let right_start = area.right().saturating_sub(global_w + 1).max(area.x);
    let hints = if app.active_tab == Tab::Security && app.state.inbox.items.is_empty() {
        &[]
    } else {
        tab_hints(app.active_tab)
    };
    draw(f, hints, area.x, right_start);
    draw(f, GLOBAL, right_start, area.right());
}

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

    let tabs = format!("Tab / 1-{}", Tab::all().len());
    let section =
        |s: &'static str| Line::from(Span::styled(s, Style::default().fg(t.accent).bold()));
    let key = |k: &str, d: &'static str| {
        Line::from(vec![
            Span::styled(format!("  {:<11}", k), t.key_hint()),
            Span::styled(d, Style::default().fg(t.text)),
        ])
    };
    let left = vec![
        section("Global"),
        key("Ctrl+K", "Command palette"),
        key("s", "Sync now"),
        key("d", "Start/stop daemon"),
        key("r", "Refresh"),
        key(&tabs, "Next tab / switch tab"),
        key("j/k ↑↓", "Move"),
        key("Enter", "Expand / edit"),
        key("?", "Toggle help"),
        key("Esc", "Close / collapse"),
        key("q", "Quit"),
        key("Ctrl+C", "Force quit"),
        key("click", "Select; again to open"),
        Line::from(""),
        section("Security"),
        key("a", "Approve, or trust key"),
        key("x", "Reject"),
        key("A", "Approve all safe items"),
        key("M", "Approve all from its machine"),
        key("Enter", "Details"),
    ];
    let right = vec![
        section("Files"),
        key("Enter", "Open history, diff"),
        key("i", "Import from profile"),
        key("t", "Toggle shared"),
        key("R", "Restore to commit"),
        key("x", "Remove from profile"),
        Line::from(""),
        section("Packages"),
        key("Enter", "Expand"),
        key("x", "Uninstall"),
        key("i", "Import from machines"),
        key("h", "Manifest history"),
        key("R", "Roll back to entry"),
        Line::from(""),
        section("Config list"),
        key("a / x", "Add / remove item"),
        key("t", "Toggle create"),
        Line::from(""),
        section("Machines"),
        key("D", "Remove old id record"),
    ];

    // One blank row above and below the longest column.
    let two_col = area.width >= 78 && area.height >= 24;
    let height = if two_col { 24 } else { 43 }.min(area.height.saturating_sub(2));
    let width = if two_col { 80 } else { 44 }.min(area.width.saturating_sub(4));
    let rect = centered(area, width, height);
    app.add_hit(rect, Hit::Block);
    let block = popup(f, rect, "Keyboard shortcuts", t.accent, t);
    let inner = block.inner(rect);
    f.render_widget(block, rect);
    let inner = Rect {
        y: inner.y + 1,
        height: inner.height.saturating_sub(1),
        ..inner
    };
    if two_col {
        let cols = Layout::horizontal([Constraint::Percentage(50), Constraint::Percentage(50)])
            .spacing(2)
            .split(inner);
        f.render_widget(Paragraph::new(left), cols[0]);
        f.render_widget(Paragraph::new(right), cols[1]);
    } else {
        let mut all = left;
        all.push(Line::from(""));
        all.extend(right);
        f.render_widget(Paragraph::new(all), inner);
    }
}

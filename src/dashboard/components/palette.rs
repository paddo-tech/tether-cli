//! Ctrl-K command palette: fuzzy search over actions, tabs, files and packages.

use super::{cursor_down, files, manager_label, scroll_for, select_row, truncate};
use crate::dashboard::app::{Action, App, Hit, Overlay, Tab};
use crate::packages::inbox::Kind;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::{
    prelude::*,
    widgets::{Block, BorderType, Borders, Clear, Paragraph},
};

#[derive(Clone, PartialEq, Debug)]
pub enum Target {
    Action(Action),
    Tab(Tab),
    File {
        section: String,
        path: String,
    },
    Package {
        manager_key: String,
        name: String,
    },
    /// Show an inbox item by id with its details. Deciding happens on the Security tab.
    Inbox(String),
    /// Ask before removing a record that looks like an old id of this machine.
    RemoveOldRecord(String),
}

pub struct Entry {
    pub label: String,
    pub kind: String,
    pub target: Target,
}

pub struct Palette {
    pub entries: Vec<Entry>,
    pub query: String,
    /// Entry index and matched char positions, best first.
    pub matches: Vec<(usize, Vec<usize>)>,
    pub cursor: usize,
}

impl Palette {
    pub fn new(entries: Vec<Entry>) -> Self {
        let mut p = Self {
            entries,
            query: String::new(),
            matches: Vec::new(),
            cursor: 0,
        };
        p.refilter();
        p
    }

    fn refilter(&mut self) {
        let mut scored: Vec<(i32, usize, Vec<usize>)> = self
            .entries
            .iter()
            .enumerate()
            .filter_map(|(i, e)| fuzzy_match(&self.query, &e.label).map(|(s, pos)| (s, i, pos)))
            .collect();
        // Stable sort keeps entry order (actions, tabs, files, packages) among equal scores.
        scored.sort_by_key(|m| std::cmp::Reverse(m.0));
        self.matches = scored.into_iter().map(|(_, i, pos)| (i, pos)).collect();
        self.cursor = 0;
    }

    pub fn selected(&self) -> Option<&Target> {
        self.matches
            .get(self.cursor)
            .map(|(i, _)| &self.entries[*i].target)
    }
}

/// Score `candidate` against `query` as a case-insensitive subsequence.
/// Consecutive runs and matches at word starts score higher; gaps cost a little.
/// Returns the score and the matched char positions, or None if not a subsequence.
pub fn fuzzy_match(query: &str, candidate: &str) -> Option<(i32, Vec<usize>)> {
    let q: Vec<char> = query.chars().filter(|c| !c.is_whitespace()).collect();
    if q.is_empty() {
        return Some((0, Vec::new()));
    }
    let c: Vec<char> = candidate.chars().collect();
    let lower: Vec<char> = c.iter().map(|ch| ch.to_ascii_lowercase()).collect();
    let boundary = |i: usize| {
        i == 0
            || matches!(c[i - 1], '/' | '.' | '_' | '-' | ' ' | ':')
            || (c[i - 1].is_lowercase() && c[i].is_uppercase())
    };

    // Align from `start`. With `prefer_words`, jump ahead to a nearby word start;
    // that can strand later query chars, so the plain earliest match is the fallback.
    let align = |start: usize, prefer_words: bool| -> Option<Vec<usize>> {
        let mut pos = vec![start];
        let mut ci = start + 1;
        for &qc in &q[1..] {
            let qc = qc.to_ascii_lowercase();
            let mut found = (ci..lower.len()).find(|&i| lower[i] == qc)?;
            if prefer_words && found != ci {
                if let Some(b) = (found..lower.len()).find(|&i| lower[i] == qc && boundary(i)) {
                    if b - found <= 8 {
                        found = b;
                    }
                }
            }
            pos.push(found);
            ci = found + 1;
        }
        Some(pos)
    };

    // Try every start of the first char and keep the best alignment.
    let mut best: Option<(i32, Vec<usize>)> = None;
    let first = q[0].to_ascii_lowercase();
    for start in (0..lower.len()).filter(|&i| lower[i] == first) {
        let Some(pos) = align(start, true).or_else(|| align(start, false)) else {
            continue;
        };
        let mut score = 0i32;
        for (k, &p) in pos.iter().enumerate() {
            score += 16;
            if boundary(p) {
                score += 10;
            }
            if k > 0 && p == pos[k - 1] + 1 {
                score += 15;
            } else if k > 0 {
                score -= (p - pos[k - 1] - 1).min(10) as i32;
            }
        }
        score -= (pos[0]).min(15) as i32;
        score -= (c.len() / 8) as i32;
        if best.as_ref().is_none_or(|(s, _)| score > *s) {
            best = Some((score, pos));
        }
    }
    best
}

/// Everything the palette can jump to or run, in display order for an empty query.
pub fn entries(app: &App) -> Vec<Entry> {
    let mut out = Vec::new();
    let daemon = if app.state.daemon_running {
        "Stop daemon"
    } else {
        "Start daemon"
    };
    for (label, action) in [
        ("Sync now", Action::Sync),
        (daemon, Action::ToggleDaemon),
        ("Refresh", Action::Refresh),
        (
            "Import packages from other machines",
            Action::ImportPackages,
        ),
        ("Import dotfile from another profile", Action::ImportDotfile),
        ("Switch this machine's profile", Action::PickProfile),
        ("Keyboard shortcuts", Action::Help),
        ("Quit", Action::Quit),
    ] {
        out.push(Entry {
            label: label.to_string(),
            kind: "action".into(),
            target: Target::Action(action),
        });
    }
    for id in app.state.old_ids.iter().map(|o| &o.machine_id) {
        out.push(Entry {
            label: format!("Remove old record {}", id),
            kind: "action".into(),
            target: Target::RemoveOldRecord(id.clone()),
        });
    }
    let pending = &app.state.inbox.items;
    // Approve all skips machine keys, so only a safe package makes the action useful.
    if pending.iter().any(|i| i.bulk_approvable()) {
        out.push(Entry {
            label: "Approve all pending packages".into(),
            kind: "action".into(),
            target: Target::Action(Action::ApproveAll),
        });
    }
    for item in pending {
        let name = match item.kind {
            Kind::Package => format!("{} ({})", item.name, manager_label(&item.manager)),
            Kind::TrustMachine { .. } => format!("machine key of {}", item.name),
        };
        out.push(Entry {
            label: format!("Review {}", name),
            kind: "inbox".into(),
            target: Target::Inbox(item.id()),
        });
    }
    for tab in Tab::all() {
        out.push(Entry {
            label: format!("Go to {}", tab.title()),
            kind: "tab".into(),
            target: Target::Tab(*tab),
        });
    }
    let mut section = String::new();
    for row in files::build_overview_rows(&app.state) {
        match row {
            files::FileRow::SectionHeader { label, .. } => section = label,
            files::FileRow::File { path, .. } => out.push(Entry {
                label: path.clone(),
                kind: "file".into(),
                target: Target::File {
                    section: section.clone(),
                    path,
                },
            }),
            _ => {}
        }
    }
    let machine_id = app.machine_id();
    if let Some(m) = app
        .state
        .machines
        .iter()
        .find(|m| m.machine_id == machine_id)
    {
        let mut managers: Vec<_> = m.packages.iter().collect();
        managers.sort_by(|a, b| a.0.cmp(b.0));
        for (key, pkgs) in managers {
            let mut pkgs = pkgs.clone();
            pkgs.sort();
            for name in pkgs {
                out.push(Entry {
                    label: name.clone(),
                    kind: manager_label(key).to_string(),
                    target: Target::Package {
                        manager_key: key.clone(),
                        name,
                    },
                });
            }
        }
    }
    out
}

/// The overlay was popped off the stack; push it back to keep it open.
/// Returns the chosen target when the user runs one.
pub fn handle_key(app: &mut App, mut p: Palette, key: KeyEvent) -> Option<Target> {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    match key.code {
        KeyCode::Esc => return None,
        KeyCode::Char('k') if ctrl => return None,
        KeyCode::Enter => return p.selected().cloned(),
        KeyCode::Down => cursor_down(&mut p.cursor, p.matches.len()),
        KeyCode::Char('n') if ctrl => cursor_down(&mut p.cursor, p.matches.len()),
        KeyCode::Up => p.cursor = p.cursor.saturating_sub(1),
        KeyCode::Char('p') if ctrl => p.cursor = p.cursor.saturating_sub(1),
        KeyCode::Backspace => {
            p.query.pop();
            p.refilter();
        }
        KeyCode::Char(c) if !ctrl => {
            p.query.push(c);
            p.refilter();
        }
        _ => {}
    }
    app.overlays.push(Overlay::Palette(p));
    None
}

pub fn render(f: &mut Frame, app: &App, p: &Palette) {
    let t = &app.theme;
    let area = f.area();
    let width = 76.min(area.width.saturating_sub(4));
    let max_rows = (area.height.saturating_sub(8) as usize).min(14);
    let rows = p.matches.len().clamp(1, max_rows.max(1));
    let height = (rows as u16 + 4).min(area.height);
    let x = area.x + (area.width - width) / 2;
    let y = area.y
        + (area.height.saturating_sub(height) / 4)
            .max(1)
            .min(area.height - height);
    let rect = Rect::new(x, y, width, height);
    if width < 20 || height < 5 {
        return;
    }

    f.render_widget(Clear, rect);
    let hint = Line::from(vec![
        Span::styled(" ↑↓", t.key_hint()),
        Span::styled(" move  ", Style::default().fg(t.muted)),
        Span::styled("⏎", t.key_hint()),
        Span::styled(" run  ", Style::default().fg(t.muted)),
        Span::styled("esc", t.key_hint()),
        Span::styled(" close ", Style::default().fg(t.muted)),
    ]);
    let count = Line::from(Span::styled(
        format!(" {}/{} ", p.matches.len(), p.entries.len()),
        Style::default().fg(t.dim),
    ));
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(t.accent))
        .title(Line::from(" Commands ").style(Style::default().fg(t.accent).bold()))
        .title_top(count.right_aligned())
        .title_bottom(hint.right_aligned())
        .style(Style::default().bg(t.popup_bg).fg(t.text));
    let inner = block.inner(rect);
    f.render_widget(block, rect);

    let input = if p.query.is_empty() {
        Line::from(vec![
            Span::styled(" ❯ ", Style::default().fg(t.accent).bold()),
            Span::styled("▏", Style::default().fg(t.accent)),
            Span::styled(
                "Search actions, tabs, files, packages",
                Style::default().fg(t.dim).italic(),
            ),
        ])
    } else {
        Line::from(vec![
            Span::styled(" ❯ ", Style::default().fg(t.accent).bold()),
            Span::styled(p.query.as_str(), Style::default().fg(t.text).bold()),
            Span::styled("▏", Style::default().fg(t.accent)),
        ])
    };
    f.render_widget(Paragraph::new(input), Rect { height: 1, ..inner });
    f.render_widget(
        Paragraph::new("─".repeat(inner.width as usize)).style(Style::default().fg(t.border)),
        Rect::new(inner.x, inner.y + 1, inner.width, 1),
    );

    let list = Rect::new(
        inner.x + 1,
        inner.y + 2,
        inner.width.saturating_sub(2),
        inner.height.saturating_sub(2),
    );
    if p.matches.is_empty() {
        f.render_widget(
            Paragraph::new(Span::styled(" No matches", Style::default().fg(t.dim))),
            list,
        );
        return;
    }
    let visible = list.height as usize;
    let scroll = scroll_for(p.cursor, visible);
    for (i, (idx, pos)) in p.matches.iter().enumerate().skip(scroll).take(visible) {
        let entry = &p.entries[*idx];
        let row = Rect::new(list.x, list.y + (i - scroll) as u16, list.width, 1);
        let selected = i == p.cursor;
        if selected {
            select_row(f, row, t);
        }
        let (icon, icon_color) = match entry.target {
            Target::Action(_) => ("»", t.accent),
            Target::RemoveOldRecord(_) => ("»", t.warn),
            Target::Tab(_) => ("#", t.info),
            Target::File { .. } => ("◇", t.ok),
            Target::Package { .. } => ("▪", t.key),
            Target::Inbox(_) => ("◆", t.warn),
        };
        let max_label = (list.width as usize).saturating_sub(entry.kind.len() + 6);
        let label = truncate(&entry.label, max_label);
        let base = if selected {
            Style::default().fg(t.text).bold()
        } else {
            Style::default().fg(t.text)
        };
        let hl = Style::default().fg(t.accent).bold().underlined();
        let mut spans = vec![Span::styled(
            format!(" {} ", icon),
            Style::default().fg(icon_color),
        )];
        spans.extend(label.chars().enumerate().map(|(ci, ch)| {
            Span::styled(ch.to_string(), if pos.contains(&ci) { hl } else { base })
        }));
        super::row(
            f,
            row,
            Line::from(spans),
            Line::from(Span::styled(
                format!("{} ", entry.kind),
                Style::default().fg(t.dim),
            )),
        );
        app.add_hit(row, Hit::Item(i));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fuzzy_requires_subsequence() {
        assert!(fuzzy_match("zrc", ".zshrc").is_some());
        assert!(fuzzy_match("xyz", ".zshrc").is_none());
        assert_eq!(fuzzy_match("", "anything").map(|m| m.0), Some(0));
    }

    #[test]
    fn fuzzy_falls_back_when_word_start_strands_the_query() {
        assert_eq!(
            fuzzy_match("abc", "axbc-b").map(|m| m.1),
            Some(vec![0, 2, 3])
        );
    }

    #[test]
    fn fuzzy_is_case_insensitive_and_reports_positions() {
        let (_, pos) = fuzzy_match("SN", "Sync now").unwrap();
        assert_eq!(pos, vec![0, 5]);
    }

    #[test]
    fn fuzzy_prefers_word_starts_and_runs() {
        let score = |q, c| fuzzy_match(q, c).unwrap().0;
        assert!(score("gc", ".gitconfig") > score("gc", ".config/magic"));
        assert!(score("sync", "Sync now") > score("sync", "Switch this machine's profile x y n c"));
        assert!(score("zsh", ".zshrc") > score("zsh", ".config/z/s/h"));
    }

    #[test]
    fn palette_ranks_best_match_first() {
        let entry = |label: &str| Entry {
            label: label.into(),
            kind: "file".into(),
            target: Target::Tab(Tab::Files),
        };
        let mut p = Palette::new(vec![entry(".config/gh/hosts.yml"), entry(".gitconfig")]);
        assert_eq!(p.matches.len(), 2);
        p.query = "gitc".into();
        p.refilter();
        assert_eq!(p.matches.len(), 1);
        assert_eq!(p.entries[p.matches[0].0].label, ".gitconfig");
    }
}

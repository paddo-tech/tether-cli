//! Security tab: the approval inbox (synced packages and machine keys that wait for this
//! machine's consent) and the machines whose signing keys it trusts.

use super::confirm::Confirm;
use super::{
    clamp_cursor, cursor_down, manager_label, panel, row, scroll_for, scrollbar, select_row,
};
use crate::cli::output::relative_time;
use crate::dashboard::app::{App, Hit, Overlay};
use crate::dashboard::msg::{Cmd, KeyOutcome};
use crate::dashboard::theme::{mix, Theme};
use crate::packages::inbox::{self, Decision, Inbox, InboxItem, Kind, Reason};
use crossterm::event::{KeyCode, KeyEvent};
use ratatui::{
    prelude::*,
    widgets::{Paragraph, Wrap},
};

/// Rows per inbox item in the list: two lines and a gap.
const ITEM_H: u16 = 3;
/// Below this body width the detail pane opens under the list on Enter.
const SPLIT_MIN_W: u16 = 110;

#[derive(Default)]
pub struct SecurityTabState {
    /// Index into `state.inbox.items`.
    pub cursor: usize,
    /// Narrow layouts show the detail pane only after Enter.
    pub detail: bool,
}

pub fn handle_key(app: &mut App, key: KeyEvent) -> KeyOutcome {
    let len = app.state.inbox.items.len();
    let cmd = match key.code {
        KeyCode::Char('j') | KeyCode::Down => {
            cursor_down(&mut app.security.cursor, len);
            None
        }
        KeyCode::Char('k') | KeyCode::Up => {
            app.security.cursor = app.security.cursor.saturating_sub(1);
            None
        }
        KeyCode::Enter => {
            app.security.detail = !app.security.detail;
            None
        }
        KeyCode::Char('a') => selected_id(app).and_then(|id| approve(app, &id)),
        KeyCode::Char('x') => {
            if let Some(id) = selected_id(app) {
                reject(app, &id);
            }
            None
        }
        KeyCode::Char('A') => {
            confirm_approve_all(app);
            None
        }
        _ => return KeyOutcome::Ignored,
    };
    KeyOutcome::Handled(cmd)
}

fn selected_id(app: &App) -> Option<String> {
    app.state
        .inbox
        .items
        .get(app.security.cursor)
        .map(|i| i.id())
}

/// Move the cursor to an item, for palette actions.
pub fn select(app: &mut App, id: &str) {
    if let Some(i) = app.state.inbox.items.iter().position(|i| i.id() == id) {
        app.security.cursor = i;
    }
}

fn reload(app: &mut App) {
    app.state.inbox = Inbox::load().unwrap_or_default();
    app.state.trusted = inbox::trusted_machines().unwrap_or_default();
    clamp_cursor(&mut app.security.cursor, app.state.inbox.items.len());
}

/// One install runs at a time, and its result toast names what it installed.
fn install_busy(app: &mut App) -> bool {
    if app.installing.is_some() {
        app.flash_error("Wait for the running install to finish");
    }
    app.installing.is_some()
}

/// Record approval, then install a package in the background. A machine item trusts its key.
pub fn approve(app: &mut App, id: &str) -> Option<Cmd> {
    let package = app
        .state
        .inbox
        .items
        .iter()
        .any(|i| i.id() == id && i.kind == Kind::Package);
    if package && install_busy(app) {
        return None;
    }
    let result = inbox::approve(id);
    reload(app);
    match result {
        Ok(item) => match &item.kind {
            Kind::Package => Some(app.start_inbox_install(item.name.clone(), vec![item])),
            Kind::TrustMachine { fingerprint, .. } => {
                app.flash_success(format!(
                    "Trusted {} ({})",
                    machine_name(app, &item.name),
                    fingerprint
                ));
                None
            }
        },
        Err(e) => {
            app.flash_error(e.to_string());
            None
        }
    }
}

pub fn reject(app: &mut App, id: &str) {
    let result = inbox::reject(id);
    reload(app);
    match result {
        Ok(item) => app.flash_success(format!("Rejected {}", item.name)),
        Err(e) => app.flash_error(e.to_string()),
    }
}

/// Machine keys are left out: each needs its fingerprint checked on its own.
pub fn confirm_approve_all(app: &mut App) {
    let packages = app
        .state
        .inbox
        .items
        .iter()
        .filter(|i| i.kind == Kind::Package);
    let malicious = packages.clone().filter(|i| is_malicious(i)).count();
    let count = packages.count() - malicious;
    if count == 0 {
        app.flash_info("Nothing to approve");
    } else if !install_busy(app) {
        app.overlays
            .push(Overlay::Confirm(Confirm::ApproveAll { count, malicious }));
    }
}

/// Approve every item that approval allows, then install them in one background run.
pub fn approve_all(app: &mut App) -> Option<Cmd> {
    if install_busy(app) {
        return None;
    }
    let ids: Vec<String> = app
        .state
        .inbox
        .items
        .iter()
        .filter(|i| i.kind == Kind::Package && !is_malicious(i))
        .map(InboxItem::id)
        .collect();
    let mut approved = Vec::new();
    for id in ids {
        match inbox::approve(&id) {
            Ok(item) => approved.push(item),
            Err(e) => app.flash_error(e.to_string()),
        }
    }
    reload(app);
    let label = match approved.as_slice() {
        [] => return None,
        [one] => one.name.clone(),
        many => format!("{} packages", many.len()),
    };
    Some(app.start_inbox_install(label, approved))
}

fn is_malicious(item: &InboxItem) -> bool {
    item.reasons.contains(&Reason::Malicious)
}

/// Short badge text for the list; the detail pane uses `Reason::label`.
fn badge(reason: Reason) -> &'static str {
    match reason {
        Reason::Unsigned => "unsigned",
        Reason::UntrustedTap => "untrusted tap",
        Reason::CooldownUnsupported => "no age check",
        Reason::Malicious => "MALICIOUS",
        Reason::UntrustedSigner => "untrusted signer",
        Reason::NewMachine => "new machine",
        Reason::KeyChanged => "KEY CHANGED",
    }
}

fn reason_color(reason: Reason, t: &Theme) -> Color {
    match reason {
        Reason::Unsigned => t.warn,
        Reason::UntrustedTap => t.key,
        Reason::CooldownUnsupported => t.info,
        Reason::Malicious | Reason::KeyChanged => t.error,
        Reason::UntrustedSigner => t.key,
        Reason::NewMachine => t.info,
    }
}

fn explain(reason: Reason) -> &'static str {
    match reason {
        Reason::Unsigned => {
            "Another machine added this package. Tether cannot yet prove that machine made the change."
        }
        Reason::UntrustedTap => {
            "It comes from a Homebrew tap outside your trusted taps. Approval also trusts the tap."
        }
        Reason::CooldownUnsupported => {
            "The installed package manager cannot enforce the minimum release age, so a very new release could install."
        }
        Reason::Malicious => {
            "OSV reports this package as malicious. Tether will not install it, even with approval."
        }
        Reason::UntrustedSigner => {
            "A valid signature covers the change, but this machine does not trust the key that made it."
        }
        Reason::NewMachine => {
            "This machine published a signing key for the first time. Trust it only if you set it up."
        }
        Reason::KeyChanged => {
            "SIGNING KEY CHANGED. This machine was trusted with a different key. If you did not set it up again, someone may be signing as it. Run `tether machines` on that machine and compare the fingerprint before you trust it."
        }
    }
}

/// Soft pill: tinted background, colored text. Malicious gets a solid pill so it stands out.
fn pill(text: &str, color: Color, t: &Theme) -> Span<'static> {
    let style = if color == t.error {
        Style::default().fg(t.brand_fg).bg(color).bold()
    } else if t.rgb {
        Style::default().fg(color).bg(mix(t.base_bg, color, 0.18))
    } else {
        Style::default().fg(color).reversed()
    };
    Span::styled(format!(" {} ", text), style)
}

fn machine_name(app: &App, id: &str) -> String {
    app.state
        .machines
        .iter()
        .find(|m| m.machine_id == id)
        .map(|m| m.hostname.trim_end_matches(".local").to_string())
        .filter(|h| !h.is_empty())
        .unwrap_or_else(|| id.to_string())
}

fn short(commit: &str) -> &str {
    &commit[..commit.len().min(7)]
}

/// How one inbox item draws: its two list lines and its detail pane.
/// A new item kind adds a match arm here and nowhere else in the view.
struct ItemView {
    heading: String,
    title: Line<'static>,
    badges: Line<'static>,
    meta: Line<'static>,
    advisories: Line<'static>,
    detail: Vec<Line<'static>>,
    /// Label of the approve button; None when only rejection is possible.
    approve: Option<&'static str>,
}

fn item_view(app: &App, item: &InboxItem) -> ItemView {
    match &item.kind {
        Kind::Package => package_view(app, item),
        Kind::TrustMachine { fingerprint, .. } => machine_view(app, item, fingerprint),
    }
}

fn reason_lines(item: &InboxItem, t: &Theme) -> Vec<Line<'static>> {
    let mut lines = vec![Line::from(Span::styled(
        "Why it is held",
        Style::default().fg(t.accent).bold(),
    ))];
    for reason in &item.reasons {
        let loud = *reason == Reason::KeyChanged;
        lines.push(Line::from(pill(
            reason.label(),
            reason_color(*reason, t),
            t,
        )));
        lines.push(Line::from(Span::styled(
            explain(*reason),
            if loud {
                Style::default().fg(t.error).bold()
            } else {
                Style::default().fg(t.muted)
            },
        )));
    }
    lines
}

fn badges(item: &InboxItem, t: &Theme) -> Line<'static> {
    let mut spans = Vec::new();
    for reason in &item.reasons {
        spans.push(pill(badge(*reason), reason_color(*reason, t), t));
        spans.push(Span::raw(" "));
    }
    spans.pop();
    Line::from(spans)
}

fn kv(k: &str, v: Span<'static>, t: &Theme) -> Line<'static> {
    Line::from(vec![
        Span::styled(format!("{:<12}", k), Style::default().fg(t.dim)),
        v,
    ])
}

fn machine_view(app: &App, item: &InboxItem, fingerprint: &str) -> ItemView {
    let t = &app.theme;
    let changed = item.reasons.contains(&Reason::KeyChanged);
    let color = if changed { t.error } else { t.info };
    let name = machine_name(app, &item.name);
    let title = Line::from(vec![
        Span::styled(
            if changed { "▲ " } else { "◇ " },
            Style::default().fg(color),
        ),
        Span::styled("trust ", Style::default().fg(t.dim)),
        Span::styled(name.clone(), Style::default().fg(t.text).bold()),
        Span::styled("  machine key", Style::default().fg(t.dim)),
    ]);
    let meta = Line::from(vec![
        Span::raw("  "),
        Span::styled(fingerprint.to_string(), Style::default().fg(t.hash)),
        Span::styled(" · ", Style::default().fg(t.border)),
        Span::styled(
            format!("held {}", relative_time(item.first_seen)),
            Style::default().fg(t.dim),
        ),
    ]);
    let text = |s: String| Span::styled(s, Style::default().fg(t.text));
    let mut detail = vec![kv("Machine", text(name.clone()), t)];
    if name != item.name {
        detail.push(kv("Machine ID", text(item.name.clone()), t));
    }
    detail.push(kv(
        "Key",
        Span::styled(fingerprint.to_string(), Style::default().fg(t.hash).bold()),
        t,
    ));
    detail.push(kv("First seen", text(relative_time(item.first_seen)), t));
    detail.push(Line::default());
    detail.extend(reason_lines(item, t));
    detail.push(Line::default());
    detail.push(Line::from(Span::styled(
        "Trusting this key lets package changes signed by it install without approval.",
        Style::default().fg(t.dim),
    )));
    ItemView {
        heading: name,
        title,
        badges: badges(item, t),
        meta,
        advisories: Line::default(),
        detail,
        approve: Some("trust key"),
    }
}

fn package_view(app: &App, item: &InboxItem) -> ItemView {
    let t = &app.theme;
    let malicious = is_malicious(item);
    let icon_color = if malicious { t.error } else { t.warn };

    let mut title = vec![
        Span::styled(
            if malicious { "✗ " } else { "◆ " },
            Style::default().fg(icon_color),
        ),
        Span::styled(item.name.clone(), Style::default().fg(t.text).bold()),
    ];
    if let Some(v) = &item.version {
        title.push(Span::styled(format!(" {}", v), Style::default().fg(t.info)));
    }
    title.push(Span::styled(
        format!("  {}", manager_label(&item.manager)),
        Style::default().fg(t.dim),
    ));

    let source = item
        .source_machine
        .as_deref()
        .map(|id| machine_name(app, id));
    let sep = || Span::styled(" · ", Style::default().fg(t.border));
    let mut meta = vec![Span::raw("  ")];
    if let Some(source) = &source {
        meta.push(Span::styled("from ", Style::default().fg(t.dim)));
        meta.push(Span::styled(source.clone(), Style::default().fg(t.muted)));
        meta.push(sep());
    }
    if let Some(commit) = &item.commit {
        meta.push(Span::styled(
            short(commit).to_string(),
            Style::default().fg(t.hash),
        ));
        meta.push(sep());
    }
    meta.push(Span::styled(
        format!("held {}", relative_time(item.first_seen)),
        Style::default().fg(t.dim),
    ));

    let advisory_style = |id: &str| {
        if crate::packages::osv::is_malicious(id) {
            Style::default().fg(t.error).bold()
        } else {
            Style::default().fg(t.warn)
        }
    };
    let mut advisories = Vec::new();
    if let Some(first) = item.advisories.first() {
        advisories.push(Span::styled(first.clone(), advisory_style(first)));
        if item.advisories.len() > 1 {
            advisories.push(Span::styled(
                format!(" +{}", item.advisories.len() - 1),
                Style::default().fg(t.dim),
            ));
        }
    }

    let kv = |k: &str, v: Span<'static>| kv(k, v, t);
    let text = |s: String| Span::styled(s, Style::default().fg(t.text));
    let section = |s: String| Line::from(Span::styled(s, Style::default().fg(t.accent).bold()));
    let mut detail = vec![
        kv(
            "Version",
            Span::styled(
                item.version.clone().unwrap_or_else(|| "latest".into()),
                Style::default().fg(t.info),
            ),
        ),
        kv("Manager", text(manager_label(&item.manager).to_string())),
    ];
    if let Some(tap) = &item.tap {
        detail.push(kv("Tap", text(tap.clone())));
    }
    if let (Some(source), Some(id)) = (&source, &item.source_machine) {
        let value = if source == id {
            id.clone()
        } else {
            format!("{} ({})", source, id)
        };
        detail.push(kv("From", text(value)));
    }
    if let Some(commit) = &item.commit {
        detail.push(kv(
            "Commit",
            Span::styled(commit.clone(), Style::default().fg(t.hash)),
        ));
    }
    detail.push(kv(
        "First seen",
        text(format!(
            "{} ({})",
            relative_time(item.first_seen),
            item.first_seen
                .with_timezone(&chrono::Local)
                .format("%b %-d %H:%M")
        )),
    ));
    if let Some(signer) = &item.signer {
        detail.push(kv(
            "Signed by",
            Span::styled(signer.clone(), Style::default().fg(t.hash)),
        ));
    }
    detail.push(Line::default());
    detail.extend(reason_lines(item, t));
    detail.push(Line::default());
    detail.push(section(format!("Advisories ({})", item.advisories.len())));
    if item.advisories.is_empty() {
        detail.push(Line::from(Span::styled(
            "OSV lists no advisories for this version",
            Style::default().fg(t.dim),
        )));
    }
    for id in &item.advisories {
        let what = if crate::packages::osv::is_malicious(id) {
            "  malicious, blocks install"
        } else {
            "  vulnerability, does not block"
        };
        detail.push(Line::from(vec![
            Span::styled(id.clone(), advisory_style(id)),
            Span::styled(what, Style::default().fg(t.dim)),
        ]));
        detail.push(Line::from(vec![
            Span::raw("  "),
            Span::styled(
                format!("osv.dev/vulnerability/{}", id),
                Style::default().fg(t.dim).underlined(),
            ),
        ]));
    }

    ItemView {
        heading: item.name.clone(),
        title: Line::from(title),
        badges: badges(item, t),
        meta: Line::from(meta),
        advisories: Line::from(advisories),
        detail,
        approve: (!malicious).then_some("approve & install"),
    }
}

pub fn render(f: &mut Frame, area: Rect, app: &App) {
    let items = &app.state.inbox.items;
    if items.is_empty() {
        let (main, trusted) = split_trusted(area, app);
        render_clean(f, main, app);
        if let Some(trusted) = trusted {
            render_trusted(f, trusted, app);
        }
        return;
    }
    let cursor = app.security.cursor.min(items.len() - 1);
    let view = item_view(app, &items[cursor]);
    if area.width >= SPLIT_MIN_W {
        let [left, detail_area] =
            Layout::horizontal([Constraint::Percentage(58), Constraint::Percentage(42)])
                .spacing(1)
                .areas(area);
        let (list_area, trusted) = split_trusted(left, app);
        render_list(f, list_area, app, cursor);
        if let Some(trusted) = trusted {
            render_trusted(f, trusted, app);
        }
        render_detail(f, detail_area, app, view);
    } else if app.security.detail && area.height >= 12 {
        let list_h = (items.len() as u16 * ITEM_H + 2).clamp(4, area.height / 2);
        let [list_area, detail_area] =
            Layout::vertical([Constraint::Length(list_h), Constraint::Min(6)]).areas(area);
        render_list(f, list_area, app, cursor);
        render_detail(f, detail_area, app, view);
    } else {
        let (list_area, trusted) = split_trusted(area, app);
        render_list(f, list_area, app, cursor);
        if let Some(trusted) = trusted {
            render_trusted(f, trusted, app);
        }
    }
}

/// Room for the trusted-machines panel under `area`, when it leaves the inbox enough height.
fn split_trusted(area: Rect, app: &App) -> (Rect, Option<Rect>) {
    let rows = app.state.trusted.len().clamp(1, 6) as u16;
    if area.height < rows + 2 + 10 {
        return (area, None);
    }
    let [main, trusted] =
        Layout::vertical([Constraint::Min(8), Constraint::Length(rows + 2)]).areas(area);
    (main, Some(trusted))
}

fn render_trusted(f: &mut Frame, area: Rect, app: &App) {
    let t = &app.theme;
    let trusted = &app.state.trusted;
    let block = panel(" Trusted machines ", false, t).title_top(
        Line::from(Span::styled(
            format!(" {} keys ", trusted.len()),
            Style::default().fg(t.dim),
        ))
        .right_aligned(),
    );
    let inner = block.inner(area);
    f.render_widget(block, area);
    if trusted.is_empty() {
        f.render_widget(
            Paragraph::new(Span::styled(
                "No trusted machine keys yet",
                Style::default().fg(t.dim),
            )),
            inner,
        );
        return;
    }
    for (i, m) in trusted.iter().take(inner.height as usize).enumerate() {
        let mut left = vec![
            Span::styled("✓ ", Style::default().fg(t.ok)),
            Span::styled(
                machine_name(app, &m.machine_id),
                Style::default().fg(t.text),
            ),
        ];
        if m.machine_id == app.machine_id() {
            left.push(Span::styled("  this", Style::default().fg(t.accent)));
        }
        row(
            f,
            Rect::new(inner.x, inner.y + i as u16, inner.width, 1),
            Line::from(left),
            Line::from(Span::styled(
                m.fingerprint.clone(),
                Style::default().fg(t.hash),
            )),
        );
    }
}

fn render_list(f: &mut Frame, area: Rect, app: &App, cursor: usize) {
    let t = &app.theme;
    let items = &app.state.inbox.items;
    let malicious = items.iter().filter(|i| is_malicious(i)).count();
    let mut count = vec![Span::styled(
        format!(" {} pending", items.len()),
        Style::default().fg(t.muted),
    )];
    if malicious > 0 {
        count.push(Span::styled(" · ", Style::default().fg(t.border)));
        count.push(Span::styled(
            format!("{} malicious", malicious),
            Style::default().fg(t.error).bold(),
        ));
    }
    count.push(Span::raw(" "));
    let block = panel(" Approval inbox ", true, t).title_top(Line::from(count).right_aligned());
    let inner = block.inner(area);
    f.render_widget(block, area);

    let visible = (inner.height / ITEM_H).max(1) as usize;
    let scroll = scroll_for(cursor, visible);
    for (i, item) in items.iter().enumerate().skip(scroll).take(visible) {
        let y = inner.y + (i - scroll) as u16 * ITEM_H;
        let h = (ITEM_H - 1).min(inner.bottom().saturating_sub(y));
        if h == 0 {
            break;
        }
        let rect = Rect::new(inner.x, y, inner.width, h);
        if i == cursor {
            for dy in 0..h {
                select_row(f, Rect::new(inner.x, y + dy, inner.width, 1), t);
            }
        }
        let view = item_view(app, item);
        row(f, Rect { height: 1, ..rect }, view.title, view.badges);
        if h > 1 {
            row(
                f,
                Rect::new(inner.x, y + 1, inner.width, 1),
                view.meta,
                view.advisories,
            );
        }
        app.add_hit(rect, Hit::Row(i));
    }
    scrollbar(f, area, items.len(), scroll, visible, t);
}

fn render_detail(f: &mut Frame, area: Rect, app: &App, view: ItemView) {
    let t = &app.theme;
    let block = panel(
        Line::from(vec![
            Span::raw(" "),
            Span::raw(view.heading.clone()),
            Span::raw(" "),
        ]),
        false,
        t,
    );
    let inner = block.inner(area);
    f.render_widget(block, area);
    if inner.height == 0 {
        return;
    }
    let buttons_h = if inner.height >= 6 { 2 } else { 0 };
    let body = Rect {
        y: inner.y + 1.min(inner.height),
        height: inner.height.saturating_sub(1 + buttons_h),
        ..inner
    };
    f.render_widget(Paragraph::new(view.detail).wrap(Wrap { trim: false }), body);
    if buttons_h == 0 {
        return;
    }

    let by = inner.bottom() - 1;
    let mut x = inner.x;
    let mut button = |label: &str, key: char, style: Style| {
        let w = label.chars().count() as u16;
        if x + w > inner.right() {
            return;
        }
        let rect = Rect::new(x, by, w, 1);
        f.render_widget(Paragraph::new(label.to_string()).style(style), rect);
        app.add_hit(rect, Hit::Key(KeyEvent::from(KeyCode::Char(key))));
        x += w + 2;
    };
    if let Some(label) = view.approve {
        button(
            &format!(" a  {} ", label),
            'a',
            Style::default().fg(t.brand_fg).bg(t.ok).bold(),
        );
    }
    button(
        " x  reject ",
        'x',
        Style::default().fg(t.text).bg(t.selection),
    );
    if view.approve.is_none() && x < inner.right() {
        f.render_widget(
            Paragraph::new(Span::styled(
                "approval is blocked",
                Style::default().fg(t.error),
            )),
            Rect::new(x, by, inner.right() - x, 1),
        );
    }
}

/// Nothing waits: say so, with the decision counts and the latest decisions.
fn render_clean(f: &mut Frame, area: Rect, app: &App) {
    let t = &app.theme;
    let inbox = &app.state.inbox;
    let block = panel(" Approval inbox ", true, t);
    let inner = block.inner(area);
    f.render_widget(block, area);

    let mut lines = vec![
        Line::from(Span::styled("╭───╮", Style::default().fg(t.ok))),
        Line::from(Span::styled("│ ✓ │", Style::default().fg(t.ok).bold())),
        Line::from(Span::styled("╰───╯", Style::default().fg(t.ok))),
        Line::default(),
        Line::from(Span::styled(
            "This machine is clean",
            Style::default().fg(t.text).bold(),
        )),
        Line::from(Span::styled(
            "No synced packages wait for approval.",
            Style::default().fg(t.muted),
        )),
        Line::default(),
        Line::from(vec![
            Span::styled(
                inbox.approved.len().to_string(),
                Style::default().fg(t.ok).bold(),
            ),
            Span::styled(" approved", Style::default().fg(t.dim)),
            Span::styled("   ·   ", Style::default().fg(t.border)),
            Span::styled(
                inbox.rejected.len().to_string(),
                Style::default().fg(t.error).bold(),
            ),
            Span::styled(" rejected", Style::default().fg(t.dim)),
        ]),
    ];

    let mut recent: Vec<(&Decision, bool)> = inbox
        .approved
        .iter()
        .map(|d| (d, true))
        .chain(inbox.rejected.iter().map(|d| (d, false)))
        .collect();
    recent.sort_by_key(|r| std::cmp::Reverse(r.0.at));
    let room = (inner.height as usize).saturating_sub(lines.len() + 4);
    if !recent.is_empty() && room > 0 {
        lines.push(Line::default());
        lines.push(Line::from(Span::styled(
            "Recent decisions",
            Style::default().fg(t.accent).bold(),
        )));
        let recent: Vec<_> = recent.into_iter().take(room.min(5)).collect();
        // Equal-width lines keep the centered columns aligned.
        let name_w = recent
            .iter()
            .map(|(d, _)| d.name.chars().count())
            .max()
            .unwrap_or(0);
        let mgr_w = recent
            .iter()
            .map(|(d, _)| manager_label(&d.manager).chars().count())
            .max()
            .unwrap_or(0);
        let ages: Vec<String> = recent.iter().map(|(d, _)| relative_time(d.at)).collect();
        let age_w = ages.iter().map(|a| a.chars().count()).max().unwrap_or(0);
        for ((d, approved), age) in recent.into_iter().zip(ages) {
            let (icon, color) = if approved {
                ("✓", t.ok)
            } else {
                ("✗", t.error)
            };
            lines.push(Line::from(vec![
                Span::styled(format!("{} ", icon), Style::default().fg(color)),
                Span::styled(format!("{:<name_w$}", d.name), Style::default().fg(t.text)),
                Span::styled(
                    format!("  {:<mgr_w$}  {:>age_w$}", manager_label(&d.manager), age),
                    Style::default().fg(t.dim),
                ),
            ]));
        }
    }

    let h = (lines.len() as u16).min(inner.height);
    let y = inner.y + (inner.height - h) / 3;
    f.render_widget(
        Paragraph::new(lines).alignment(Alignment::Center),
        Rect::new(inner.x, y, inner.width, h),
    );
}

/// Pending count and whether any is malicious, for the header badge.
pub fn pending(app: &App) -> (usize, bool) {
    let items = &app.state.inbox.items;
    (items.len(), items.iter().any(is_malicious))
}

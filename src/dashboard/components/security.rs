//! Security tab: the approval inbox (synced packages and machine keys that wait for this
//! machine's consent) and the machines whose signing keys it trusts.

use super::confirm::Confirm;
use super::{
    clamp_cursor, cursor_down, manager_label, panel, row, scroll_for, scrollbar, select_row,
    truncate,
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
use std::cell::RefCell;

/// Rows per inbox item in the list: two lines and a gap.
const ITEM_H: u16 = 3;
/// Below this body width the detail pane opens under the list on Enter.
const SPLIT_MIN_W: u16 = 110;
/// Width of the detail pane's label column; values wrap under the column after it.
const LABEL_W: usize = 12;

#[derive(Default)]
pub struct SecurityTabState {
    /// Index into `state.inbox.items`.
    pub cursor: usize,
    /// Id of the selected item. A reload keeps the selection on this id, not on the index.
    /// None after the selected item left the inbox: keys that decide wait until the next
    /// draw has shown which item the cursor is on.
    pub selected: RefCell<Option<String>>,
    /// The item at the cursor as the last draw showed it. A reload can replace an item
    /// under the same id, so keys that decide act on this copy, and only while it still
    /// matches the inbox.
    pub shown: RefCell<Option<InboxItem>>,
    /// Narrow layouts show the detail pane only after Enter.
    pub detail: bool,
}

pub fn handle_key(app: &mut App, key: KeyEvent) -> KeyOutcome {
    let len = app.state.inbox.items.len();
    let cmd = match key.code {
        KeyCode::Char('j') | KeyCode::Down => {
            let mut cursor = app.security.cursor;
            cursor_down(&mut cursor, len);
            move_cursor(app, cursor);
            None
        }
        KeyCode::Char('k') | KeyCode::Up => {
            move_cursor(app, app.security.cursor.saturating_sub(1));
            None
        }
        KeyCode::Enter => {
            app.security.detail = !app.security.detail;
            None
        }
        KeyCode::Esc => {
            app.security.detail = false;
            None
        }
        KeyCode::Char('a') => selected(app).and_then(|item| {
            if item.signature_failed() {
                app.overlays
                    .push(Overlay::Confirm(Confirm::approve_signature_failed(item)));
                None
            } else if let Kind::TrustMachine { fingerprint, .. } = &item.kind {
                app.overlays.push(Overlay::Confirm(Confirm::Trust {
                    ignored: super::machines::ignored_record(app, &item.name),
                    machine_id: item.name.clone(),
                    label: machine_name(app, &item.name),
                    fingerprint: fingerprint.clone(),
                    changed: item.reasons.contains(&Reason::KeyChanged),
                    item: Some(Box::new(item)),
                    arming: Default::default(),
                }));
                None
            } else {
                approve(app, item)
            }
        }),
        KeyCode::Char('x') => {
            if let Some(item) = selected(app) {
                app.overlays.push(Overlay::Confirm(Confirm::reject(item)));
            }
            None
        }
        KeyCode::Char('A') => {
            confirm_approve_all(app);
            None
        }
        KeyCode::Char('M') => {
            confirm_approve_machine(app);
            None
        }
        _ => return KeyOutcome::Ignored,
    };
    KeyOutcome::Handled(cmd)
}

/// The item the user has selected, as displayed.
fn selected(app: &mut App) -> Option<InboxItem> {
    let current = app.state.inbox.items.get(app.security.cursor)?;
    let id = current.id();
    let shown = app.security.shown.borrow().clone()?;
    if app.security.selected.borrow().as_deref() != Some(id.as_str()) || shown.id() != id {
        return None;
    }
    if shown != *current {
        app.flash_error(format!("{} changed. Review it again", id));
        return None;
    }
    Some(shown)
}

pub fn move_cursor(app: &mut App, index: usize) {
    app.security.cursor = index;
    *app.security.selected.borrow_mut() = app.state.inbox.items.get(index).map(InboxItem::id);
}

/// Show an item with its details, for the palette. The palette never decides: the user
/// sees the fingerprint or warning here first.
pub fn open(app: &mut App, id: &str) {
    if let Some(i) = app.state.inbox.items.iter().position(|i| i.id() == id) {
        move_cursor(app, i);
        app.security.detail = true;
    }
}

/// After the inbox changed, put the cursor back on the selected item.
pub fn reselect(app: &mut App) {
    let items = &app.state.inbox.items;
    let found = app
        .security
        .selected
        .borrow()
        .as_ref()
        .and_then(|id| items.iter().position(|i| i.id() == *id));
    match found {
        Some(i) => app.security.cursor = i,
        None => {
            clamp_cursor(&mut app.security.cursor, items.len());
            *app.security.selected.borrow_mut() = None;
        }
    }
}

pub fn reload(app: &mut App) {
    app.state.inbox = Inbox::load().unwrap_or_default();
    inbox::sort_by_group(&mut app.state.inbox.items);
    app.state.trusted = inbox::trusted_machines().unwrap_or_default();
    reselect(app);
}

/// Approve the item as displayed in the background: install a package, or trust a key.
pub fn approve(app: &mut App, item: InboxItem) -> Option<Cmd> {
    match &item.kind {
        Kind::Package => {
            if app.install_busy() {
                return None;
            }
            Some(app.start_inbox_install(item.name.clone(), vec![item], true))
        }
        Kind::TrustMachine { .. } => {
            let label = machine_name(app, &item.name);
            Some(Cmd::TrustKey {
                item: Box::new(item),
                label,
            })
        }
    }
}

/// Machine keys are left out: each needs its fingerprint checked on its own. So are
/// packages whose source record fails its signature.
/// The confirm keeps the items it shows, so items that arrive while it is open wait.
pub fn confirm_approve_all(app: &mut App) {
    let packages = app
        .state
        .inbox
        .items
        .iter()
        .filter(|i| i.kind == Kind::Package);
    let held = packages.clone().filter(|i| !i.bulk_approvable()).count();
    let items: Vec<InboxItem> = packages.filter(|i| i.bulk_approvable()).cloned().collect();
    if items.is_empty() {
        app.flash_info("Nothing to approve");
    } else if !app.install_busy() {
        app.overlays
            .push(Overlay::Confirm(Confirm::approve_all(items, held, None)));
    }
}

/// Approve all from the selected item's machine, as approve all does for the whole inbox.
pub fn confirm_approve_machine(app: &mut App) {
    let Some(machine) = app
        .state
        .inbox
        .items
        .get(app.security.cursor)
        .and_then(|i| i.from_machine().map(str::to_string))
    else {
        app.flash_info("This item comes from no machine record");
        return;
    };
    let (items, held) = inbox::approvable_from(&app.state.inbox.items, &machine);
    if items.is_empty() {
        app.flash_info(format!("Nothing to approve from {}", machine));
    } else if !app.install_busy() {
        let label = machine_name(app, &machine);
        app.overlays.push(Overlay::Confirm(Confirm::approve_all(
            items,
            held,
            Some(label),
        )));
    }
}

/// Approve and install exactly the items the confirm showed, in one background run.
/// Without `osv_required`, the user has agreed to install them without an OSV answer.
pub fn approve_all(app: &mut App, items: Vec<InboxItem>, osv_required: bool) -> Option<Cmd> {
    if app.install_busy() {
        return None;
    }
    let label = match items.as_slice() {
        [] => return None,
        [one] => one.name.clone(),
        many => format!("{} packages", many.len()),
    };
    Some(app.start_inbox_install(label, items, osv_required))
}

fn is_malicious(item: &InboxItem) -> bool {
    item.malicious()
}

fn reason_color(reason: Reason, t: &Theme) -> Color {
    match reason {
        Reason::Unsigned => t.warn,
        Reason::UntrustedTap => t.key,
        Reason::CooldownUnsupported | Reason::TooNew | Reason::PinnedTooNew => t.info,
        Reason::Malicious
        | Reason::MaliciousUpgrade
        | Reason::MaliciousUnresolved
        | Reason::KeyChanged
        | Reason::SignatureFailed => t.error,
        Reason::UntrustedSigner => t.key,
        Reason::NewMachine => t.info,
        Reason::OtherOsVersion => t.warn,
    }
}

fn explain(reason: Reason) -> &'static str {
    match reason {
        Reason::Unsigned => {
            "Another machine added this package. Tether cannot yet prove that machine made the change."
        }
        Reason::UntrustedTap => {
            "It comes from a Homebrew tap outside your trusted taps. Approval covers this package only, not the tap."
        }
        Reason::CooldownUnsupported => {
            "The installed package manager cannot enforce the minimum release age, and Tether could not check it in the registry, so a very new release could install."
        }
        Reason::TooNew => {
            "The installed package manager cannot enforce the minimum release age, and the registry shows this release is newer than it. It installs on its own once it is old enough."
        }
        Reason::Malicious => {
            "OSV reports this package as malicious. Tether will not install it, even with approval."
        }
        Reason::MaliciousUpgrade => {
            "OSV reports the version an upgrade would install as malicious. Tether keeps the installed version and will not install this one, even with approval. A later clean release upgrades as usual."
        }
        Reason::MaliciousUnresolved => {
            "OSV reports malicious releases of this package, and Tether could not find the release that would install. Approve only if you checked that release yourself."
        }
        Reason::UntrustedSigner => {
            "A valid signature covers the change, but this machine does not trust the key that made it."
        }
        Reason::NewMachine => {
            "This machine published a signing key for the first time. Trust it only if you set it up."
        }
        Reason::SignatureFailed => {
            "The machine this came from is trusted, but its record fails its signature. Someone may have edited the record or the manifest in the repo. Do not approve unless you know why the signature fails."
        }
        Reason::KeyChanged => {
            "SIGNING KEY CHANGED. This machine was trusted with a different key. If you did not set it up again, someone may be signing as it. Run 'tether machines show' on that machine and compare the fingerprint before you trust it."
        }
        Reason::OtherOsVersion => {
            "Only machines on another OS list the newest trusted version, and it failed to install here. This is the newest release that suits this machine. No trusted machine lists it, so it installs only when you approve it."
        }
        Reason::PinnedTooNew => {
            "A trusted machine lists a release that is newer than the minimum release age, so it did not install here. This is the newest release older than the limit. No trusted machine lists it, so it installs only when you approve it."
        }
    }
}

/// Soft pill: tinted background, colored text. Malicious gets a solid pill so it stands out.
pub(super) fn pill(text: &str, color: Color, t: &Theme) -> Span<'static> {
    let style = if color == t.error {
        Style::default().fg(t.brand_fg).bg(color).bold()
    } else if t.rgb {
        Style::default().fg(color).bg(mix(t.base_bg, color, 0.18))
    } else {
        Style::default().fg(color).reversed()
    };
    Span::styled(format!(" {} ", text), style)
}

pub(super) fn machine_name(app: &App, id: &str) -> String {
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

/// Relative age without the trailing " ago", for rows short on room.
fn short_age(at: chrono::DateTime<chrono::Utc>) -> String {
    let age = relative_time(at);
    age.strip_suffix(" ago").map(str::to_string).unwrap_or(age)
}

/// One line of the detail pane. Labeled values and advisories wrap at draw time,
/// so continuation lines indent under their value instead of under the label.
enum Detail {
    Text(Line<'static>),
    /// Label and value; the value wraps under the value column.
    Kv(&'static str, Span<'static>),
    /// Two parts on one line when they fit, else the second indents on the next line.
    Pair(Span<'static>, Span<'static>),
}

impl From<Line<'static>> for Detail {
    fn from(line: Line<'static>) -> Self {
        Detail::Text(line)
    }
}

/// Split at spaces to fit `width` columns; a word longer than a line breaks inside it.
fn wrap_words(s: &str, width: usize) -> Vec<String> {
    let width = width.max(1);
    let mut lines = vec![String::new()];
    for word in s.split(' ') {
        let mut word: Vec<char> = word.chars().collect();
        let len = lines.last().map_or(0, |l| l.chars().count());
        if len > 0 && len + 1 + word.len() <= width {
            lines.last_mut().expect("never empty").push(' ');
        } else if len > 0 {
            lines.push(String::new());
        }
        while word.len() > width {
            let rest = word.split_off(width);
            lines.last_mut().expect("never empty").extend(word);
            lines.push(String::new());
            word = rest;
        }
        lines.last_mut().expect("never empty").extend(word);
    }
    lines
}

fn detail_lines(detail: Vec<Detail>, width: usize, t: &Theme) -> Vec<Line<'static>> {
    let mut out = Vec::new();
    for d in detail {
        match d {
            Detail::Text(line) => out.push(line),
            Detail::Kv(label, value) => {
                let label_w = LABEL_W.min(width / 2);
                for (i, part) in wrap_words(&value.content, width - label_w)
                    .into_iter()
                    .enumerate()
                {
                    let head = if i == 0 { label } else { "" };
                    out.push(Line::from(vec![
                        Span::styled(format!("{:<label_w$}", head), Style::default().fg(t.dim)),
                        Span::styled(part, value.style),
                    ]));
                }
            }
            Detail::Pair(a, b) => {
                if a.width() + 2 + b.width() <= width {
                    out.push(Line::from(vec![a, Span::raw("  "), b]));
                } else {
                    out.push(Line::from(a));
                    out.push(Line::from(vec![Span::raw("  "), b]));
                }
            }
        }
    }
    out
}

/// How one inbox item draws: its two list lines and its detail pane.
/// A new item kind adds a match arm here and nowhere else in the view.
struct ItemView {
    heading: String,
    title: Line<'static>,
    badges: Line<'static>,
    /// The second list line as (left, right), fullest first. The list takes the first
    /// that fits, so detail drops in a fixed order instead of overlapping.
    meta: Vec<(Line<'static>, Line<'static>)>,
    detail: Vec<Detail>,
    /// Label of the approve button; None when only rejection is possible.
    approve: Option<&'static str>,
}

fn item_view(app: &App, item: &InboxItem) -> ItemView {
    match &item.kind {
        Kind::Package => package_view(app, item),
        Kind::TrustMachine { fingerprint, .. } => machine_view(app, item, fingerprint),
    }
}

fn reason_lines(item: &InboxItem, t: &Theme) -> Vec<Detail> {
    let mut lines: Vec<Detail> = vec![Line::from(Span::styled(
        "Why it is held",
        Style::default().fg(t.accent).bold(),
    ))
    .into()];
    for reason in &item.reasons {
        let loud = matches!(reason, Reason::KeyChanged | Reason::SignatureFailed);
        lines.push(Line::from(pill(reason.label(), reason_color(*reason, t), t)).into());
        lines.push(
            Line::from(Span::styled(
                explain(*reason),
                if loud {
                    Style::default().fg(t.error).bold()
                } else {
                    Style::default().fg(t.muted)
                },
            ))
            .into(),
        );
    }
    lines
}

fn badges(item: &InboxItem, t: &Theme) -> Line<'static> {
    let mut spans = Vec::new();
    for reason in &item.reasons {
        spans.push(pill(reason.label(), reason_color(*reason, t), t));
        spans.push(Span::raw(" "));
    }
    spans.pop();
    Line::from(spans)
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
    let meta = |age: String| {
        Line::from(vec![
            Span::raw("  "),
            Span::styled(fingerprint.to_string(), Style::default().fg(t.hash)),
            Span::styled(" · ", Style::default().fg(t.border)),
            Span::styled(age, Style::default().fg(t.dim)),
        ])
    };
    let meta = vec![
        (
            meta(format!("held {}", relative_time(item.first_seen))),
            Line::default(),
        ),
        (meta(short_age(item.first_seen)), Line::default()),
    ];
    let text = |s: String| Span::styled(s, Style::default().fg(t.text));
    let mut detail = vec![Detail::Kv("Machine", text(name.clone()))];
    if name != item.name {
        detail.push(Detail::Kv("Machine ID", text(item.name.clone())));
    }
    detail.push(Detail::Kv(
        "Key",
        Span::styled(fingerprint.to_string(), Style::default().fg(t.hash).bold()),
    ));
    detail.push(Detail::Kv(
        "First seen",
        text(relative_time(item.first_seen)),
    ));
    detail.push(Line::default().into());
    detail.extend(reason_lines(item, t));
    detail.push(Line::default().into());
    detail.push(
        Line::from(Span::styled(
            "Trusting this key lets package changes signed by it install without approval.",
            Style::default().fg(t.dim),
        ))
        .into(),
    );
    ItemView {
        heading: name,
        title,
        badges: badges(item, t),
        meta,
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
    let meta = |commit: bool, age: String| {
        let mut meta = vec![Span::raw("  ")];
        if let Some(source) = &source {
            meta.push(Span::styled("from ", Style::default().fg(t.dim)));
            meta.push(Span::styled(source.clone(), Style::default().fg(t.muted)));
            meta.push(sep());
        }
        if let Some(c) = item.commit.as_deref().filter(|_| commit) {
            meta.push(Span::styled(
                short(c).to_string(),
                Style::default().fg(t.hash),
            ));
            meta.push(sep());
        }
        meta.push(Span::styled(age, Style::default().fg(t.dim)));
        Line::from(meta)
    };

    let advisory_style = |id: &str| {
        if crate::packages::osv::is_malicious(id) {
            Style::default().fg(t.error).bold()
        } else {
            Style::default().fg(t.warn)
        }
    };
    let advisories = |more: bool| {
        let mut spans = Vec::new();
        if let Some(first) = item.advisories.first() {
            spans.push(Span::styled(first.clone(), advisory_style(first)));
            if more && item.advisories.len() > 1 {
                spans.push(Span::styled(
                    format!(" +{}", item.advisories.len() - 1),
                    Style::default().fg(t.dim),
                ));
            }
        }
        Line::from(spans)
    };
    let held = format!("held {}", relative_time(item.first_seen));
    let age = short_age(item.first_seen);
    let meta = vec![
        (meta(true, held.clone()), advisories(true)),
        (meta(false, held), advisories(true)),
        (meta(false, age.clone()), advisories(true)),
        (meta(false, age), advisories(false)),
    ];

    let kv = Detail::Kv;
    let text = |s: String| Span::styled(s, Style::default().fg(t.text));
    let section = |s: String| -> Detail {
        Line::from(Span::styled(s, Style::default().fg(t.accent).bold())).into()
    };
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
    detail.push(Line::default().into());
    detail.extend(reason_lines(item, t));
    detail.push(Line::default().into());
    detail.push(section(format!("Advisories ({})", item.advisories.len())));
    if item.advisories.is_empty() {
        detail.push(
            Line::from(Span::styled(
                "OSV lists no advisories for this version",
                Style::default().fg(t.dim),
            ))
            .into(),
        );
    }
    for id in &item.advisories {
        let what = if crate::packages::osv::is_malicious(id) {
            "malicious, blocks install"
        } else {
            "vulnerability, does not block"
        };
        detail.push(Detail::Pair(
            Span::styled(id.clone(), advisory_style(id)),
            Span::styled(what, Style::default().fg(t.dim)),
        ));
        detail.push(
            Line::from(vec![
                Span::raw("  "),
                Span::styled(
                    format!("osv.dev/vulnerability/{}", id),
                    Style::default().fg(t.dim).underlined(),
                ),
            ])
            .into(),
        );
    }

    ItemView {
        heading: item.name.clone(),
        title: Line::from(title),
        badges: badges(item, t),
        meta,
        detail,
        approve: (!malicious).then_some("approve & install"),
    }
}

pub fn render(f: &mut Frame, area: Rect, app: &App) {
    let area = old_build_note(f, area, app);
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
    app.security
        .selected
        .borrow_mut()
        .get_or_insert_with(|| items[cursor].id());
    *app.security.shown.borrow_mut() = Some(items[cursor].clone());
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

/// One line naming the machines on 1.x above the tab, and the area left under it.
fn old_build_note(f: &mut Frame, area: Rect, app: &App) -> Rect {
    let ids = &app.state.old_builds;
    if ids.is_empty() || area.height < 10 {
        return area;
    }
    let text = format!(
        "{} {}",
        ids.join(", "),
        crate::sync::signing::OLD_BUILD_NOTE
    );
    let lines = wrap_words(&text, area.width.max(1) as usize);
    let [note, rest] =
        Layout::vertical([Constraint::Length(lines.len() as u16), Constraint::Min(0)]).areas(area);
    f.render_widget(
        Paragraph::new(
            lines
                .into_iter()
                .map(|l| Line::from(Span::styled(l, Style::default().fg(app.theme.warn))))
                .collect::<Vec<_>>(),
        ),
        note,
    );
    rest
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
    // Machine ids are unique where hostnames are not, so the id stays whole and the
    // fingerprint gives way.
    let rows: Vec<Line> = trusted
        .iter()
        .take(inner.height as usize)
        .map(|m| {
            let mut left = vec![
                Span::styled("✓ ", Style::default().fg(t.ok)),
                Span::styled(m.machine_id.clone(), Style::default().fg(t.text)),
            ];
            if m.machine_id == app.machine_id() {
                left.push(Span::styled("  this", Style::default().fg(t.accent)));
            }
            Line::from(left)
        })
        .collect();
    // Fingerprints start in one column, after the longest id.
    let name_w = rows.iter().map(Line::width).max().unwrap_or(0) + 2;
    let room = (inner.width as usize).saturating_sub(name_w);
    for (i, (m, left)) in trusted.iter().zip(rows).enumerate() {
        let y = inner.y + i as u16;
        row(
            f,
            Rect::new(inner.x, y, inner.width, 1),
            left,
            Line::default(),
        );
        if room >= 12 {
            f.render_widget(
                Span::styled(truncate(&m.fingerprint, room), Style::default().fg(t.hash)),
                Rect::new(inner.x + name_w as u16, y, room as u16, 1),
            );
        }
    }
}

fn group_heading(app: &App, group: &inbox::Group, open: bool) -> (Line<'static>, Line<'static>) {
    let t = &app.theme;
    let machine = group
        .machine
        .as_deref()
        .map_or("no machine record".to_string(), |m| machine_name(app, m));
    let n = group.items.len();
    let left = Line::from(vec![
        Span::styled(if open { "▾ " } else { "▸ " }, Style::default().fg(t.dim)),
        Span::styled(machine, Style::default().fg(t.text).bold()),
        Span::styled(
            format!("  {} item{}", n, if n == 1 { "" } else { "s" }),
            Style::default().fg(t.muted),
        ),
    ]);
    let mut right = Vec::new();
    for reason in &group.reasons {
        right.push(pill(reason.label(), reason_color(*reason, t), t));
        right.push(Span::raw(" "));
    }
    right.pop();
    (left, Line::from(right))
}

/// Every group shows as one heading line, and only the group at the cursor lists its items,
/// in the room the headings leave. So hundreds of packages from a new machine still show
/// every group. Returns the first item to show and how many fit.
fn item_window(group_len: usize, position: usize, groups: usize, height: u16) -> (usize, usize) {
    let room = (height as usize).saturating_sub(groups);
    let visible = (room / ITEM_H as usize).clamp(1, group_len.max(1));
    (scroll_for(position, visible), visible)
}

fn render_list(f: &mut Frame, area: Rect, app: &App, cursor: usize) {
    let t = &app.theme;
    let items = &app.state.inbox.items;
    let groups = inbox::groups(items);
    let malicious = items.iter().filter(|i| is_malicious(i)).count();
    let mut count = vec![Span::styled(
        format!(
            " {} pending in {} group{}",
            items.len(),
            groups.len(),
            if groups.len() == 1 { "" } else { "s" }
        ),
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
    let block = panel(" Inbox ", true, t).title_top(Line::from(count).right_aligned());
    let inner = block.inner(area);
    f.render_widget(block, area);

    let mut y = inner.y;
    for group in &groups {
        if y >= inner.bottom() {
            break;
        }
        let position = group.items.iter().position(|&i| i == cursor);
        let rect = Rect::new(inner.x, y, inner.width, 1);
        let (left, right) = group_heading(app, group, position.is_some());
        row(f, rect, left, right);
        app.add_hit(rect, Hit::Row(group.items[0]));
        y += 1;
        let Some(position) = position else {
            continue;
        };
        let (first, visible) = item_window(group.items.len(), position, groups.len(), inner.height);
        for &i in group.items.iter().skip(first).take(visible) {
            let h = (ITEM_H - 1).min(inner.bottom().saturating_sub(y));
            if h == 0 {
                break;
            }
            let rect = Rect::new(inner.x + 2, y, inner.width.saturating_sub(2), h);
            if i == cursor {
                for dy in 0..h {
                    select_row(f, Rect::new(rect.x, y + dy, rect.width, 1), t);
                }
            }
            let view = item_view(app, &items[i]);
            row(f, Rect { height: 1, ..rect }, view.title, view.badges);
            if h > 1 {
                let w = rect.width as usize;
                let mut meta = view.meta;
                let fits = meta
                    .iter()
                    .position(|(l, r)| l.width() + 2 + r.width() <= w)
                    .unwrap_or(meta.len() - 1);
                let (left, right) = meta.swap_remove(fits);
                row(f, Rect::new(rect.x, y + 1, rect.width, 1), left, right);
            }
            app.add_hit(rect, Hit::Row(i));
            y += ITEM_H;
        }
        scrollbar(f, area, group.items.len(), first, visible, t);
    }
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
    let lines = detail_lines(view.detail, body.width as usize, t);
    f.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), body);
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
    let block = panel(" Inbox ", true, t);
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
            "The Inbox is empty: nothing waits for approval.",
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

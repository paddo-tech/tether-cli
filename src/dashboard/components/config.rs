use super::confirm::Confirm;
use super::{clamp_cursor, cursor_down, list, panel, row, scroll_for, scrollbar, select_row};
use crate::dashboard::app::{App, Hit, Overlay};
use crate::dashboard::config_edit::{self, FieldKind};
use crate::dashboard::msg::KeyOutcome;
use crossterm::event::{KeyCode, KeyEvent};
use ratatui::{prelude::*, widgets::Paragraph};

#[derive(Default)]
pub struct ConfigTabState {
    /// Index into `config_edit::fields()`.
    pub selected: usize,
    pub editing: bool,
    pub edit_buf: String,
    pub list_edit: Option<ListEditState>,
}

pub struct ListEditState {
    field_key: &'static str,
    field_label: &'static str,
    is_dotfile: bool,
    items: Vec<String>,
    pub cursor: usize,
    pub adding: bool,
    add_buf: String,
}

impl ListEditState {
    fn new(field_key: &'static str, field_label: &'static str, is_dotfile: bool) -> Self {
        Self {
            field_key,
            field_label,
            is_dotfile,
            items: Vec::new(),
            cursor: 0,
            adding: false,
            add_buf: String::new(),
        }
    }
}

pub fn handle_key(app: &mut App, key: KeyEvent) -> KeyOutcome {
    if app.config.list_edit.is_some() {
        return list_edit_key(app, key);
    } else if app.config.editing {
        text_edit_key(app, key);
    } else {
        match key.code {
            KeyCode::Enter => activate_field(app),
            KeyCode::Char('j') | KeyCode::Down => {
                cursor_down(&mut app.config.selected, config_edit::fields().len());
            }
            KeyCode::Char('k') | KeyCode::Up => {
                app.config.selected = app.config.selected.saturating_sub(1);
            }
            _ => return KeyOutcome::Ignored,
        }
    }
    KeyOutcome::Handled(None)
}

/// Apply a config edit, or show why it was refused or did not save. A failed save would
/// leave the edit in memory only, so the config goes back to what it was.
fn edit_config(
    app: &mut App,
    edit: impl FnOnce(&mut crate::config::Config) -> config_edit::EditResult,
) -> bool {
    let Some(config) = app.state.config.as_mut() else {
        return false;
    };
    let before = config.clone();
    match edit(config) {
        Ok(()) => true,
        Err(e) => {
            *config = before;
            app.flash_error(e);
            false
        }
    }
}

/// Remove the list item the confirm showed, if the list still has it at `index`.
pub fn remove_list_item(app: &mut App, index: usize, item: &str) {
    let Some(le) = app.config.list_edit.as_ref() else {
        return;
    };
    if le.items.get(index).map(String::as_str) != Some(item) {
        app.flash_error(format!("{} moved. Select it again", item));
        return;
    }
    let (field_key, is_dotfile) = (le.field_key, le.is_dotfile);
    edit_config(app, |c| {
        if is_dotfile {
            config_edit::remove_dotfile(c, index)
        } else {
            config_edit::remove_list_item(c, field_key, index)
        }
    });
    refresh_list_edit(app);
    if let Some(ref mut le) = app.config.list_edit {
        clamp_cursor(&mut le.cursor, le.items.len());
    }
}

/// While an item is typed every key goes to it; otherwise unknown keys reach the global
/// keymap.
fn list_edit_key(app: &mut App, key: KeyEvent) -> KeyOutcome {
    let Some(le) = app.config.list_edit.as_mut() else {
        return KeyOutcome::Ignored;
    };
    if le.adding {
        match key.code {
            KeyCode::Esc => {
                le.adding = false;
                le.add_buf.clear();
            }
            KeyCode::Enter => {
                let buf = std::mem::take(&mut le.add_buf);
                let field_key = le.field_key;
                let is_dotfile = le.is_dotfile;
                let added = edit_config(app, |c| {
                    if is_dotfile {
                        config_edit::add_dotfile(c, &buf, true)
                    } else {
                        config_edit::add_list_item(c, field_key, &buf)
                    }
                });
                refresh_list_edit(app);
                // A refused value stays in the input, so the user can correct it
                if let (false, Some(le)) = (added, app.config.list_edit.as_mut()) {
                    le.adding = true;
                    le.add_buf = buf;
                }
            }
            KeyCode::Backspace => {
                le.add_buf.pop();
            }
            KeyCode::Char(c) => {
                le.add_buf.push(c);
            }
            _ => {}
        }
        return KeyOutcome::Handled(None);
    }

    match key.code {
        KeyCode::Esc => {
            app.config.list_edit = None;
        }
        KeyCode::Char('j') | KeyCode::Down => {
            cursor_down(&mut le.cursor, le.items.len());
        }
        KeyCode::Char('k') | KeyCode::Up => {
            le.cursor = le.cursor.saturating_sub(1);
        }
        KeyCode::Char('a') => {
            le.adding = true;
            le.add_buf.clear();
        }
        KeyCode::Char('x') | KeyCode::Delete => {
            if let Some(item) = le.items.get(le.cursor) {
                let confirm = Confirm::RemoveListItem {
                    list: le.field_label,
                    index: le.cursor,
                    item: item.clone(),
                    arming: Default::default(),
                };
                app.overlays.push(Overlay::Confirm(confirm));
            }
        }
        KeyCode::Char('t') if le.is_dotfile => {
            let cursor = le.cursor;
            edit_config(app, |c| config_edit::toggle_dotfile_create(c, cursor));
            refresh_list_edit(app);
        }
        _ => return KeyOutcome::Ignored,
    }
    KeyOutcome::Handled(None)
}

fn text_edit_key(app: &mut App, key: KeyEvent) {
    match key.code {
        KeyCode::Esc => {
            app.config.editing = false;
            app.config.edit_buf.clear();
        }
        KeyCode::Enter => {
            let idx = app.config.selected;
            let buf = std::mem::take(&mut app.config.edit_buf);
            // A refused value stays in the field, so the user can correct it
            if !edit_config(app, |c| config_edit::set_value(c, idx, &buf)) {
                app.config.edit_buf = buf;
                return;
            }
            app.config.editing = false;
        }
        KeyCode::Backspace => {
            app.config.edit_buf.pop();
        }
        KeyCode::Char(c) => {
            app.config.edit_buf.push(c);
        }
        _ => {}
    }
}

/// Enter on a field: toggle a bool, start a text edit, or open the list sub-view.
fn activate_field(app: &mut App) {
    let idx = app.config.selected;
    let Some(field) = config_edit::fields().get(idx) else {
        return;
    };
    match field.kind {
        FieldKind::Bool => {
            edit_config(app, |c| config_edit::toggle(c, idx));
        }
        FieldKind::Text => {
            if let Some(ref config) = app.state.config {
                app.config.edit_buf = config_edit::get_value(config, idx);
                app.config.editing = true;
            }
        }
        FieldKind::List => {
            if app.state.config.is_some() {
                app.config.list_edit = Some(ListEditState::new(field.key, field.label, false));
                refresh_list_edit(app);
            }
        }
        FieldKind::DotfileList => {
            if app.state.config.is_some() {
                app.config.list_edit = Some(ListEditState::new("dotfiles.files", "Dotfiles", true));
                refresh_list_edit(app);
            }
        }
    }
}

/// Refresh list items from current config, keeping the cursor and leaving add mode.
fn refresh_list_edit(app: &mut App) {
    let (Some(le), Some(config)) = (app.config.list_edit.as_mut(), app.state.config.as_ref())
    else {
        return;
    };
    le.items = if le.is_dotfile {
        config_edit::get_dotfile_items(config)
            .iter()
            .map(|(path, create)| {
                format!("{}  create: {}", path, if *create { "yes" } else { "no" })
            })
            .collect()
    } else {
        config_edit::get_list_items(config, le.field_key)
    };
    le.adding = false;
    le.add_buf.clear();
}

pub fn render(f: &mut Frame, area: Rect, app: &App) {
    let t = &app.theme;
    let selected = app.config.selected;
    let Some(config) = &app.state.config else {
        let block = panel(" Config ", true, t);
        let inner = block.inner(area);
        f.render_widget(block, area);
        f.render_widget(
            Paragraph::new(Span::styled("No config loaded", Style::default().fg(t.dim))),
            inner,
        );
        return;
    };

    if let Some(le) = &app.config.list_edit {
        render_list_edit(f, area, le, app);
        return;
    }

    let block = panel(" Config ", true, t);
    let inner = block.inner(area);
    f.render_widget(block, area);

    // Section headers interleave with fields; `None` marks a header row, and an empty
    // header is the blank row between sections.
    let mut rows: Vec<(Option<usize>, &str)> = Vec::new();
    let mut last_section = "";
    for (i, field) in config_edit::fields().iter().enumerate() {
        if field.section != last_section {
            if !rows.is_empty() {
                rows.push((None, ""));
            }
            rows.push((None, field.section));
            last_section = field.section;
        }
        rows.push((Some(i), field.label));
    }
    let selected_row = rows
        .iter()
        .position(|(i, _)| *i == Some(selected))
        .unwrap_or(0);
    let visible = inner.height as usize;
    let scroll = scroll_for(selected_row, visible);

    for (n, (field, label)) in rows.iter().enumerate().skip(scroll).take(visible) {
        let r = Rect::new(inner.x, inner.y + (n - scroll) as u16, inner.width, 1);
        let Some(idx) = *field else {
            if label.is_empty() {
                continue;
            }
            let rule = "─".repeat((inner.width as usize).saturating_sub(label.len() + 1));
            f.render_widget(
                Line::from(vec![
                    Span::styled(format!("{} ", label), Style::default().fg(t.accent).bold()),
                    Span::styled(rule, Style::default().fg(t.border)),
                ]),
                r,
            );
            continue;
        };
        let is_selected = idx == selected;
        if is_selected {
            select_row(f, r, t);
        }
        app.add_hit(r, Hit::Row(idx));
        let value = config_edit::get_value(config, idx);
        let label_style = if is_selected {
            Style::default().fg(t.text).bold()
        } else {
            Style::default().fg(t.text)
        };
        let left = Line::from(Span::styled(format!("  {}", label), label_style));
        let right = match config_edit::fields()[idx].kind {
            FieldKind::Bool => {
                if value == "true" {
                    Line::from(vec![
                        Span::styled("on ", Style::default().fg(t.ok)),
                        Span::styled("●━━", Style::default().fg(t.ok)),
                    ])
                } else {
                    Line::from(vec![
                        Span::styled("off ", Style::default().fg(t.dim)),
                        Span::styled("━━○", Style::default().fg(t.dim)),
                    ])
                }
            }
            FieldKind::Text if is_selected && app.config.editing => Line::from(vec![
                Span::styled(
                    app.config.edit_buf.as_str(),
                    Style::default().fg(t.value).bold(),
                ),
                Span::styled("▏", Style::default().fg(t.accent)),
            ]),
            FieldKind::Text => Line::from(Span::styled(value, Style::default().fg(t.value))),
            FieldKind::List | FieldKind::DotfileList => Line::from(vec![
                Span::styled(value, Style::default().fg(t.muted)),
                Span::styled("  ›", Style::default().fg(t.dim)),
            ]),
        };
        row(f, r, left, right);
    }
    scrollbar(f, area, rows.len(), scroll, visible, t);
}

fn render_list_edit(f: &mut Frame, area: Rect, le: &ListEditState, app: &App) {
    let t = &app.theme;
    let mut hints = vec![
        Span::styled(" esc", t.key_hint()),
        Span::styled(" back  ", Style::default().fg(t.muted)),
        Span::styled("a", t.key_hint()),
        Span::styled(" add  ", Style::default().fg(t.muted)),
        Span::styled("x", t.key_hint()),
        Span::styled(" remove ", Style::default().fg(t.muted)),
    ];
    if le.is_dotfile {
        hints.push(Span::styled(" t", t.key_hint()));
        hints.push(Span::styled(
            " toggle create ",
            Style::default().fg(t.muted),
        ));
    }
    let block = panel(
        format!(" Config › {} ({}) ", le.field_label, le.items.len()),
        true,
        t,
    )
    .title_bottom(Line::from(hints).right_aligned());
    let inner = block.inner(area);
    f.render_widget(block, area);
    if inner.height == 0 {
        return;
    }

    let list_area = if le.adding {
        let input = Rect::new(inner.x, inner.bottom() - 1, inner.width, 1);
        f.render_widget(
            Line::from(vec![
                Span::styled("+ ", Style::default().fg(t.ok).bold()),
                Span::styled(le.add_buf.as_str(), Style::default().fg(t.text)),
                Span::styled("▏", Style::default().fg(t.accent)),
            ]),
            input,
        );
        Rect {
            height: inner.height - 1,
            ..inner
        }
    } else {
        inner
    };

    list(
        f,
        app,
        area,
        list_area,
        &le.items,
        le.cursor,
        |f, r, item, selected| {
            let style = if selected {
                Style::default().fg(t.text).bold()
            } else {
                Style::default().fg(t.text)
            };
            f.render_widget(Line::from(Span::styled(item.as_str(), style)), r);
        },
    );
}

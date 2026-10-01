use super::cursor_down;
use crate::dashboard::app::App;
use crate::dashboard::config_edit::{self, FieldKind};
use crate::dashboard::msg::KeyOutcome;
use crate::dashboard::theme::Theme;
use crossterm::event::{KeyCode, KeyEvent};
use ratatui::{prelude::*, widgets::*};

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
    cursor: usize,
    adding: bool,
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
        list_edit_key(app, key);
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

/// Apply a config edit and flash on failure.
fn edit_config(app: &mut App, edit: impl FnOnce(&mut crate::config::Config) -> bool) {
    let ok = app.state.config.as_mut().map(edit).unwrap_or(false);
    if !ok {
        app.flash_error("save failed");
    }
}

fn list_edit_key(app: &mut App, key: KeyEvent) {
    let Some(le) = app.config.list_edit.as_mut() else {
        return;
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
                le.adding = false;
                edit_config(app, |c| {
                    if is_dotfile {
                        config_edit::add_dotfile(c, &buf, true)
                    } else {
                        config_edit::add_list_item(c, field_key, &buf)
                    }
                });
                refresh_list_edit(app);
            }
            KeyCode::Backspace => {
                le.add_buf.pop();
            }
            KeyCode::Char(c) => {
                le.add_buf.push(c);
            }
            _ => {}
        }
        return;
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
        KeyCode::Char('d') | KeyCode::Delete => {
            let cursor = le.cursor;
            let field_key = le.field_key;
            let is_dotfile = le.is_dotfile;
            edit_config(app, |c| {
                if is_dotfile {
                    config_edit::remove_dotfile(c, cursor)
                } else {
                    config_edit::remove_list_item(c, field_key, cursor)
                }
            });
            refresh_list_edit(app);
            if let Some(ref mut le) = app.config.list_edit {
                if le.cursor > 0 && le.cursor >= le.items.len() {
                    le.cursor = le.items.len().saturating_sub(1);
                }
            }
        }
        KeyCode::Char('t') if le.is_dotfile => {
            let cursor = le.cursor;
            edit_config(app, |c| config_edit::toggle_dotfile_create(c, cursor));
            refresh_list_edit(app);
        }
        _ => {}
    }
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
            edit_config(app, |c| config_edit::set_value(c, idx, &buf));
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
        FieldKind::Bool => edit_config(app, |c| config_edit::toggle(c, idx)),
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
    let editing = app.config.editing;
    let edit_buf = app.config.edit_buf.as_str();
    let Some(config) = &app.state.config else {
        let msg = Paragraph::new(Span::styled(
            "  No config loaded",
            Style::default().fg(t.muted),
        ))
        .block(
            Block::default()
                .title(" Config ")
                .borders(Borders::ALL)
                .border_style(Style::default().fg(t.border)),
        );
        f.render_widget(msg, area);
        return;
    };

    // If list sub-view is active, render that instead
    if let Some(le) = &app.config.list_edit {
        render_list_edit(f, area, le, t);
        return;
    }

    let fields = config_edit::fields();

    let inner = Block::default()
        .title(" Config ")
        .borders(Borders::ALL)
        .border_style(Style::default().fg(t.border));
    let inner_area = inner.inner(area);
    f.render_widget(inner, area);

    let visible_height = inner_area.height as usize;
    let mut rows: Vec<Row> = Vec::new();
    let mut field_row_map: Vec<Option<usize>> = Vec::new();
    let mut last_section = "";

    for (i, field) in fields.iter().enumerate() {
        if field.section != last_section {
            rows.push(Row {
                is_header: true,
                label: field.section.to_string(),
                value: String::new(),
                kind: FieldKind::Bool,
            });
            field_row_map.push(None);
            last_section = field.section;
        }
        let value = config_edit::get_value(config, i);
        rows.push(Row {
            is_header: false,
            label: field.label.to_string(),
            value,
            kind: field.kind,
        });
        field_row_map.push(Some(i));
    }

    let selected_row = field_row_map
        .iter()
        .position(|m| *m == Some(selected))
        .unwrap_or(0);

    let scroll = if selected_row >= visible_height {
        selected_row - visible_height + 1
    } else {
        0
    };

    for (y, (row_idx, row)) in
        (inner_area.y..inner_area.y + inner_area.height).zip(rows.iter().enumerate().skip(scroll))
    {
        let is_selected = field_row_map[row_idx] == Some(selected);

        if row.is_header {
            let span = Span::styled(
                format!("  {}", row.label),
                Style::default().fg(t.accent).bold(),
            );
            f.render_widget(
                Paragraph::new(Line::from(span)),
                Rect::new(inner_area.x, y, inner_area.width, 1),
            );
        } else {
            let (prefix, val_display) = match row.kind {
                FieldKind::Bool => {
                    let cb = if row.value == "true" { "[x]" } else { "[ ]" };
                    (format!("    {} ", cb), String::new())
                }
                FieldKind::Text => {
                    let val = if is_selected && editing {
                        format!("{}_ ", edit_buf)
                    } else {
                        row.value.clone()
                    };
                    ("       ".to_string(), val)
                }
                FieldKind::List | FieldKind::DotfileList => {
                    ("    >  ".to_string(), row.value.clone())
                }
            };

            let style = if is_selected {
                Style::default().fg(t.text).bg(t.selection)
            } else {
                Style::default().fg(t.text)
            };

            let line = Line::from(vec![
                Span::styled(prefix, style),
                Span::styled(&row.label, style),
                if !val_display.is_empty() {
                    Span::styled(format!("  {}", val_display), style.fg(t.value))
                } else {
                    Span::raw("")
                },
                Span::styled(" ".repeat(inner_area.width as usize), style),
            ]);
            f.render_widget(
                Paragraph::new(line),
                Rect::new(inner_area.x, y, inner_area.width, 1),
            );
        }
    }
}

fn render_list_edit(f: &mut Frame, area: Rect, le: &ListEditState, t: &Theme) {
    let title = format!(" {} ({}) ", le.field_label, le.items.len());

    let block = Block::default()
        .title(title)
        .borders(Borders::ALL)
        .border_style(Style::default().fg(t.border));
    let inner_area = block.inner(area);
    f.render_widget(block, area);

    if inner_area.height == 0 {
        return;
    }

    // Header line with keybindings
    let header = Line::from(vec![
        Span::styled("  Esc", t.key_hint()),
        Span::styled(" back  ", Style::default().fg(t.muted)),
        Span::styled("a", t.key_hint()),
        Span::styled(" add  ", Style::default().fg(t.muted)),
        Span::styled("d", t.key_hint()),
        Span::styled(" delete", Style::default().fg(t.muted)),
        if le.is_dotfile {
            Span::styled("  t", t.key_hint())
        } else {
            Span::raw("")
        },
        if le.is_dotfile {
            Span::styled(" toggle create", Style::default().fg(t.muted))
        } else {
            Span::raw("")
        },
    ]);
    f.render_widget(
        Paragraph::new(header),
        Rect::new(inner_area.x, inner_area.y, inner_area.width, 1),
    );

    // Separator
    if inner_area.height < 2 {
        return;
    }
    let sep = "─".repeat(inner_area.width as usize);
    f.render_widget(
        Paragraph::new(Span::styled(sep, Style::default().fg(t.muted))),
        Rect::new(inner_area.x, inner_area.y + 1, inner_area.width, 1),
    );

    let list_start_y = inner_area.y + 2;
    let list_height = (inner_area.height as usize).saturating_sub(2);

    // Add mode input at the bottom
    let (items_height, add_line) = if le.adding {
        (
            list_height.saturating_sub(1),
            Some(list_start_y + list_height.saturating_sub(1) as u16),
        )
    } else {
        (list_height, None)
    };

    // Scroll for items
    let scroll = if le.cursor >= items_height {
        le.cursor - items_height + 1
    } else {
        0
    };

    for (y, (i, item)) in (list_start_y..list_start_y + items_height as u16)
        .zip(le.items.iter().enumerate().skip(scroll))
    {
        let is_selected = i == le.cursor;
        let style = if is_selected {
            Style::default().fg(t.text).bg(t.selection)
        } else {
            Style::default().fg(t.text)
        };

        let marker = if is_selected { ">" } else { " " };
        let line = Line::from(vec![
            Span::styled(format!("  {} ", marker), style),
            Span::styled(item, style),
            Span::styled(" ".repeat(inner_area.width as usize), style),
        ]);
        f.render_widget(
            Paragraph::new(line),
            Rect::new(inner_area.x, y, inner_area.width, 1),
        );
    }

    // Render add input line
    if let Some(add_y) = add_line {
        let line = Line::from(vec![
            Span::styled("  + ", Style::default().fg(t.ok).bold()),
            Span::styled(&le.add_buf, Style::default().fg(t.text)),
            Span::styled("_", Style::default().fg(t.key)),
        ]);
        f.render_widget(
            Paragraph::new(line),
            Rect::new(inner_area.x, add_y, inner_area.width, 1),
        );
    }
}

struct Row {
    is_header: bool,
    label: String,
    value: String,
    kind: FieldKind,
}

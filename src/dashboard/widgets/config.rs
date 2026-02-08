use crate::config::Config;
use crate::dashboard::config_edit::{self, FieldKind};
use crate::dashboard::ListEditState;
use ratatui::{prelude::*, widgets::*};

pub fn render(
    f: &mut Frame,
    area: Rect,
    config: &Option<Config>,
    selected: usize,
    editing: bool,
    edit_buf: &str,
    list_edit: Option<&ListEditState>,
) {
    let Some(config) = config else {
        let msg = Paragraph::new(Span::styled(
            "  No config loaded",
            Style::default().fg(Color::DarkGray),
        ))
        .block(
            Block::default()
                .title(" Config ")
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::DarkGray)),
        );
        f.render_widget(msg, area);
        return;
    };

    // If list sub-view is active, render that instead
    if let Some(le) = list_edit {
        render_list_edit(f, area, le);
        return;
    }

    let fields = config_edit::fields();

    let inner = Block::default()
        .title(" Config ")
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::DarkGray));
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

    let mut y = inner_area.y;
    for (row_idx, row) in rows.iter().enumerate().skip(scroll) {
        if y >= inner_area.y + inner_area.height {
            break;
        }

        let is_selected = field_row_map[row_idx] == Some(selected);

        if row.is_header {
            let span = Span::styled(
                format!("  {}", row.label),
                Style::default().fg(Color::Cyan).bold(),
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
                Style::default().fg(Color::White).bg(Color::DarkGray)
            } else {
                Style::default().fg(Color::White)
            };

            let line = Line::from(vec![
                Span::styled(prefix, style),
                Span::styled(&row.label, style),
                if !val_display.is_empty() {
                    Span::styled(format!("  {}", val_display), style.fg(Color::Yellow))
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
        y += 1;
    }
}

fn render_list_edit(f: &mut Frame, area: Rect, le: &ListEditState) {
    let title = format!(" {} ({}) ", le.field_label, le.items.len());

    let block = Block::default()
        .title(title)
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::DarkGray));
    let inner_area = block.inner(area);
    f.render_widget(block, area);

    if inner_area.height == 0 {
        return;
    }

    // Header line with keybindings
    let header = Line::from(vec![
        Span::styled("  Esc", Style::default().fg(Color::Yellow).bold()),
        Span::styled(" back  ", Style::default().fg(Color::DarkGray)),
        Span::styled("a", Style::default().fg(Color::Yellow).bold()),
        Span::styled(" add  ", Style::default().fg(Color::DarkGray)),
        Span::styled("d", Style::default().fg(Color::Yellow).bold()),
        Span::styled(" delete", Style::default().fg(Color::DarkGray)),
        if le.is_dotfile {
            Span::styled("  t", Style::default().fg(Color::Yellow).bold())
        } else {
            Span::raw("")
        },
        if le.is_dotfile {
            Span::styled(" toggle create", Style::default().fg(Color::DarkGray))
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
        Paragraph::new(Span::styled(sep, Style::default().fg(Color::DarkGray))),
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

    let mut y = list_start_y;
    for (i, item) in le.items.iter().enumerate().skip(scroll) {
        if y >= list_start_y + items_height as u16 {
            break;
        }

        let is_selected = i == le.cursor;
        let style = if is_selected {
            Style::default().fg(Color::White).bg(Color::DarkGray)
        } else {
            Style::default().fg(Color::White)
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
        y += 1;
    }

    // Render add input line
    if let Some(add_y) = add_line {
        let line = Line::from(vec![
            Span::styled("  + ", Style::default().fg(Color::Green).bold()),
            Span::styled(&le.add_buf, Style::default().fg(Color::White)),
            Span::styled("_", Style::default().fg(Color::Yellow)),
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

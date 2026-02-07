use crate::config::Config;
use crate::dashboard::config_edit::{self, FieldKind};
use ratatui::{prelude::*, widgets::*};

pub fn render(
    f: &mut Frame,
    area: Rect,
    config: &Option<Config>,
    selected: usize,
    editing: bool,
    edit_buf: &str,
) {
    let fields = config_edit::fields();

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

    let inner = Block::default()
        .title(" Config ")
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::DarkGray));
    let inner_area = inner.inner(area);
    f.render_widget(inner, area);

    let visible_height = inner_area.height as usize;
    // Build display rows: section headers + fields
    let mut rows: Vec<Row> = Vec::new();
    let mut last_section = "";
    let mut field_row_map: Vec<Option<usize>> = Vec::new(); // row index -> field index (None = header)

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

    // Find the row index for the selected field
    let selected_row = field_row_map
        .iter()
        .position(|m| *m == Some(selected))
        .unwrap_or(0);

    // Scroll so selected row is visible
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
            let checkbox = match row.kind {
                FieldKind::Bool => {
                    if row.value == "true" {
                        "[x]"
                    } else {
                        "[ ]"
                    }
                }
                FieldKind::Text => "   ",
            };

            let val_display = if is_selected && editing {
                format!("{}_ ", edit_buf)
            } else {
                match row.kind {
                    FieldKind::Bool => String::new(),
                    FieldKind::Text => row.value.clone(),
                }
            };

            let style = if is_selected {
                Style::default().fg(Color::White).bg(Color::DarkGray)
            } else {
                Style::default().fg(Color::White)
            };

            let line = Line::from(vec![
                Span::styled(format!("    {} ", checkbox), style),
                Span::styled(&row.label, style),
                if !val_display.is_empty() {
                    Span::styled(format!("  {}", val_display), style.fg(Color::Yellow))
                } else {
                    Span::raw("")
                },
                // Pad to fill the row background
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

struct Row {
    is_header: bool,
    label: String,
    value: String,
    kind: FieldKind,
}

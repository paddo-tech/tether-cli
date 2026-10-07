use super::{panel, row};
use crate::dashboard::theme::Theme;
use ratatui::{prelude::*, widgets::Paragraph};

/// Runs of equal lines as (line, count), in order.
fn collapse(lines: Vec<String>) -> Vec<(String, usize)> {
    let mut runs: Vec<(String, usize)> = Vec::new();
    for line in lines {
        match runs.last_mut() {
            Some((last, n)) if *last == line => *n += 1,
            _ => runs.push((line, 1)),
        }
    }
    runs
}

/// Daemon log tail, newest at the bottom, colored by the glyph or level each line carries.
/// A line the daemon repeats shows once, with its count at the right.
pub fn render(f: &mut Frame, area: Rect, lines: &[String], t: &Theme) {
    let block = panel(" Daemon log ", false, t);
    let inner = block.inner(area);
    f.render_widget(block, area);
    let clean: Vec<String> = lines
        .iter()
        .map(|l| strip_ansi(l))
        .filter(|l| !l.trim().is_empty())
        .collect();
    if clean.is_empty() {
        f.render_widget(
            Paragraph::new(Span::styled("No activity", Style::default().fg(t.dim))),
            inner,
        );
        return;
    }
    let runs = collapse(clean);
    let start = runs.len().saturating_sub(inner.height as usize);
    for (i, (l, n)) in runs[start..].iter().enumerate() {
        let color = line_color(l, t);
        let count = if *n > 1 {
            Line::from(Span::styled(format!("×{}", n), Style::default().fg(t.dim)))
        } else {
            Line::default()
        };
        row(
            f,
            Rect::new(inner.x, inner.y + i as u16, inner.width, 1),
            Line::from(Span::styled(l.as_str(), Style::default().fg(color))),
            count,
        );
    }
}

/// The color of a log line, by the glyph or level it carries.
pub fn line_color(line: &str, t: &Theme) -> Color {
    let s = line.trim_start();
    if s.starts_with('✗') || s.contains(" ERROR ") || s.starts_with("ERROR") {
        t.error
    } else if s.starts_with('⚠') || s.contains(" WARN ") || s.starts_with("Warning") {
        t.warn
    } else if s.starts_with('✓') {
        t.ok
    } else if s.starts_with('ℹ') {
        t.info
    } else {
        t.muted
    }
}

/// Drop ANSI escape sequences that the CLI writes into the log.
pub fn strip_ansi(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\x1b' {
            if chars.peek() == Some(&'[') {
                chars.next();
                for n in chars.by_ref() {
                    if n.is_ascii_alphabetic() {
                        break;
                    }
                }
            }
        } else if c != '\t' && !c.is_control() {
            out.push(c);
        } else if c == '\t' {
            out.push_str("  ");
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn collapses_repeated_lines() {
        let lines = ["a", "b", "b", "b", "a"].map(String::from).to_vec();
        assert_eq!(
            collapse(lines),
            vec![("a".into(), 1), ("b".into(), 3), ("a".into(), 1)]
        );
    }

    #[test]
    fn strips_sgr_sequences() {
        assert_eq!(
            strip_ansi("\x1b[1m\x1b[33m⚠\x1b[39m\x1b[0m \x1b[33m  .zshrc\x1b[39m"),
            "⚠   .zshrc"
        );
        assert_eq!(strip_ansi("plain\ttext"), "plain  text");
    }
}

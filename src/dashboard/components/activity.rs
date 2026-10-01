use super::panel;
use crate::dashboard::theme::Theme;
use ratatui::{prelude::*, widgets::Paragraph};

/// Daemon log tail, newest at the bottom, colored by the glyph or level each line carries.
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
    let start = clean.len().saturating_sub(inner.height as usize);
    let text: Vec<Line> = clean[start..]
        .iter()
        .map(|l| {
            let s = l.trim_start();
            let color = if s.starts_with('✗') || s.contains(" ERROR ") || s.starts_with("ERROR") {
                t.error
            } else if s.starts_with('⚠') || s.contains(" WARN ") || s.starts_with("Warning") {
                t.warn
            } else if s.starts_with('✓') {
                t.ok
            } else if s.starts_with('ℹ') {
                t.info
            } else {
                t.muted
            };
            Line::from(Span::styled(l.clone(), Style::default().fg(color)))
        })
        .collect();
    f.render_widget(Paragraph::new(text), inner);
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
    fn strips_sgr_sequences() {
        assert_eq!(
            strip_ansi("\x1b[1m\x1b[33m⚠\x1b[39m\x1b[0m \x1b[33m  .zshrc\x1b[39m"),
            "⚠   .zshrc"
        );
        assert_eq!(strip_ansi("plain\ttext"), "plain  text");
    }
}

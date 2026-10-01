//! Unified diff lines with line numbers and syntax colors.

use crate::dashboard::theme::Theme;
use ratatui::prelude::*;

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum DiffKind {
    FileHeader,
    Hunk,
    Add,
    Del,
    Context,
}

#[derive(Clone, PartialEq, Debug)]
pub struct DiffLine {
    pub kind: DiffKind,
    pub old: Option<usize>,
    pub new: Option<usize>,
    pub text: String,
}

/// Classify each line of a unified diff and number it from the hunk headers.
pub fn annotate(lines: &[String]) -> Vec<DiffLine> {
    let (mut old, mut new) = (0usize, 0usize);
    lines
        .iter()
        .map(|line| {
            let (kind, o, n) = if line.starts_with("@@") {
                if let Some((o, n)) = hunk_starts(line) {
                    old = o;
                    new = n;
                }
                (DiffKind::Hunk, None, None)
            } else if line.starts_with("+++") || line.starts_with("---") {
                (DiffKind::FileHeader, None, None)
            } else if let Some(rest) = line.strip_prefix('+') {
                new += 1;
                return DiffLine {
                    kind: DiffKind::Add,
                    old: None,
                    new: Some(new - 1),
                    text: rest.to_string(),
                };
            } else if let Some(rest) = line.strip_prefix('-') {
                old += 1;
                return DiffLine {
                    kind: DiffKind::Del,
                    old: Some(old - 1),
                    new: None,
                    text: rest.to_string(),
                };
            } else {
                old += 1;
                new += 1;
                return DiffLine {
                    kind: DiffKind::Context,
                    old: Some(old - 1),
                    new: Some(new - 1),
                    text: line.strip_prefix(' ').unwrap_or(line).to_string(),
                };
            };
            DiffLine {
                kind,
                old: o,
                new: n,
                text: line.clone(),
            }
        })
        .collect()
}

/// `@@ -12,3 +14,5 @@` gives the first old and new line numbers: (12, 14).
fn hunk_starts(header: &str) -> Option<(usize, usize)> {
    let mut parts = header.split_whitespace().skip(1);
    let num = |p: &str| p[1..].split(',').next()?.parse::<usize>().ok();
    let old = num(parts.next()?)?;
    let new = num(parts.next()?)?;
    Some((old, new))
}

/// Draw one diff line: `old new │ ± text`, with a tinted background for changes.
pub fn render_line(f: &mut Frame, area: Rect, line: &DiffLine, selected: bool, t: &Theme) {
    let num = |n: Option<usize>| {
        n.map(|n| format!("{:>4}", n))
            .unwrap_or_else(|| "    ".into())
    };
    let (sign, fg, bg) = match line.kind {
        DiffKind::Add => ("+", t.ok, Some(t.diff_add_bg)),
        DiffKind::Del => ("-", t.error, Some(t.diff_del_bg)),
        DiffKind::Hunk => (" ", t.info, None),
        DiffKind::FileHeader => (" ", t.dim, None),
        DiffKind::Context => (" ", t.muted, None),
    };
    if let (Some(bg), false) = (bg, selected) {
        f.buffer_mut().set_style(area, Style::default().bg(bg));
    }
    let mut text_style = Style::default().fg(fg);
    match line.kind {
        DiffKind::Hunk => text_style = text_style.italic(),
        DiffKind::FileHeader => text_style = text_style.bold(),
        _ => {}
    }
    let spans = vec![
        Span::raw("    "),
        Span::styled(num(line.old), Style::default().fg(t.dim)),
        Span::styled(num(line.new), Style::default().fg(t.dim)),
        Span::styled(" │ ", Style::default().fg(t.border)),
        Span::styled(format!("{} ", sign), Style::default().fg(fg).bold()),
        Span::styled(line.text.as_str(), text_style),
    ];
    f.render_widget(Line::from(spans), area);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lines(s: &str) -> Vec<String> {
        s.lines().map(str::to_string).collect()
    }

    #[test]
    fn numbers_lines_from_hunk_headers() {
        let diff = annotate(&lines(
            "--- a/x\n+++ b/x\n@@ -3,3 +3,4 @@\n keep\n-old\n+new\n+more\n tail",
        ));
        let kinds: Vec<_> = diff.iter().map(|l| l.kind).collect();
        assert_eq!(
            kinds,
            vec![
                DiffKind::FileHeader,
                DiffKind::FileHeader,
                DiffKind::Hunk,
                DiffKind::Context,
                DiffKind::Del,
                DiffKind::Add,
                DiffKind::Add,
                DiffKind::Context,
            ]
        );
        let nums: Vec<_> = diff.iter().map(|l| (l.old, l.new)).collect();
        assert_eq!(nums[3], (Some(3), Some(3)));
        assert_eq!(nums[4], (Some(4), None));
        assert_eq!(nums[5], (None, Some(4)));
        assert_eq!(nums[6], (None, Some(5)));
        assert_eq!(nums[7], (Some(5), Some(6)));
        assert_eq!(diff[4].text, "old");
    }

    #[test]
    fn parses_hunk_without_counts() {
        assert_eq!(hunk_starts("@@ -1 +1 @@"), Some((1, 1)));
        assert_eq!(hunk_starts("@@ -0,0 +1,2 @@ ctx"), Some((0, 1)));
        assert_eq!(hunk_starts("@@ junk"), None);
    }
}

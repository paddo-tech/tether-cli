//! Multi-row bar sparkline with a height gradient.

use crate::dashboard::theme::{mix, Theme};
use ratatui::prelude::*;

const EIGHTHS: [&str; 9] = [" ", "▁", "▂", "▃", "▄", "▅", "▆", "▇", "█"];

/// Draw `values` as bars, one column each with a one-column gap, right-aligned so
/// the newest value sits at the right edge. `highlight_last` recolors the newest bar.
pub fn render(f: &mut Frame, area: Rect, values: &[u64], highlight_last: Option<Color>, t: &Theme) {
    if area.height == 0 || area.width == 0 {
        return;
    }
    let max = values.iter().copied().max().unwrap_or(0).max(1);
    let rows = area.height as u64;
    let shown = values.len().min(area.width.div_ceil(2) as usize);
    let start = values.len() - shown;
    let buf = f.buffer_mut();
    for (i, &v) in values[start..].iter().enumerate() {
        let x = area.right() as i32 - 1 - 2 * (shown - 1 - i) as i32;
        if x < area.x as i32 {
            continue;
        }
        let x = x as u16;
        let frac = v as f32 / max as f32;
        let mut color = if t.rgb {
            mix(t.info, t.accent, frac)
        } else if frac > 0.5 {
            t.accent
        } else {
            t.info
        };
        if i == shown - 1 {
            color = highlight_last.unwrap_or(color);
        }
        // Scale to eighths of a cell; any non-zero value shows at least one eighth.
        let eighths = if v == 0 {
            0
        } else {
            (v * rows * 8 / max).max(1)
        };
        for r in 0..rows {
            let y = area.bottom() - 1 - r as u16;
            let fill = eighths.saturating_sub(r * 8).min(8) as usize;
            let cell = &mut buf[(x, y)];
            if fill == 0 && r == 0 {
                cell.set_symbol("·").set_fg(t.dim);
            } else {
                cell.set_symbol(EIGHTHS[fill]).set_fg(color);
            }
        }
    }
}

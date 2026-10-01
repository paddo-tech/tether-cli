pub mod activity;
pub mod config;
pub mod confirm;
pub mod file_import;
pub mod files;
pub mod header;
pub mod help;
pub mod machines;
pub mod overview;
pub mod packages;
pub mod pkg_import;
pub mod profile_picker;
pub mod tabs;

use ratatui::layout::Rect;

/// Display label for a package manager key
pub fn manager_label(key: &str) -> &str {
    match key {
        "brew_formulae" => "Brew formulae",
        "brew_casks" => "Brew casks",
        "brew_taps" => "Brew taps",
        "npm" => "npm",
        "pnpm" => "pnpm",
        "bun" => "Bun",
        "gem" => "Gem",
        "uv" => "uv",
        _ => key,
    }
}

/// Centered popup area clamped to the frame.
pub fn centered(area: Rect, width: u16, height: u16) -> Rect {
    let x = (area.width.saturating_sub(width)) / 2;
    let y = (area.height.saturating_sub(height)) / 2;
    Rect::new(x, y, width, height)
}

/// Move a list cursor down by one, stopping at the last row.
pub fn cursor_down(cursor: &mut usize, len: usize) {
    if *cursor < len.saturating_sub(1) {
        *cursor += 1;
    }
}

/// Clamp a list cursor after the row count shrinks.
pub fn clamp_cursor(cursor: &mut usize, len: usize) {
    if *cursor >= len {
        *cursor = len.saturating_sub(1);
    }
}

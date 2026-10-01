use ratatui::style::{Color, Style};

/// Every color the dashboard draws with. Components read colors from here, never from `Color`.
pub struct Theme {
    pub text: Color,
    pub muted: Color,
    pub border: Color,
    pub accent: Color,
    pub key: Color,
    pub hash: Color,
    pub value: Color,
    pub ok: Color,
    pub warn: Color,
    pub error: Color,
    pub team: Color,
    pub selection: Color,
    pub base_bg: Color,
    pub brand_fg: Color,
    pub brand_bg: Color,
}

impl Theme {
    /// The 16-color ANSI palette, so the terminal's own color scheme applies.
    pub fn ansi() -> Self {
        Self {
            text: Color::White,
            muted: Color::Gray,
            border: Color::Gray,
            accent: Color::Cyan,
            key: Color::Yellow,
            hash: Color::Yellow,
            value: Color::Yellow,
            ok: Color::Green,
            warn: Color::Yellow,
            error: Color::Red,
            team: Color::Magenta,
            selection: Color::Indexed(240),
            base_bg: Color::Reset,
            brand_fg: Color::Black,
            brand_bg: Color::Cyan,
        }
    }

    /// Style for a key name in a shortcut hint.
    pub fn key_hint(&self) -> Style {
        Style::default().fg(self.key).bold()
    }

    /// Foreground for one line of unified diff output.
    pub fn diff_fg(&self, line: &str) -> Color {
        if line.starts_with("@@") {
            self.accent
        } else if line.starts_with("+++") || line.starts_with("---") {
            self.muted
        } else if line.starts_with('+') {
            self.ok
        } else if line.starts_with('-') {
            self.error
        } else {
            self.muted
        }
    }
}

use comfy_table::{presets, ContentArrangement, Table};
use std::fmt::Display;
use std::io::IsTerminal;
use std::sync::{Mutex, OnceLock};

pub struct Output;

static CAPTURED: Mutex<Option<Vec<String>>> = Mutex::new(None);

/// With `--json`, stdout carries only the JSON, so messages go to stderr.
static JSON: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Colour only when stdout is a terminal and `NO_COLOR` is unset or empty, so pipes, files
/// and `NO_COLOR` get plain text.
pub fn color_enabled() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| {
        colors_wanted(
            std::env::var_os("NO_COLOR").as_deref(),
            std::io::stdout().is_terminal(),
        )
    })
}

fn colors_wanted(no_color: Option<&std::ffi::OsStr>, terminal: bool) -> bool {
    no_color.is_none_or(|v| v.is_empty()) && terminal
}

/// The owo-colors methods the CLI uses, as plain text when [`color_enabled`] is false. CLI
/// code imports this trait instead of `owo_colors::OwoColorize`.
pub trait Colorize: Display {
    fn bold(&self) -> String {
        paint(self, |s| owo_colors::OwoColorize::bold(&s).to_string())
    }
    fn dimmed(&self) -> String {
        paint(self, |s| owo_colors::OwoColorize::dimmed(&s).to_string())
    }
    fn red(&self) -> String {
        paint(self, |s| owo_colors::OwoColorize::red(&s).to_string())
    }
    fn green(&self) -> String {
        paint(self, |s| owo_colors::OwoColorize::green(&s).to_string())
    }
    fn yellow(&self) -> String {
        paint(self, |s| owo_colors::OwoColorize::yellow(&s).to_string())
    }
    fn cyan(&self) -> String {
        paint(self, |s| owo_colors::OwoColorize::cyan(&s).to_string())
    }
    fn bright_black(&self) -> String {
        paint(self, |s| {
            owo_colors::OwoColorize::bright_black(&s).to_string()
        })
    }
    fn bright_blue(&self) -> String {
        paint(self, |s| {
            owo_colors::OwoColorize::bright_blue(&s).to_string()
        })
    }
    fn bright_cyan(&self) -> String {
        paint(self, |s| {
            owo_colors::OwoColorize::bright_cyan(&s).to_string()
        })
    }
    fn bright_white(&self) -> String {
        paint(self, |s| {
            owo_colors::OwoColorize::bright_white(&s).to_string()
        })
    }
}

impl<T: Display + ?Sized> Colorize for T {}

fn paint<T: Display + ?Sized>(value: &T, style: impl FnOnce(String) -> String) -> String {
    let text = value.to_string();
    if color_enabled() {
        style(text)
    } else {
        text
    }
}

// Icon constants
impl Output {
    pub const CHECK: &str = "✓";
    pub const CROSS: &str = "✗";
    pub const INFO: &str = "ℹ";
    pub const WARN: &str = "⚠";
    pub const ARROW: &str = "→";
    pub const DOT: &str = "●";
    pub const BULLET: &str = "•";
}

impl Output {
    /// Print JSON on stdout, and send every message to stderr from now on.
    pub fn json(value: &serde_json::Value) -> anyhow::Result<()> {
        println!("{}", serde_json::to_string_pretty(value)?);
        Ok(())
    }

    pub fn set_json(on: bool) {
        JSON.store(on, std::sync::atomic::Ordering::Relaxed);
    }

    fn line(text: String) {
        if JSON.load(std::sync::atomic::Ordering::Relaxed) {
            eprintln!("{}", text);
        } else {
            println!("{}", text);
        }
    }

    pub fn success(message: &str) {
        Self::line(format!("{} {}", Self::CHECK.green().bold(), message));
    }

    pub fn error(message: &str) {
        eprintln!("{} {}", Self::CROSS.red().bold(), message.red());
    }

    pub fn info(message: &str) {
        Self::line(format!("{} {}", Self::INFO.bright_blue().bold(), message));
    }

    /// While the dashboard owns the terminal, a warning goes to the log and waits for the
    /// dashboard to show it, because printing would draw over its screen.
    pub fn warning(message: &str) {
        if let Some(queue) = CAPTURED.lock().unwrap_or_else(|e| e.into_inner()).as_mut() {
            log::warn!("{}", message);
            queue.push(message.to_string());
            return;
        }
        Self::line(format!(
            "{} {}",
            Self::WARN.yellow().bold(),
            message.yellow()
        ));
    }

    /// Hold warnings for the dashboard from now on, or print them again with `false`.
    pub fn capture_warnings(on: bool) {
        *CAPTURED.lock().unwrap_or_else(|e| e.into_inner()) = on.then(Vec::new);
    }

    /// Warnings held since the last call.
    pub fn take_warnings() -> Vec<String> {
        CAPTURED
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_mut()
            .map(std::mem::take)
            .unwrap_or_default()
    }

    pub fn header(message: &str) {
        println!("\n{}\n", message.bright_cyan().bold());
    }

    pub fn subheader(message: &str) {
        println!("{}", message.bright_white().bold());
    }

    pub fn step(step_num: usize, total: usize, message: &str) {
        println!(
            "{} {}",
            format!("[{}/{}]", step_num, total).bright_black(),
            message
        );
    }

    pub fn dim(message: &str) {
        println!("{}", message.bright_black());
    }

    pub fn section(title: &str) {
        println!();
        println!("{}", title.bright_cyan().bold());
    }

    pub fn list_item(text: &str) {
        println!("  {} {}", Self::BULLET.bright_black(), text);
    }

    pub fn status_line(label: &str, value: &str, good: bool) {
        if good {
            println!("  {} {} {}", Self::DOT.green(), label.bright_black(), value);
        } else {
            println!(
                "  {} {} {}",
                Self::DOT.yellow(),
                label.bright_black(),
                value
            );
        }
    }

    /// A table that styles its cells only when [`color_enabled`] is true.
    pub fn table() -> Table {
        let mut table = Table::new();
        if !color_enabled() {
            table.force_no_tty();
        }
        table
    }

    pub fn table_minimal() -> Table {
        let mut table = Self::table();
        table
            .load_preset(presets::UTF8_BORDERS_ONLY)
            .set_content_arrangement(ContentArrangement::Dynamic);
        table
    }

    pub fn table_full() -> Table {
        let mut table = Self::table();
        table
            .load_preset(presets::UTF8_FULL)
            .set_content_arrangement(ContentArrangement::Dynamic);
        table
    }

    pub fn key_value(key: &str, value: &str) {
        let padded = format!("{:14}", key);
        println!("  {}  {}", padded.bright_white().bold(), value);
    }

    pub fn key_value_colored(key: &str, value: &str, color_fn: impl Fn(&str) -> String) {
        let padded = format!("{:14}", key);
        println!("  {}  {}", padded.bright_white().bold(), color_fn(value));
    }

    pub fn divider() {
        println!(
            "  {}",
            "────────────────────────────────────────────".bright_black()
        );
    }

    pub fn badge(text: &str, good: bool) -> String {
        let badge = format!("[{}]", text);
        if good {
            badge.green().to_string()
        } else {
            badge.red().to_string()
        }
    }

    pub fn diff_line(symbol: &str, text: &str, kind: &str) {
        match kind {
            "added" => println!("  {} {}", symbol.green(), text),
            "removed" => println!("  {} {}", symbol.red(), text),
            _ => println!("  {} {}", symbol.yellow(), text),
        }
    }
}

pub fn relative_time(dt: chrono::DateTime<chrono::Utc>) -> String {
    let now = chrono::Utc::now();
    let duration = now.signed_duration_since(dt);

    let seconds = duration.num_seconds();
    if seconds < 60 {
        return "just now".to_string();
    }

    let minutes = duration.num_minutes();
    if minutes < 60 {
        return format!("{}m ago", minutes);
    }

    let hours = duration.num_hours();
    if hours < 24 {
        return format!("{}h ago", hours);
    }

    let days = duration.num_days();
    if days < 2 {
        return "yesterday".to_string();
    }
    if days < 7 {
        return format!("{}d ago", days);
    }

    dt.with_timezone(&chrono::Local)
        .format("%b %d %H:%M")
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsStr;

    #[test]
    fn colour_needs_a_terminal_and_no_no_color() {
        assert!(colors_wanted(None, true));
        assert!(colors_wanted(Some(OsStr::new("")), true));
        assert!(!colors_wanted(Some(OsStr::new("1")), true));
        assert!(!colors_wanted(None, false));
    }
}

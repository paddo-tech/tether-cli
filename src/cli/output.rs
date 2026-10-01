use comfy_table::{presets, ContentArrangement, Table};
use owo_colors::OwoColorize;
use std::sync::Mutex;

pub struct Output;

static CAPTURED: Mutex<Option<Vec<String>>> = Mutex::new(None);

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
    pub fn success(message: &str) {
        println!("{} {}", Self::CHECK.green().bold(), message);
    }

    pub fn error(message: &str) {
        eprintln!("{} {}", Self::CROSS.red().bold(), message.red());
    }

    pub fn info(message: &str) {
        println!("{} {}", Self::INFO.bright_blue().bold(), message);
    }

    /// While the dashboard owns the terminal, a warning goes to the log and waits for the
    /// dashboard to show it, because printing would draw over its screen.
    pub fn warning(message: &str) {
        if let Some(queue) = CAPTURED.lock().unwrap_or_else(|e| e.into_inner()).as_mut() {
            log::warn!("{}", message);
            queue.push(message.to_string());
            return;
        }
        println!("{} {}", Self::WARN.yellow().bold(), message.yellow());
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

    pub fn table_minimal() -> Table {
        let mut table = Table::new();
        table
            .load_preset(presets::UTF8_BORDERS_ONLY)
            .set_content_arrangement(ContentArrangement::Dynamic);
        table
    }

    pub fn table_full() -> Table {
        let mut table = Table::new();
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

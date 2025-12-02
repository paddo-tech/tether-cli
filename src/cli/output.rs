use comfy_table::{presets, ContentArrangement, Table};
use owo_colors::OwoColorize;

pub struct Output;

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

    pub fn warning(message: &str) {
        println!("{} {}", Self::WARN.yellow().bold(), message.yellow());
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
}

use owo_colors::OwoColorize;

pub struct Output;

impl Output {
    pub fn success(message: &str) {
        println!("{} {}", "✓".green().bold(), message);
    }

    pub fn error(message: &str) {
        eprintln!("{} {}", "✗".red().bold(), message.red());
    }

    pub fn info(message: &str) {
        println!("{} {}", "ℹ".blue().bold(), message.bright_blue());
    }

    pub fn warning(message: &str) {
        println!("{} {}", "⚠".yellow().bold(), message.yellow());
    }

    pub fn header(message: &str) {
        println!("\n{}\n", message.bright_cyan().bold());
    }

    pub fn step(step_num: usize, total: usize, message: &str) {
        println!(
            "{} {}",
            format!("[{}/{}]", step_num, total).bright_black(),
            message
        );
    }
}

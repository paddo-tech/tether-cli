use indicatif::{ProgressBar, ProgressStyle};
use owo_colors::OwoColorize;
use std::time::Duration;

use super::Output;

pub struct Progress;

impl Progress {
    pub fn spinner(message: &str) -> ProgressBar {
        let pb = ProgressBar::new_spinner();
        pb.set_style(
            ProgressStyle::default_spinner()
                .template("{spinner:.cyan} {msg}")
                .unwrap()
                .tick_chars("⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏"),
        );
        pb.set_message(message.to_string());
        pb.enable_steady_tick(Duration::from_millis(80));
        pb
    }

    pub fn bar(total: u64, message: &str) -> ProgressBar {
        let pb = ProgressBar::new(total);
        pb.set_style(
            ProgressStyle::default_bar()
                .template("{msg} [{bar:30.cyan/dim}] {pos}/{len}")
                .unwrap()
                .progress_chars("━╸─"),
        );
        pb.set_message(message.to_string());
        pb
    }

    pub fn finish_success(pb: &ProgressBar, message: &str) {
        pb.finish_with_message(format!("{} {}", Output::CHECK.green(), message));
    }

    pub fn finish_error(pb: &ProgressBar, message: &str) {
        pb.finish_with_message(format!("{} {}", Output::CROSS.red(), message));
    }
}

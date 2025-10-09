use anyhow::Result;
use inquire::{Confirm, Select, Text};

pub struct Prompt;

impl Prompt {
    pub fn confirm(message: &str, default: bool) -> Result<bool> {
        Ok(Confirm::new(message).with_default(default).prompt()?)
    }

    pub fn input(message: &str, default: Option<&str>) -> Result<String> {
        let mut prompt = Text::new(message);

        if let Some(d) = default {
            prompt = prompt.with_default(d);
        }

        Ok(prompt.prompt()?)
    }

    pub fn select(message: &str, options: Vec<&str>, default: usize) -> Result<usize> {
        let selection = Select::new(message, options.clone())
            .with_starting_cursor(default)
            .prompt()?;

        // Find the index of the selected option
        Ok(options.iter().position(|&x| x == selection).unwrap_or(0))
    }
}

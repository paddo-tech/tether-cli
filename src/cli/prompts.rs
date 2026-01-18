use anyhow::Result;
use inquire::ui::{Color, RenderConfig, StyleSheet, Styled};
use inquire::{Confirm, MultiSelect, Password, PasswordDisplayMode, Select, Text};

pub struct Prompt;

impl Prompt {
    pub fn theme() -> RenderConfig<'static> {
        RenderConfig::default()
            .with_prompt_prefix(Styled::new("›").with_fg(Color::LightCyan))
            .with_highlighted_option_prefix(Styled::new("›").with_fg(Color::LightGreen))
            .with_answer(StyleSheet::new().with_fg(Color::LightGreen))
            .with_help_message(StyleSheet::new().with_fg(Color::DarkGrey))
    }

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

    pub fn input_with_help(message: &str, default: Option<&str>, help: &str) -> Result<String> {
        let mut prompt = Text::new(message).with_help_message(help);

        if let Some(d) = default {
            prompt = prompt.with_default(d);
        }

        Ok(prompt.prompt()?)
    }

    pub fn select(message: &str, options: Vec<&str>, default: usize) -> Result<usize> {
        let selection = Select::new(message, options.clone())
            .with_starting_cursor(default)
            .prompt()?;

        Ok(options.iter().position(|&x| x == selection).unwrap_or(0))
    }

    /// Multi-select with default selections. Returns indices of selected options.
    pub fn multi_select(
        message: &str,
        options: Vec<&str>,
        defaults: &[usize],
    ) -> Result<Vec<usize>> {
        let selections = MultiSelect::new(message, options.clone())
            .with_default(defaults)
            .prompt()?;

        Ok(selections
            .iter()
            .filter_map(|s| options.iter().position(|&x| x == *s))
            .collect())
    }

    pub fn password(message: &str) -> Result<String> {
        Ok(Password::new(message)
            .with_display_mode(PasswordDisplayMode::Masked)
            .without_confirmation()
            .prompt()?)
    }

    pub fn password_with_help(message: &str, help: &str) -> Result<String> {
        Ok(Password::new(message)
            .with_display_mode(PasswordDisplayMode::Masked)
            .with_help_message(help)
            .without_confirmation()
            .prompt()?)
    }

    pub fn password_with_confirm(message: &str, confirm_message: &str) -> Result<String> {
        Ok(Password::new(message)
            .with_display_mode(PasswordDisplayMode::Masked)
            .with_custom_confirmation_message(confirm_message)
            .prompt()?)
    }
}

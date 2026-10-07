use anyhow::Result;
use inquire::ui::{Color, RenderConfig, StyleSheet, Styled};
use inquire::{Confirm, MultiSelect, Password, PasswordDisplayMode, Select, Text};
use std::io::IsTerminal;
use std::sync::atomic::{AtomicBool, Ordering};

pub struct Prompt;

static ASSUME_YES: AtomicBool = AtomicBool::new(false);

/// Every prompt goes through these functions, so `-y` and a missing terminal act the same
/// everywhere: `-y` confirms an action and takes each question's default answer. Without a
/// terminal and without `-y`, a prompt fails and names `-y`. A prompt with no default
/// answer, such as a passphrase, fails without a terminal even with `-y`.
impl Prompt {
    pub fn theme() -> RenderConfig<'static> {
        RenderConfig::default()
            .with_prompt_prefix(Styled::new("›").with_fg(Color::LightCyan))
            .with_highlighted_option_prefix(Styled::new("›").with_fg(Color::LightGreen))
            .with_answer(StyleSheet::new().with_fg(Color::LightGreen))
            .with_help_message(StyleSheet::new().with_fg(Color::DarkGrey))
    }

    pub fn set_assume_yes(yes: bool) {
        ASSUME_YES.store(yes, Ordering::Relaxed);
    }

    pub fn assume_yes() -> bool {
        ASSUME_YES.load(Ordering::Relaxed)
    }

    pub fn is_interactive() -> bool {
        std::io::stdin().is_terminal()
    }

    fn needs_terminal(message: &str, with_yes: bool) -> anyhow::Error {
        let question = message.trim().trim_end_matches([':', '?']);
        if with_yes {
            anyhow::anyhow!(
                "\"{}\" needs a terminal. Pass -y to accept the default answer",
                question
            )
        } else {
            anyhow::anyhow!(
                "\"{}\" needs a terminal and has no default answer",
                question
            )
        }
    }

    /// Confirm an action the user asked for. `-y` confirms it.
    pub fn confirm(message: &str, default: bool) -> Result<bool> {
        if Self::assume_yes() {
            return Ok(true);
        }
        if !Self::is_interactive() {
            return Err(anyhow::anyhow!(
                "\"{}\" needs a terminal. Pass -y to confirm",
                message.trim().trim_end_matches('?')
            ));
        }
        Ok(Confirm::new(message).with_default(default).prompt()?)
    }

    /// A yes/no question that is not a confirmation. `-y` takes `default`, so a question
    /// whose safe answer is no stays no.
    pub fn question(message: &str, default: bool) -> Result<bool> {
        if Self::assume_yes() {
            return Ok(default);
        }
        if !Self::is_interactive() {
            return Err(Self::needs_terminal(message, true));
        }
        Ok(Confirm::new(message).with_default(default).prompt()?)
    }

    pub fn input(message: &str, default: Option<&str>) -> Result<String> {
        if let Some(answer) = Self::unattended(message, default)? {
            return Ok(answer);
        }
        let mut prompt = Text::new(message);

        if let Some(d) = default {
            prompt = prompt.with_default(d);
        }

        Ok(prompt.prompt()?)
    }

    pub fn input_with_help(message: &str, default: Option<&str>, help: &str) -> Result<String> {
        if let Some(answer) = Self::unattended(message, default)? {
            return Ok(answer);
        }
        let mut prompt = Text::new(message).with_help_message(help);

        if let Some(d) = default {
            prompt = prompt.with_default(d);
        }

        Ok(prompt.prompt()?)
    }

    /// The default answer under `-y`, `None` to ask, or an error without a terminal.
    fn unattended(message: &str, default: Option<&str>) -> Result<Option<String>> {
        match default {
            Some(d) if Self::assume_yes() => Ok(Some(d.to_string())),
            _ if Self::is_interactive() => Ok(None),
            Some(_) => Err(Self::needs_terminal(message, true)),
            None => Err(Self::needs_terminal(message, false)),
        }
    }

    /// `default` is where the cursor starts and the answer `-y` takes.
    pub fn select(message: &str, options: Vec<&str>, default: usize) -> Result<usize> {
        if Self::assume_yes() {
            return Ok(default);
        }
        if !Self::is_interactive() {
            return Err(Self::needs_terminal(message, true));
        }
        let selection = Select::new(message, options.clone())
            .with_starting_cursor(default)
            .prompt()?;

        Ok(options.iter().position(|&x| x == selection).unwrap_or(0))
    }

    /// Pick one of `options`, which have no default answer.
    pub fn pick(message: &str, options: Vec<String>) -> Result<String> {
        if !Self::is_interactive() {
            return Err(Self::needs_terminal(message, false));
        }
        Ok(Select::new(message, options).prompt()?)
    }

    /// Multi-select with default selections. Returns indices of selected options.
    pub fn multi_select(
        message: &str,
        options: Vec<&str>,
        defaults: &[usize],
    ) -> Result<Vec<usize>> {
        if Self::assume_yes() {
            return Ok(defaults.to_vec());
        }
        if !Self::is_interactive() {
            return Err(Self::needs_terminal(message, true));
        }
        let selections = MultiSelect::new(message, options.clone())
            .with_default(defaults)
            .prompt()?;

        Ok(selections
            .iter()
            .filter_map(|s| options.iter().position(|&x| x == *s))
            .collect())
    }

    pub fn password(message: &str) -> Result<String> {
        if !Self::is_interactive() {
            return Err(Self::needs_terminal(message, false));
        }
        Ok(Password::new(message)
            .with_display_mode(PasswordDisplayMode::Masked)
            .without_confirmation()
            .prompt()?)
    }

    pub fn password_with_help(message: &str, help: &str) -> Result<String> {
        if !Self::is_interactive() {
            return Err(Self::needs_terminal(message, false));
        }
        Ok(Password::new(message)
            .with_display_mode(PasswordDisplayMode::Masked)
            .with_help_message(help)
            .without_confirmation()
            .prompt()?)
    }

    pub fn password_with_confirm(message: &str, confirm_message: &str) -> Result<String> {
        if !Self::is_interactive() {
            return Err(Self::needs_terminal(message, false));
        }
        Ok(Password::new(message)
            .with_display_mode(PasswordDisplayMode::Masked)
            .with_custom_confirmation_message(confirm_message)
            .prompt()?)
    }
}

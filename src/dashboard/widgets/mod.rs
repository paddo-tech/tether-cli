pub mod activity;
pub mod config;
pub mod files;
pub mod help;
pub mod machines;
pub mod packages;
pub mod status;

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
        "winget" => "Winget",
        _ => key,
    }
}

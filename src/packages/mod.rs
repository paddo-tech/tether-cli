pub mod brew;
pub mod bun;
pub mod gem;
pub mod manager;
pub mod mapping;
pub mod npm;
pub mod pnpm;
pub mod uv;
pub mod winget;

pub use brew::{normalize_formula_name, BrewManager, BrewfilePackages};
pub use bun::BunManager;
pub use gem::GemManager;
pub use manager::{PackageInfo, PackageManager};
pub use npm::NpmManager;
pub use pnpm::PnpmManager;
pub use uv::UvManager;
pub use winget::WingetManager;

/// Resolve a program name to its full path.
/// On Windows, `Command::new("npm")` only finds `.exe` files, but tools like
/// npm/pnpm install as `.cmd` files. `which` respects PATHEXT and finds them.
pub(crate) fn resolve_program(name: &str) -> std::path::PathBuf {
    which::which(name).unwrap_or_else(|_| name.into())
}

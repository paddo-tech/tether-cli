pub mod brew;
pub mod bun;
pub mod gem;
pub mod manager;
pub mod npm;
pub mod pnpm;
pub mod uv;

pub use brew::{normalize_formula_name, BrewManager, BrewfilePackages};
pub use bun::BunManager;
pub use gem::GemManager;
pub use manager::{PackageInfo, PackageManager};
pub use npm::NpmManager;
pub use pnpm::PnpmManager;
pub use uv::UvManager;

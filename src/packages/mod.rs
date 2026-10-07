pub mod brew;
pub mod bun;
pub mod gem;
pub mod inbox;
pub mod manager;
pub mod npm;
pub mod osv;
pub mod pin;
pub mod pnpm;
pub mod policy;
pub mod resolve;
pub mod uv;
pub mod validate;

pub use brew::{normalize_formula_name, BrewManager, BrewfilePackages};
pub use bun::BunManager;
pub use gem::GemManager;
pub use manager::{
    install_upgrades, planned_upgrades, update_all, Hold, PackageInfo, PackageManager, Upgrade,
};
pub use npm::NpmManager;
pub use pnpm::PnpmManager;
pub use policy::{Cooldown, PackagePolicy};
pub use uv::UvManager;
pub use validate::{validate_name, validate_version, Ecosystem};

/// Package managers read local package files and project config (`*.gem`, `.npmrc`,
/// `pnpm-workspace.yaml`) from the working directory, so every one runs in an empty
/// directory Tether owns instead of the caller's.
pub fn command(program: &str) -> anyhow::Result<tokio::process::Command> {
    let dir = crate::home_dir()?.join(".tether").join("run");
    if std::fs::read_dir(&dir).is_ok_and(|mut entries| entries.next().is_some()) {
        std::fs::remove_dir_all(&dir)?;
    }
    std::fs::create_dir_all(&dir)?;
    let mut cmd = tokio::process::Command::new(program);
    cmd.current_dir(dir);
    Ok(cmd)
}

/// Some tools (pnpm) report failures on stdout, so surface both streams.
pub fn command_error_message(output: &std::process::Output) -> String {
    [&output.stderr, &output.stdout]
        .iter()
        .map(|bytes| String::from_utf8_lossy(bytes).trim().to_string())
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join("\n")
}

/// Manager keys of package ids (`manager:name`) and machine records.
pub const MANAGER_KEYS: &[&str] = &[
    "brew_formulae",
    "brew_casks",
    "brew_taps",
    "npm",
    "pnpm",
    "bun",
    "gem",
    "uv",
];

/// The manager key `name` stands for. `brew` and `cask` are short for `brew_formulae` and
/// `brew_casks`.
pub fn key_of_manager(name: &str) -> Option<&'static str> {
    match name {
        "brew" => Some("brew_formulae"),
        "cask" => Some("brew_casks"),
        _ => MANAGER_KEYS.iter().copied().find(|k| *k == name),
    }
}

/// `id` with a short manager name replaced by its key, so `brew:jq` reads as
/// `brew_formulae:jq`.
pub fn normalize_id(id: &str) -> String {
    match id
        .split_once(':')
        .and_then(|(m, n)| Some((key_of_manager(m)?, n)))
    {
        Some((key, name)) => format!("{}:{}", key, name),
        None => id.to_string(),
    }
}

/// Look up a simple (non-brew) manager by its machine-state key.
pub fn manager_for_key(key: &str) -> Option<Box<dyn PackageManager>> {
    Some(match key {
        "npm" => Box::new(NpmManager::new()),
        "pnpm" => Box::new(PnpmManager::new()),
        "bun" => Box::new(BunManager::new()),
        "gem" => Box::new(GemManager::new()),
        "uv" => Box::new(UvManager::new()),
        _ => return None,
    })
}

/// Uninstall a package by its machine-state key, such as `brew_casks` or `npm`.
pub async fn uninstall(manager_key: &str, name: &str) -> anyhow::Result<()> {
    let manager: Box<dyn PackageManager> = match manager_key {
        "brew_formulae" | "brew_casks" => Box::new(BrewManager),
        _ => manager_for_key(manager_key)
            .ok_or_else(|| anyhow::anyhow!("Unknown manager: {}", manager_key))?,
    };
    manager.uninstall(name).await
}

#[cfg(test)]
mod tests {
    use super::command_error_message;
    use std::os::unix::process::ExitStatusExt;
    use std::process::{ExitStatus, Output};

    fn output(stderr: &[u8], stdout: &[u8]) -> Output {
        Output {
            status: ExitStatus::from_raw(0),
            stdout: stdout.to_vec(),
            stderr: stderr.to_vec(),
        }
    }

    #[test]
    fn short_manager_names_read_as_keys() {
        use super::{key_of_manager, normalize_id};
        assert_eq!(key_of_manager("brew"), Some("brew_formulae"));
        assert_eq!(key_of_manager("cask"), Some("brew_casks"));
        assert_eq!(key_of_manager("npm"), Some("npm"));
        assert_eq!(key_of_manager("nope"), None);
        assert_eq!(normalize_id("brew:jq"), "brew_formulae:jq");
        assert_eq!(normalize_id("cask:zoom"), "brew_casks:zoom");
        assert_eq!(normalize_id("npm:x"), "npm:x");
        assert_eq!(normalize_id("machine:abc"), "machine:abc");
        assert_eq!(normalize_id("typescript"), "typescript");
    }

    #[test]
    fn joins_trimmed_streams_stderr_first() {
        assert_eq!(
            command_error_message(&output(b"  warn  ", b" ERR_PNPM_X \n")),
            "warn\nERR_PNPM_X"
        );
    }

    #[test]
    fn falls_back_to_stdout_when_stderr_blank() {
        assert_eq!(
            command_error_message(&output(b"   \n", b"  ENOENT  ")),
            "ENOENT"
        );
    }

    #[test]
    fn empty_when_both_blank() {
        assert_eq!(command_error_message(&output(b"", b"  ")), "");
    }
}

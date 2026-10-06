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
pub use manager::{PackageInfo, PackageManager};
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

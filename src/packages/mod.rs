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

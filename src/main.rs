use clap::Parser;
use std::process::ExitCode;
use tether::cli::{Cli, Prompt};

/// Every failure prints one `Error: ...` line on stderr and exits 1.
#[tokio::main]
async fn main() -> ExitCode {
    let cli = Cli::parse();
    // The daemon's stderr is daemon.log, and launchd and systemd start it without RUST_LOG.
    let default_filter = if cli.is_daemon_run() { "info" } else { "error" };
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or(default_filter))
        .init();
    inquire::set_global_render_config(if tether::cli::output::color_enabled() {
        Prompt::theme()
    } else {
        inquire::ui::RenderConfig::empty()
    });

    match cli.run().await {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("Error: {:#}", e);
            ExitCode::FAILURE
        }
    }
}

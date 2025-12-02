use anyhow::Result;
use clap::Parser;
use tether::cli::{Cli, Prompt};

#[tokio::main]
async fn main() -> Result<()> {
    env_logger::init();
    inquire::set_global_render_config(Prompt::theme());

    let cli = Cli::parse();
    cli.run().await?;

    Ok(())
}

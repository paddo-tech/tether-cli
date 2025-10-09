use anyhow::Result;
use clap::Parser;
use tether::cli::Cli;

#[tokio::main]
async fn main() -> Result<()> {
    env_logger::init();

    let cli = Cli::parse();
    cli.run().await?;

    Ok(())
}

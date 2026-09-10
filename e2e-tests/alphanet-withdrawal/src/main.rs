use alphanet_withdrawal::cli::Cli;
use clap::Parser;

#[tokio::main]
async fn main() -> eyre::Result<()> {
    let cli = Cli::parse();
    cli.run().await?;
    Ok(())
}

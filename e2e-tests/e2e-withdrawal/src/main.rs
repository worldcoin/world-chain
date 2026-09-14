use clap::Parser;
use e2e_withdrawal::cli::Cli;

#[tokio::main]
async fn main() -> eyre::Result<()> {
    let cli = Cli::parse();
    let step_outcome = cli.run().await?;
    tracing::info!(?step_outcome);
    Ok(())
}

use clap::Parser;
use e2e_withdrawal::cli::Cli;

#[tokio::main]
async fn main() -> eyre::Result<()> {
    let cli = Cli::parse();
    cli.run().await?;
    Ok(())
}

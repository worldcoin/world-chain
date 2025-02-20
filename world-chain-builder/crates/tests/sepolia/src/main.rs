use clap::Parser;
use cli::identities::generate_identities;
use cli::transactions::{create_bundle, send_bundle};
use cli::Cli;

mod cli;

#[tokio::main]
async fn main() -> eyre::Result<()> {
    tracing_subscriber::fmt::init();
    let cli = Cli::parse();
    match cli.command {
        cli::Commands::Generate(args) => generate_identities(args).await?,
        cli::Commands::Bundle(args) => create_bundle(args).await?,
        cli::Commands::Send(args) => send_bundle(args).await?,
    }
    Ok(())
}

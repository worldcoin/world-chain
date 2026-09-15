use clap::Parser;
use e2e_withdrawal::cli::Cli;
use eyre::eyre::{WrapErr, eyre};
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> eyre::Result<()> {
    let cli = Cli::parse();
    let filter = EnvFilter::builder()
        .with_default_directive(tracing::Level::INFO.into())
        .from_env()
        .wrap_err("invalid RUST_LOG filter")?;
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .try_init()
        .map_err(|err| eyre!("failed to initialize tracing: {err}"))?;

    if let Err(error) = cli.run().await {
        tracing::error!(error = ?error, "withdrawal command failed");
        return Err(error.into());
    }
    Ok(())
}

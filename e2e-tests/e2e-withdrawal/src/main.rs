use clap::Parser;
use e2e_withdrawal::cli::Cli;
use eyre::eyre::{WrapErr, eyre};
use std::process::ExitCode;
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> eyre::Result<ExitCode> {
    let cli = Cli::parse();
    let filter = EnvFilter::builder()
        .with_default_directive(tracing::Level::WARN.into())
        .from_env()
        .wrap_err("invalid RUST_LOG filter")?;
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .json()
        .with_writer(std::io::stderr)
        .try_init()
        .map_err(|err| eyre!("failed to initialize tracing: {err}"))?;

    match cli.run().await {
        Ok(outcome) => {
            // One-shot CLI output; routine outcomes are not production INFO logs.
            println!("{outcome:?}");
            Ok(ExitCode::SUCCESS)
        }
        Err(error) => {
            tracing::error!(?error, "withdrawal command failed");
            Ok(ExitCode::FAILURE)
        }
    }
}

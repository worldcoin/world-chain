#![cfg_attr(not(test), warn(unused_crate_dependencies))]

use alloy_primitives::{address, Address};
use clap::Parser;
use cli::identities::generate_identities;
use cli::transactions::{create_bundle, send_bundle};
use cli::Cli;

mod cli;

pub const PBH_SIGNATURE_AGGREGATOR: Address = address!("8af27Ee9AF538C48C7D2a2c8BD6a40eF830e2489");

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

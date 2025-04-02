#![cfg_attr(not(test), warn(unused_crate_dependencies))]

use alloy_primitives::{address, Address};
use clap::Parser;
use cli::identities::generate_identities;
use cli::transactions::{create_bundle, send_bundle};
use cli::Cli;

mod cli;

pub const PBH_SIGNATURE_AGGREGATOR: Address = address!("f07d3efadD82A1F0b4C5Cc3476806d9a170147Ba");

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

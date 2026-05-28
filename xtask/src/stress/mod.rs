use alloy_primitives::{Address, address};
use cli::{
    Cli,
    identities::generate_identities,
    transactions::{create_bundle, send_aa, send_bundle, send_invalid_pbh, stake_aa},
};

use self::cli::transactions::load_test;

pub mod cli;

pub const PBH_SIGNATURE_AGGREGATOR: Address = address!("8af27Ee9AF538C48C7D2a2c8BD6a40eF830e2489");

pub type Args = Cli;

pub async fn run(args: Args) -> eyre::Result<()> {
    match args.command {
        cli::Commands::Generate(args) => generate_identities(args).await?,
        cli::Commands::Bundle(args) => create_bundle(args).await?,
        cli::Commands::Send(args) => send_bundle(args).await?,
        cli::Commands::SendAA(args) => send_aa(args).await?,
        cli::Commands::StakeAA(args) => stake_aa(args).await?,
        cli::Commands::SendInvalidProofPBH(args) => send_invalid_pbh(args).await?,
        cli::Commands::LoadTest(args) => load_test(args).await?,
    }
    Ok(())
}

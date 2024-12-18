use alloy_primitives::Address;
use clap::value_parser;
use reth_optimism_node::args::RollupArgs;
/// Parameters for rollup configuration
#[derive(Debug, Clone, Default, PartialEq, Eq, clap::Args)]
pub struct ExtArgs {
    /// op rollup args
    #[command(flatten)]
    pub rollup_args: RollupArgs,

    /// builder args
    #[command(flatten)]
    pub builder_args: WorldChainBuilderArgs,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, clap::Args)]
#[command(next_help_heading = "PBH Builder")]
pub struct WorldChainBuilderArgs {
    /// Sets the number of allowed PBH transactions per month
    #[arg(long = "builder.num_pbh_txs", default_value = "30")]
    pub num_pbh_txs: u8,

    /// Sets the max blockspace reserved for verified transactions. If there are not enough
    /// verified transactions to fill the capacity, the remaining blockspace will be filled with
    /// unverified transactions.
    /// This arg is a percentage of the total blockspace with the default set to 70 (ie 70%).
    #[arg(long = "builder.verified_blockspace_capacity", default_value = "70", value_parser = value_parser!(u8).range(0..=100))]
    pub verified_blockspace_capacity: u8,

    /// Sets the ERC-4337 EntryPoint Proxy contract address
    /// This contract is used to validate 4337 PBH bundles
    #[arg(long = "builder.pbh_validator")]
    pub pbh_validator: Address,

    /// Sets the ERC0-7766 Signature Aggregator contract address
    /// This contract signifies that a given bundle should receive priority inclusion if it passes validation
    #[arg(long = "builder.signature_aggregator")]
    pub signature_aggregator: Address,
}

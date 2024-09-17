use reth_node_optimism::args::RollupArgs;

/// Parameters for rollup configuration
#[derive(Debug, Clone, Default, PartialEq, Eq, clap::Args)]
pub struct ExtArgs {
    /// op rollup args
    #[command(flatten)]
    pub rollup_args: RollupArgs,

    /// builder args
    #[command(flatten)]
    pub builder_args: WorldCoinBuilderArgs,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, clap::Args)]
#[command(next_help_heading = "PBH Builder")]
pub struct WorldCoinBuilderArgs {
    /// Clears existing pbh semaphore nullifiers from the database
    #[arg(long = "builder.clear_nullifiers")]
    pub clear_nullifiers: bool,

    /// Sets the number of allowed PBH transactions per month
    #[arg(long = "builder.num_pbh_txs", default_value = "30")]
    pub num_pbh_txs: u16,
}

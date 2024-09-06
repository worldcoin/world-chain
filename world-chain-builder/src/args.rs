use reth_node_optimism::args::RollupArgs;

/// Parameters for rollup configuration
#[derive(Debug, Clone, Default, PartialEq, Eq, clap::Args)]
pub struct ExtArgs {
    /// op rollup args
    #[command(flatten)]
    pub rollup_args: RollupArgs,

    /// builder args
    #[command(flatten)]
    pub builder_args: PbhBuilderArgs,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, clap::Args)]
#[command(next_help_heading = "PBH Builder")]
pub struct PbhBuilderArgs {
    /// Just a place holder for now
    #[arg(long = "builder.beacon_endpoints")]
    pub beacon_endpoints: Vec<String>,
}

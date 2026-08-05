use crate::WorldChainSpecParser;
use clap::Subcommand;
use core::fmt;
use reth_cli::chainspec::ChainSpecParser;
use reth_cli_commands::{
    config_cmd, db, dump_genesis, init_cmd,
    node::{self, NoArgs},
    p2p, prune, re_execute, stage,
};

#[derive(Debug, Subcommand)]
pub enum Commands<
    Spec: ChainSpecParser = WorldChainSpecParser,
    Ext: clap::Args + fmt::Debug = NoArgs,
> {
    /// Start the node
    #[command(name = "node")]
    Node(Box<node::NodeCommand<Spec, Ext>>),
    /// Initialize the database from a genesis file.
    #[command(name = "init")]
    Init(init_cmd::InitCommand<Spec>),
    /// Dumps genesis block JSON configuration to stdout.
    DumpGenesis(dump_genesis::DumpGenesisCommand<Spec>),
    /// Database debugging utilities
    #[command(name = "db")]
    Db(db::Command<Spec>),
    /// Manipulate individual stages.
    #[command(name = "stage")]
    Stage(Box<stage::Command<Spec>>),
    /// P2P Debugging utilities
    #[command(name = "p2p")]
    P2P(Box<p2p::Command<Spec>>),
    /// Write config to stdout
    #[command(name = "config")]
    Config(config_cmd::Command),
    /// Prune according to the configuration without any limits
    #[command(name = "prune")]
    Prune(prune::PruneCommand<Spec>),
    /// Re-execute blocks in parallel to verify historical sync correctness.
    #[command(name = "re-execute")]
    ReExecute(re_execute::Command<Spec>),
    /// Manage storage of historical proofs in expanded trie db in fault proof window.
    #[command(name = "proofs")]
    OpProofs(op_proofs::Command<Spec>),
}

use crate::WorldChainSpecParser;
use clap::Subcommand;
use core::fmt;
use reth_chainspec::{EthChainSpec, EthereumHardforks, Hardforks};
use reth_cli::chainspec::ChainSpecParser;
use reth_cli_commands::{
    config_cmd, db, dump_genesis, init_cmd,
    node::{self, NoArgs},
    p2p, prune, re_execute, stage,
};
use std::sync::Arc;
use world_chain_commands::proofs;

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
    OpProofs(proofs::Command<Spec>),
}

impl<
    C: ChainSpecParser<ChainSpec: EthChainSpec + Hardforks + EthereumHardforks>,
    Ext: clap::Args + fmt::Debug,
> Commands<C, Ext>
{
    /// Returns the underlying chain being used for commands
    pub fn chain_spec(&self) -> Option<&Arc<C::ChainSpec>> {
        match self {
            Self::Node(cmd) => cmd.chain_spec(),
            Self::Init(cmd) => cmd.chain_spec(),
            Self::DumpGenesis(cmd) => cmd.chain_spec(),
            Self::Db(cmd) => cmd.chain_spec(),
            Self::Stage(cmd) => cmd.chain_spec(),
            Self::P2P(cmd) => cmd.chain_spec(),
            Self::Config(_) => None,
            Self::Prune(cmd) => cmd.chain_spec(),
            Self::ReExecute(cmd) => cmd.chain_spec(),
            Self::OpProofs(cmd) => cmd.chain_spec(),
        }
    }
}

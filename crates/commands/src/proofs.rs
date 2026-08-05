use clap::{Parser, Subcommand};
use reth_cli::chainspec::ChainSpecParser;
use reth_cli_commands::common::CliNodeTypes;
use std::sync::Arc;
use world_chain_chainspec::WorldChainSpec;

pub mod init;

/// `op-reth op-proofs` command
#[derive(Debug, Parser)]
pub struct Command<C: ChainSpecParser> {
    #[command(subcommand)]
    command: Subcommands<C>,
}

impl<C: ChainSpecParser<ChainSpec = WorldChainSpec>> Command<C> {
    /// Execute `op-proofs` command
    pub async fn execute<N: CliNodeTypes<ChainSpec = C::ChainSpec>>(
        self,
        runtime: reth_tasks::Runtime,
    ) -> eyre::Result<()> {
        match self.command {
            Subcommands::Init(cmd) => cmd.execute::<N>(runtime).await,
        }
    }
}

impl<C: ChainSpecParser> Command<C> {
    /// Returns the underlying chain being used to run this command
    pub const fn chain_spec(&self) -> Option<&Arc<C::ChainSpec>> {
        match &self.command {
            Subcommands::Init(cmd) => cmd.chain_spec(),
        }
    }
}

/// `op-reth op-proofs` subcommands
#[derive(Debug, Subcommand)]
pub enum Subcommands<C: ChainSpecParser> {
    /// Initialize the proofs storage with the current state of the chain
    #[command(name = "init")]
    Init(init::InitCommand<C>),
}

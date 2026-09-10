use crate::cmd::Command;
use clap::Parser;

/// Cli struct for the alphanet-withdrawal binary.
#[derive(Debug, Parser)]
pub struct Cli {
    /// The command to run.
    #[command(subcommand)]
    command: Command,
}

impl Cli {
    /// Run the cli binary to completion.
    pub async fn run(&self) -> eyre::Result<()> {
        self.command.run().await
    }
}

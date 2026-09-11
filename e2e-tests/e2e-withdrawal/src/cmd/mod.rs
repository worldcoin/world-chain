use crate::args::{FinalizeArgs, InitArgs, ProveArgs, StepArgs};
use clap::Subcommand;

mod finalize;
mod init;
mod prove;
mod step;

/// Commands for an end-to-end L2->L1 withdrawal flow.
///
/// The workflow is:
///
/// 1. Initialize the withdrawal by sending an L2 transaction to the `L2ToL1MessagePasser` contract.
///
/// 2. Once a proof-system game exists whose related L2 block number is greater than or equal to the
///    block that included that transaction, send an L1 transaction to `OptimismPortal` to prove
///    the withdrawal.
///
/// 3. After the proof-system game is finalized and `OptimismPortal`'s `PROOF_MATURITY` delay has
///    elapsed, send an L1 transaction to `OptimismPortal` to finalize the withdrawal.
#[derive(Debug, Subcommand)]
pub enum Command {
    /// Initialize the L2->L1 withdrawal.
    Init(InitArgs),
    /// Prove the withdrawal.
    Prove(ProveArgs),
    /// Finalize the withdrawal.
    Finalize(FinalizeArgs),
    /// Run the next stage of the withdrawal workflow based on stored state.
    Step(StepArgs),
}

impl Command {
    /// Run the command to completion.
    pub async fn run(&self) -> eyre::Result<()> {
        match self {
            Self::Init(args) => init::run(args).await,
            Self::Prove(args) => prove::run(args).await,
            Self::Finalize(args) => finalize::run(args).await,
            Self::Step(args) => step::run(args).await,
        }
    }
}

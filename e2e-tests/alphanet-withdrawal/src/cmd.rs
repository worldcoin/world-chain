use crate::args::{FinalizeArgs, InitArgs, ProveArgs};
use clap::Subcommand;

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
}

impl Command {
    /// Run the command to completion.
    pub fn run(&self) -> eyre::Result<()> {
        match self {
            Self::Init(init_args) => {
                
            }
            Self::Prove(prove_args) => {

            }
            Self::Finalize(finalize_args) => {

            }
        }
        Ok(())
    }
}
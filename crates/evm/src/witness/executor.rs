use crossbeam_channel::Sender;
use reth_evm::{
    Evm,
    block::{BlockExecutionError, BlockExecutionResult, BlockExecutor, ExecutableTx},
};
use reth_revm::{State, witness::ExecutionWitnessRecord};

use super::CapturedBlock;

/// A [`BlockExecutor`] that delegates to an inner executor and, on
/// [`finish`](BlockExecutor::finish), snapshots the post-execution state into an
/// [`ExecutionWitnessRecord`] which is forwarded over an optional channel.
///
/// The snapshot is taken directly from the live read/write state cache used during execution, so
/// capturing incurs no re-execution.
#[derive(Debug)]
pub struct WitnessExecutor<E> {
    /// The wrapped block executor.
    pub(crate) inner: E,
    /// Optional channel that receives the captured block on `finish`.
    pub(crate) sender: Option<Sender<CapturedBlock>>,
    /// Number of the block being executed.
    pub(crate) block_number: u64,
}

/// Records an [`ExecutionWitnessRecord`] from a database when it is a [`State`] cache.
///
/// The [`BlockExecutorFactory`](reth_evm::block::BlockExecutorFactory) trait requires the produced
/// executor to be a [`BlockExecutor`] for *any* `DB: StateDB`, so the witness executor cannot
/// statically constrain its database to a [`State`]. We therefore use [`min_specialization`] to
/// record a real witness for the `&mut State<_>` used by reth's standard block-execution path and
/// fall back to a no-op for every other database.
///
/// [`min_specialization`]: https://doc.rust-lang.org/unstable-book/language-features/min-specialization.html
trait MaybeRecordWitness {
    /// Returns the recorded witness if `self` is backed by a [`State`] cache.
    fn maybe_record_witness(&self) -> Option<ExecutionWitnessRecord>;
}

impl<T> MaybeRecordWitness for T {
    default fn maybe_record_witness(&self) -> Option<ExecutionWitnessRecord> {
        None
    }
}

impl<DB> MaybeRecordWitness for &mut State<DB> {
    fn maybe_record_witness(&self) -> Option<ExecutionWitnessRecord> {
        Some(ExecutionWitnessRecord::from_executed_state(
            self,
            Default::default(),
        ))
    }
}

impl<E> BlockExecutor for WitnessExecutor<E>
where
    E: BlockExecutor,
{
    type Transaction = E::Transaction;
    type Receipt = E::Receipt;
    type Evm = E::Evm;
    type Result = E::Result;

    fn apply_pre_execution_changes(&mut self) -> Result<(), BlockExecutionError> {
        self.inner.apply_pre_execution_changes()
    }

    fn execute_transaction_without_commit(
        &mut self,
        tx: impl ExecutableTx<Self>,
    ) -> Result<Self::Result, BlockExecutionError> {
        self.inner.execute_transaction_without_commit(tx)
    }

    fn commit_transaction(&mut self, output: Self::Result) -> reth_evm::block::GasOutput {
        self.inner.commit_transaction(output)
    }

    fn finish(
        self,
    ) -> Result<(Self::Evm, BlockExecutionResult<Self::Receipt>), BlockExecutionError> {
        if let Some(sender) = &self.sender {
            // Snapshot the live execution state cache directly from the database backing execution.
            // During reth's standard block execution this database is `&mut State<_>`, so capturing
            // reads the already-populated read/write cache without any re-execution.
            if let Some(record) = self.inner.evm().db().maybe_record_witness() {
                let captured = CapturedBlock {
                    block_number: self.block_number,
                    record,
                };

                if let Err(err) = sender.send(captured) {
                    tracing::warn!(
                        target: "world_chain::witness",
                        block_number = self.block_number,
                        %err,
                        "failed to forward captured execution witness",
                    );
                }
            }
        }

        self.inner.finish()
    }

    fn evm_mut(&mut self) -> &mut Self::Evm {
        self.inner.evm_mut()
    }

    fn evm(&self) -> &Self::Evm {
        self.inner.evm()
    }

    fn receipts(&self) -> &[Self::Receipt] {
        self.inner.receipts()
    }
}

use crossbeam_channel::Sender;
use reth_evm::{
    Evm,
    block::{BlockExecutionError, BlockExecutionResult, BlockExecutor, ExecutableTx},
};
use reth_revm::State;
use revm::context::Block;
use revm_database::{EmptyDB, states::bundle_state::BundleRetention};
use tracing::error;

use crate::BlockExecutionWitness;

/// A [`BlockExecutor`] that delegates to an inner executor and, on
#[derive(Debug)]
pub struct WorldChainBlockExecutor<E> {
    /// The wrapped block executor.
    pub(crate) inner: E,
    /// Optional channel that receives the captured block on `finish`.
    pub(crate) sender: Option<Sender<BlockExecutionWitness>>,
}

/// Snapshots execution state from a database when it is a [`State`] cache.
trait MaybeWitness {
    /// Returns a state snapshot if `self` is backed by a [`State`] cache.
    fn witness(&self) -> Option<State<EmptyDB>>;
}

impl<T> MaybeWitness for T {
    default fn witness(&self) -> Option<State<EmptyDB>> {
        None
    }
}

impl<DB> MaybeWitness for &mut State<DB> {
    fn witness(&self) -> Option<State<EmptyDB>> {
        let mut snapshot = State::builder()
            .with_cached_prestate(self.cache.clone())
            .with_block_hashes(self.block_hashes.clone())
            .build();
        snapshot.bundle_state = self.bundle_state.clone();
        snapshot.transition_state = self.transition_state.clone();
        snapshot.merge_transitions(BundleRetention::PlainState);
        Some(snapshot)
    }
}

impl<E> BlockExecutor for WorldChainBlockExecutor<E>
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
        let (evm, result) = self.inner.finish()?;
        if let Some(sender) = self.sender
            && let Some(record) = evm.db().witness()
        {
            let block_number = evm.block().number();

            let captured = BlockExecutionWitness {
                block_number: block_number.to(),
                record,
            };

            let _ = sender.try_send(captured).inspect_err(|e| {
                error!(target: "world_chain::witness", %block_number, %e, "failed to send captured witness");
            });
        }

        Ok((evm, result))
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

#[cfg(test)]
mod tests {
    use super::MaybeWitness;
    use alloy_primitives::{Address, B256, U256};
    use reth_revm::State;
    use revm::{
        DatabaseCommit,
        state::{Account, AccountInfo},
    };

    #[test]
    fn witness_snapshot_preserves_pending_destruction_and_block_hash_reads() {
        let address = Address::with_last_byte(1);
        let mut state = State::builder().with_bundle_update().build();
        state.insert_account(
            address,
            AccountInfo {
                balance: U256::from(1),
                ..Default::default()
            },
        );
        let mut destroyed = Account::default();
        destroyed.mark_touch();
        destroyed.mark_selfdestruct();
        state.commit([(address, destroyed)].into_iter().collect());
        state.block_hashes.insert(42, B256::with_last_byte(2));

        let state_ref = &mut state;
        let snapshot = state_ref
            .witness()
            .expect("state cache produces a witness snapshot");
        assert!(snapshot.bundle_state.state[&address].was_destroyed());
        assert_eq!(snapshot.block_hashes.get(42), Some(B256::with_last_byte(2)));
        assert!(state.bundle_state.state.is_empty());
        assert!(
            state
                .transition_state
                .as_ref()
                .is_some_and(|transitions| !transitions.transitions.is_empty())
        );
    }
}

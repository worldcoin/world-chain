use crate::execution_strategy::{self, ExecutionStrategy, StateRootResult};
use alloy_primitives::B256;
use reth_evm::{ConfigureEvm, block::BlockExecutionError};
use reth_provider::StateProviderFactory;
use revm_database::BundleState;

/// Strategy for state root computation.
pub trait StateRootStrategy: Send + Sync {
    type Handle: StateRootHandle;

    fn prepare(
        &self,
        client: impl StateProviderFactory + Clone + 'static,
        parent_hash: B256,
        bundle_state: BundleState,
    ) -> Result<Self::Handle, BlockExecutionError>;
}

pub trait StateRootHandle: Send {
    fn finish(self) -> Result<StateRootResult, BlockExecutionError>;
}

/// Associates execution and state-root strategies for an EVM.
///
/// The `Execution` strategy receives the `StateRoot` strategy via [`ValidationCtx`],
/// so the state root type is declared once here and flows through to the executor.
///
/// [`ValidationCtx`]: crate::execution_strategy::ValidationCtx
pub trait FlashblockTypes<Evm: ConfigureEvm> {
    type StateRoot: StateRootStrategy + Default;
    type Execution: ExecutionStrategy<Evm, Self::StateRoot>;
}

#[derive(Default)]
pub struct AsyncStateRootStrategy;

pub struct ChannelStateRootHandle {
    pub receiver: crossbeam_channel::Receiver<Result<StateRootResult, BlockExecutionError>>,
}

impl StateRootStrategy for AsyncStateRootStrategy {
    type Handle = ChannelStateRootHandle;

    fn prepare(
        &self,
        client: impl StateProviderFactory + Clone + 'static,
        parent_hash: B256,
        bundle_state: BundleState,
    ) -> Result<Self::Handle, BlockExecutionError> {
        let (sender, receiver) = crossbeam_channel::bounded(1);
        let state_root_provider = client
            .state_by_block_hash(parent_hash)
            .map_err(BlockExecutionError::other)?;
        rayon::spawn(move || {
            let result = execution_strategy::compute_state_root(
                state_root_provider.into(),
                bundle_state.state.iter(),
            );
            let _ = sender.send(result);
        });
        Ok(ChannelStateRootHandle { receiver })
    }
}

impl StateRootHandle for ChannelStateRootHandle {
    fn finish(self) -> Result<StateRootResult, BlockExecutionError> {
        self.receiver.recv().map_err(BlockExecutionError::other)?
    }
}

/// Synchronous state root strategy: computes the state root inline on `prepare`.
#[derive(Default)]
pub struct SyncStateRootStrategy;

pub struct SyncStateRootHandle(Result<StateRootResult, BlockExecutionError>);

impl StateRootStrategy for SyncStateRootStrategy {
    type Handle = SyncStateRootHandle;

    fn prepare(
        &self,
        client: impl StateProviderFactory + Clone + 'static,
        parent_hash: B256,
        bundle_state: BundleState,
    ) -> Result<Self::Handle, BlockExecutionError> {
        let state_root_provider = client
            .state_by_block_hash(parent_hash)
            .map_err(BlockExecutionError::other)?;
        let result = execution_strategy::compute_state_root(
            state_root_provider.into(),
            bundle_state.state.iter(),
        );
        Ok(SyncStateRootHandle(result))
    }
}

impl StateRootHandle for SyncStateRootHandle {
    fn finish(self) -> Result<StateRootResult, BlockExecutionError> {
        self.0
    }
}

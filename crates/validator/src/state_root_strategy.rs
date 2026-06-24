use std::sync::Arc;

use crate::execution_strategy::{ExecutionStrategy, StateRootResult};
use alloy_primitives::B256;
use reth_evm::{ConfigureEvm, block::BlockExecutionError};
use reth_provider::{StateProvider, StateProviderFactory};
use reth_trie_common::HashedPostState;

/// Walks the trie to compute the state root for the given hashed post-state.
pub fn compute_state_root(
    state_provider: Arc<dyn StateProvider + Send>,
    hashed_state: HashedPostState,
) -> Result<StateRootResult, BlockExecutionError> {
    let (state_root, trie_updates) = state_provider
        .state_root_with_updates(hashed_state.clone())
        .map_err(BlockExecutionError::other)?;
    Ok(StateRootResult {
        state_root,
        trie_updates,
        hashed_state,
    })
}

/// Strategy for state root computation.
pub trait StateRootStrategy: Send + Sync {
    type Handle: StateRootHandle;

    fn prepare(
        &self,
        client: impl StateProviderFactory + Clone + 'static,
        parent_hash: B256,
        hashed_state: HashedPostState,
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
        hashed_state: HashedPostState,
    ) -> Result<Self::Handle, BlockExecutionError> {
        let (sender, receiver) = crossbeam_channel::bounded(1);
        let state_root_provider = client
            .state_by_block_hash(parent_hash)
            .map_err(BlockExecutionError::other)?;

        std::thread::Builder::new()
            .name("flashblock-state-root".to_string())
            .spawn(move || {
                let result = compute_state_root(state_root_provider.into(), hashed_state);
                let _ = sender.send(result);
            })
            .map_err(BlockExecutionError::other)?;
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
        hashed_state: HashedPostState,
    ) -> Result<Self::Handle, BlockExecutionError> {
        let state_root_provider = client
            .state_by_block_hash(parent_hash)
            .map_err(BlockExecutionError::other)?;
        let result = compute_state_root(state_root_provider.into(), hashed_state);
        Ok(SyncStateRootHandle(result))
    }
}

impl StateRootHandle for SyncStateRootHandle {
    fn finish(self) -> Result<StateRootResult, BlockExecutionError> {
        self.0
    }
}

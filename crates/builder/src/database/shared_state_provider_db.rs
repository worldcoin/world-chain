use std::sync::Arc;

use alloy_primitives::{Address, B256};
use parking_lot::Mutex;
use reth_provider::{
    AccountReader, BlockHashReader, BytecodeReader, ProviderError, StateProvider, StateProviderBox,
};
use revm::DatabaseRef;

/// A cloneable [`DatabaseRef`] adapter over [`StateProviderBox`] for BAL validation.
///
/// The upstream provider handle is `Send` but not `Clone` or `Sync`, so BAL workers share it
/// behind `Arc<Mutex<_>>` when a temporal database falls back to the provider.
#[derive(Clone)]
pub(crate) struct SharedStateProviderDatabase {
    provider: Arc<Mutex<StateProviderBox>>,
}

impl SharedStateProviderDatabase {
    pub(crate) fn new(provider: StateProviderBox) -> Self {
        Self {
            provider: Arc::new(Mutex::new(provider)),
        }
    }
}

impl std::fmt::Debug for SharedStateProviderDatabase {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SharedStateProviderDatabase")
            .finish_non_exhaustive()
    }
}

impl DatabaseRef for SharedStateProviderDatabase {
    type Error = ProviderError;

    fn basic_ref(&self, address: Address) -> Result<Option<revm::state::AccountInfo>, Self::Error> {
        let provider = self.provider.lock();
        Ok(AccountReader::basic_account(&**provider, &address)?.map(Into::into))
    }

    fn code_by_hash_ref(&self, code_hash: B256) -> Result<revm::state::Bytecode, Self::Error> {
        let provider = self.provider.lock();
        Ok(BytecodeReader::bytecode_by_hash(&**provider, &code_hash)?
            .unwrap_or_default()
            .0)
    }

    fn storage_ref(
        &self,
        address: Address,
        index: revm::primitives::StorageKey,
    ) -> Result<revm::primitives::StorageValue, Self::Error> {
        let provider = self.provider.lock();
        Ok(StateProvider::storage(&**provider, address, index.into())?.unwrap_or_default())
    }

    fn block_hash_ref(&self, number: u64) -> Result<B256, Self::Error> {
        let provider = self.provider.lock();
        Ok(BlockHashReader::block_hash(&**provider, number)?.unwrap_or_default())
    }
}

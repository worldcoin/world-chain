#![cfg_attr(not(any(test, feature = "test")), warn(unused_crate_dependencies))]

use ordering::WorldChainOrdering;
use reth::{api::FullNodeTypes, transaction_pool::{blobstore::DiskFileBlobStore, Pool, TransactionValidationTaskExecutor}};
use tx::WorldChainPooledTransaction;
use validator::WorldChainTransactionValidator;

pub mod bindings;
pub mod eip4337;
pub mod error;
pub mod noop;
pub mod ordering;
pub mod root;
pub mod tx;
pub mod validator;

#[cfg(any(feature = "test", test))]
pub mod mock;
#[cfg(any(feature = "test", test))]
pub mod test_utils;

/// Type alias for World Chain transaction pool
pub type WorldChainTransactionPool<Client, S, T = WorldChainPooledTransaction> = Pool<
    TransactionValidationTaskExecutor<WorldChainTransactionValidator<Client, T>>,
    WorldChainOrdering<WorldChainPooledTransaction>,
    S,
>;

/// A wrapper type with sensible defaults for the World Chain transaction pool.
pub type BasicWorldChainPool<N> = WorldChainTransactionPool<
    <N as FullNodeTypes>::Provider,
    DiskFileBlobStore,
    WorldChainPooledTransaction,
>;
#![warn(unused_crate_dependencies)]

use ordering::WorldChainOrdering;
use reth_node_api::FullNodeTypes;
use reth_transaction_pool::{
    Pool, TransactionValidationTaskExecutor, blobstore::DiskFileBlobStore,
};
use tx::WorldChainPooledTransaction;
use validator::WorldChainTransactionValidator;
use world_chain_evm::WorldChainEvmConfig;

pub mod bindings;
pub mod eip4337;
pub mod error;
pub mod noop;
pub mod ordering;
pub mod root;
pub mod tx;
pub mod validator;

/// Type alias for World Chain transaction pool
pub type WorldChainTransactionPool<
    Client,
    S,
    T = WorldChainPooledTransaction,
    Evm = WorldChainEvmConfig,
> = Pool<
    TransactionValidationTaskExecutor<WorldChainTransactionValidator<Client, T, Evm>>,
    WorldChainOrdering<T>,
    S,
>;

/// A wrapper type with sensible defaults for the World Chain transaction pool.
pub type BasicWorldChainPool<N, T = WorldChainPooledTransaction, Evm = WorldChainEvmConfig> =
    WorldChainTransactionPool<<N as FullNodeTypes>::Provider, DiskFileBlobStore, T, Evm>;

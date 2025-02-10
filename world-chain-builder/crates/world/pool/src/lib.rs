#![cfg_attr(not(any(test, feature = "test")), warn(unused_crate_dependencies))]

use ordering::WorldChainOrdering;
use reth::transaction_pool::{Pool, TransactionValidationTaskExecutor};
use tx::WorldChainPooledTransaction;
use validator::WorldChainTransactionValidator;

pub mod bindings;
pub mod builder;
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

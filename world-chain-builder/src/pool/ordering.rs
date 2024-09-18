use super::tx::WorldCoinPoolTransaction;
use crate::pbh::db::ValidatedPbhTransactionTable;
use reth_db::transaction::DbTx;
use reth_db::{Database as _, DatabaseEnv, DatabaseError};
use reth_transaction_pool::{CoinbaseTipOrdering, Priority, TransactionOrdering};
use revm_primitives::U256;
use std::sync::Arc;

/// Default ordering for the pool.
///
/// The transactions are ordered by their coinbase tip.
/// The higher the coinbase tip is, the higher the priority of the transaction.
#[derive(Debug)]
#[non_exhaustive]
// TODO: update to WorldChainOrdering
pub struct WorldCoinOrdering<T> {
    inner: CoinbaseTipOrdering<T>,
    database_env: Arc<DatabaseEnv>,
}

/// Ordering is automatically derived.
/// The ordering of fields here is important.
#[derive(Debug, Default, Clone, Ord, PartialOrd, Eq, PartialEq)]
pub struct WorldCoinPriority {
    is_pbh: bool,
    effective_tip_per_gas: Option<U256>,
}

impl<T> WorldCoinOrdering<T>
where
    T: WorldCoinPoolTransaction + 'static,
{
    /// Create a new [`WorldCoinOrdering`].
    pub fn new(database_env: Arc<DatabaseEnv>) -> Self {
        Self {
            inner: CoinbaseTipOrdering::default(),
            database_env,
        }
    }
    fn try_is_pbh(&self, transaction: &T) -> Result<bool, DatabaseError> {
        let db_tx = self.database_env.tx()?;
        Ok(db_tx
            .get::<ValidatedPbhTransactionTable>(*transaction.hash())?
            .is_some())
    }
    fn is_pbh(&self, transaction: &T) -> bool {
        self.try_is_pbh(transaction).unwrap_or_else(|error| {
            tracing::error!(?error, "Failed to load transaction from database");
            false
        })
    }
}

impl<T> TransactionOrdering for WorldCoinOrdering<T>
where
    T: WorldCoinPoolTransaction + 'static,
{
    type PriorityValue = WorldCoinPriority;
    type Transaction = T;

    fn priority(
        &self,
        transaction: &Self::Transaction,
        base_fee: u64,
    ) -> Priority<Self::PriorityValue> {
        let effective_tip_per_gas = transaction.effective_tip_per_gas(base_fee).map(U256::from);
        let is_pbh = self.is_pbh(transaction);
        Some(WorldCoinPriority {
            is_pbh,
            effective_tip_per_gas,
        })
        .into()
    }
}

impl<T> Clone for WorldCoinOrdering<T> {
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.clone(),
            database_env: self.database_env.clone(),
        }
    }
}

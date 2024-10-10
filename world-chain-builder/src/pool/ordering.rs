use super::tx::WorldChainPoolTransaction;
use crate::pbh::db::ValidatedPbhTransactionTable;
use reth::transaction_pool::{CoinbaseTipOrdering, Priority, TransactionOrdering};
use reth_db::transaction::DbTx;
use reth_db::{Database as _, DatabaseEnv, DatabaseError};
use revm_primitives::U256;
use std::sync::Arc;

/// Default ordering for the pool.
///
/// The transactions are ordered by their coinbase tip.
/// The higher the coinbase tip is, the higher the priority of the transaction.
#[derive(Debug)]
#[non_exhaustive]
pub struct WorldChainOrdering<T> {
    inner: CoinbaseTipOrdering<T>,
    pbh_db: Arc<DatabaseEnv>,
}

/// Ordering is automatically derived.
///
/// The ordering of fields here is important.
#[derive(Debug, Default, Clone, Ord, PartialOrd, Eq, PartialEq)]
pub struct WorldChainPriority {
    is_pbh: bool,
    effective_tip_per_gas: Option<U256>,
}

impl<T> WorldChainOrdering<T>
where
    T: WorldChainPoolTransaction + 'static,
{
    /// Create a new [`WorldChainOrdering`].
    pub fn new(database_env: Arc<DatabaseEnv>) -> Self {
        Self {
            inner: CoinbaseTipOrdering::default(),
            pbh_db: database_env,
        }
    }
    fn try_is_pbh(&self, transaction: &T) -> Result<bool, DatabaseError> {
        let db_tx = self.pbh_db.tx()?;
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

impl<T> TransactionOrdering for WorldChainOrdering<T>
where
    T: WorldChainPoolTransaction + 'static,
{
    type PriorityValue = WorldChainPriority;
    type Transaction = T;

    fn priority(
        &self,
        transaction: &Self::Transaction,
        base_fee: u64,
    ) -> Priority<Self::PriorityValue> {
        let effective_tip_per_gas = transaction.effective_tip_per_gas(base_fee).map(U256::from);
        let is_pbh = self.is_pbh(transaction);
        Some(WorldChainPriority {
            is_pbh,
            effective_tip_per_gas,
        })
        .into()
    }
}

impl<T> Clone for WorldChainOrdering<T> {
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.clone(),
            pbh_db: self.pbh_db.clone(),
        }
    }
}

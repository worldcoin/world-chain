use super::tx::WorldChainPoolTransaction;
use reth::transaction_pool::{CoinbaseTipOrdering, Priority, TransactionOrdering};
use revm_primitives::U256;

/// Default ordering for the pool.
///
/// The transactions are ordered by their coinbase tip.
/// The higher the coinbase tip is, the higher the priority of the transaction.
#[derive(Debug)]
pub struct WorldChainOrdering<T> {
    inner: CoinbaseTipOrdering<T>,
}

/// Ordering is automatically derived.
///
/// The ordering of fields here is important.
#[derive(Debug, Default, Clone, Ord, PartialOrd, Eq, PartialEq)]
pub struct WorldChainPriority {
    is_pbh: bool,
    effective_tip_per_gas: Option<U256>,
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

        Some(WorldChainPriority {
            is_pbh: transaction.pbh_payload().is_some(),
            effective_tip_per_gas,
        })
        .into()
    }
}

impl<T> Clone for WorldChainOrdering<T> {
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.clone(),
        }
    }
}

impl<T> Default for WorldChainOrdering<T> {
    fn default() -> Self {
        Self {
            inner: CoinbaseTipOrdering::default(),
        }
    }
}

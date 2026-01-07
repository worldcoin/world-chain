use std::sync::Arc;

use alloy_primitives::{Address, B256};
use revm::{
    DatabaseRef,
    database::BundleState,
    primitives::{StorageKey, StorageValue},
    state::{AccountInfo, Bytecode},
};

/// A layered database for executing a bundle over an underlying database.
#[derive(Clone, Debug)]
pub struct BundleDb<DB: DatabaseRef> {
    /// Layer 1: Underlying [`BundleState`] from prior flashblocks _or_ the pre-execution changes.
    pub bundle: Arc<BundleState>,
    /// Layer 2: The underlying database
    pub db: DB,
}

impl<DB: DatabaseRef> BundleDb<DB> {
    /// Creates a new [`BundleDb`] from the given bundle state and underlying database.
    pub fn new(db: DB, bundle: Arc<BundleState>) -> Self {
        Self { bundle, db }
    }
}

impl<DB: DatabaseRef> DatabaseRef for BundleDb<DB> {
    type Error = DB::Error;

    fn basic_ref(&self, address: Address) -> Result<Option<AccountInfo>, Self::Error> {
        if let Some(account) = self.bundle.account(&address) {
            // First try to get the account info directly from the bundle
            if let Some(info) = account.account_info() {
                tracing::trace!(
                    target: "flashblocks::bundle_db",
                    ?address,
                    balance = %info.balance,
                    nonce = %info.nonce,
                    "BundleDb account read from bundle info"
                );
                return Ok(Some(info));
            }

            // Account exists in bundle but info is None.
            // This can happen when only balance/storage was modified without full account load.
            // In this case, we need to reconstruct the account info from the underlying database
            // and apply changes from the bundle.
            //
            // Check if we have any actual changes in this account (balance changes are in original_info
            // or we can infer from the account's status)
            if let Some(original_info) = account.original_info.clone() {
                // The account had an original state, use that
                tracing::trace!(
                    target: "flashblocks::bundle_db",
                    ?address,
                    balance = %original_info.balance,
                    nonce = %original_info.nonce,
                    "BundleDb account read from bundle original_info (info was None)"
                );
                return Ok(Some(original_info));
            }
        }

        let result = self.db.basic_ref(address)?;
        tracing::trace!(
            target: "flashblocks::bundle_db",
            ?address,
            has_result = result.is_some(),
            balance = result.as_ref().map(|a| a.balance.to_string()).unwrap_or_default(),
            "BundleDb account read from fallback db"
        );
        Ok(result)
    }

    fn code_by_hash_ref(&self, code_hash: B256) -> Result<Bytecode, Self::Error> {
        if let Some(bytecode) = self.bundle.bytecode(&code_hash) {
            return Ok(bytecode);
        }

        self.db.code_by_hash_ref(code_hash)
    }

    fn storage_ref(
        &self,
        address: Address,
        index: StorageKey,
    ) -> Result<StorageValue, Self::Error> {
        if let Some(account) = self.bundle.account(&address)
            && let Some(storage) = account.storage_slot(index)
        {
            tracing::trace!(
                target: "flashblocks::bundle_db",
                ?address,
                ?index,
                value = ?storage,
                source = "bundle",
                "BundleDb storage read from bundle"
            );
            return Ok(storage);
        }

        let val = self.db.storage_ref(address, index)?;
        tracing::trace!(
            target: "flashblocks::bundle_db",
            ?address,
            ?index,
            value = ?val,
            source = "fallback_db",
            "BundleDb storage read from fallback db"
        );
        Ok(val)
    }

    fn block_hash_ref(&self, number: u64) -> Result<B256, Self::Error> {
        self.db.block_hash_ref(number)
    }
}

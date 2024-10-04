use std::path::Path;
use std::sync::Arc;

use alloy_primitives::TxHash;
use bytes::BufMut;
use reth_db::mdbx::{DatabaseArguments, DatabaseFlags};
use reth_db::{create_db, DatabaseError};
// TODO: maybe think about some sort of data retention policy for PBH transactions.
use reth_db::table::{Compress, Decompress, Table};
use revm_primitives::B256;
use serde::{Deserialize, Serialize};
use tracing::info;

/// Table for executed nullifiers.
#[derive(Debug, Clone, Default)]
pub struct ExecutedPbhNullifierTable;

impl Table for ExecutedPbhNullifierTable {
    const NAME: &'static str = "ExecutedPbhNullifiers";

    type Key = B256;

    type Value = EmptyValue;
}

/// Table to store PBH validated transactions along with their nullifiers.
#[derive(Debug, Clone, Default)]
pub struct ValidatedPbhTransactionTable;

impl Table for ValidatedPbhTransactionTable {
    const NAME: &'static str = "ValidatedPbhTransactions";

    type Key = TxHash;

    type Value = B256;
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct EmptyValue;

impl Decompress for EmptyValue {
    fn decompress(_: &[u8]) -> Result<Self, reth_db::DatabaseError> {
        Ok(Self)
    }
}

impl Compress for EmptyValue {
    type Compressed = Vec<u8>;

    fn compress_to_buf<B: BufMut + AsMut<[u8]>>(self, _buf: &mut B) {}
}

pub fn load_world_chain_db(
    data_dir: &Path,
    clear_nullifiers: bool,
) -> Result<Arc<reth_db::DatabaseEnv>, eyre::eyre::Error> {
    let path = data_dir.join("world-chain");
    if clear_nullifiers {
        info!(?path, "Clearing semaphore-nullifiers database");
        // delete the directory
        std::fs::remove_dir_all(&path)?;
    }
    info!(?path, "Opening semaphore-nullifiers database");
    let db = create_db(path, DatabaseArguments::default())?;

    let tx = db
        .begin_rw_txn()
        .map_err(|e| DatabaseError::InitTx(e.into()))?;

    tx.create_db(
        Some(ExecutedPbhNullifierTable::NAME),
        DatabaseFlags::default(),
    )
    .map_err(|e| DatabaseError::CreateTable(e.into()))?;
    tx.create_db(
        Some(ValidatedPbhTransactionTable::NAME),
        DatabaseFlags::default(),
    )
    .map_err(|e| DatabaseError::CreateTable(e.into()))?;

    tx.commit().map_err(|e| DatabaseError::Commit(e.into()))?;

    Ok(Arc::new(db))
}

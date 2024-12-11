use std::fmt;
use std::path::Path;
use std::sync::Arc;

use bytes::BufMut;
use reth_db::cursor::DbCursorRW;
use reth_db::mdbx::tx::Tx;
use reth_db::mdbx::{DatabaseArguments, DatabaseFlags, RW};
use reth_db::table::{Compress, Decompress, Table};
use reth_db::transaction::DbTxMut;
use reth_db::{create_db, tables, DatabaseError, TableType, TableViewer};
use reth_db_api;
use revm_primitives::{FixedBytes, B256};
use semaphore::Field;
use serde::{Deserialize, Serialize};
use tracing::info;

tables! {
    /// Table to store PBH validated transactions along with their nullifiers.
    ///
    /// When a trasnaction is validated before being inserted into the pool,
    /// a mapping is created from the transaction hash to the nullifier here.
    /// This is primarily used as a caching mechanism to avoid certain types of
    /// DoS attacks.
    table ValidatedPbhTransaction<Key = B256, Value = EmptyValue>;
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

/// Set the store the nullifier for a tx after it
/// has been included in the block
/// don't forget to call db_tx.commit() at the very end
pub fn set_pbh_nullifier(db_tx: &Tx<RW>, nullifier: Field) -> Result<(), DatabaseError> {
    let bytes: FixedBytes<32> = nullifier.into();
    let mut cursor = db_tx.cursor_write::<ValidatedPbhTransaction>()?;
    cursor.insert(bytes, EmptyValue)?;
    Ok(())
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
        Some(ValidatedPbhTransaction::NAME),
        DatabaseFlags::default(),
    )
    .map_err(|e| DatabaseError::CreateTable(e.into()))?;

    tx.commit().map_err(|e| DatabaseError::Commit(e.into()))?;

    Ok(Arc::new(db))
}

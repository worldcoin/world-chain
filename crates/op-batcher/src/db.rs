//! MDBX-backed persistence for the OP Batcher.
//!
//! Mirrors the proposer crate's `db` module. This store is a **crash-recovery
//! aid only** — the authoritative batching cursor is the derivation-backed L2
//! safe head read from the node (see `BATCHER_SPEC.md` §11/§14), not this store.
//!
//! Schema:
//! * `head` (single-entry, key `0x00`): the last ExEx-processed L2 head
//!   `(block_number, block_hash)`, encoded as JSON.
//! * `last_batch` (single-entry, key `0x00`): the last successfully confirmed
//!   batch submission, encoded as JSON. Observability + fast-restart hint only.

use std::path::{Path, PathBuf};

use alloy_primitives::B256;
use parking_lot::Mutex;
use reth_libmdbx::{DatabaseFlags, Environment, Geometry, WriteFlags};
use serde::{Deserialize, Serialize};
use thiserror::Error;

const TABLE_LAST_BATCH: &str = "last_batch";
const TABLE_HEAD: &str = "head";
const SOLE_KEY: &[u8] = &[0u8];

/// Snapshot of the last successfully confirmed batch submission.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct StoredBatch {
    /// First L2 block in the submitted channel.
    pub l2_start: u64,
    /// Last L2 block in the submitted channel.
    pub l2_end: u64,
    /// Channel ID (16 bytes) the batch belonged to.
    pub channel_id: [u8; 16],
    /// L1 transaction hash of the (last) confirmed frame.
    pub l1_tx_hash: B256,
    /// L1 block the tx was included in.
    pub l1_block_number: u64,
    /// Unix seconds at which the batch was confirmed.
    pub at_unix: u64,
}

/// Snapshot of the last L2 head the ExEx finished processing.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub struct StoredHead {
    pub block_number: u64,
    pub block_hash: B256,
}

/// MDBX-backed batcher store.
pub struct BatcherStore {
    env: Environment,
    path: PathBuf,
    write_lock: Mutex<()>,
}

impl BatcherStore {
    /// Open (creating if absent) the MDBX environment at `dir`.
    pub fn open(dir: &Path) -> Result<Self, BatcherStoreError> {
        std::fs::create_dir_all(dir).map_err(|e| BatcherStoreError::Io(e.to_string()))?;
        let mut builder = Environment::builder();
        builder.set_max_dbs(8);
        builder.set_geometry(Geometry {
            size: Some(0..(1usize << 30)),
            growth_step: Some(64 * 1024 * 1024),
            shrink_threshold: None,
            page_size: None,
        });
        let env = builder.open(dir).map_err(map_mdbx)?;
        {
            let tx = env.begin_rw_txn().map_err(map_mdbx)?;
            tx.create_db(Some(TABLE_LAST_BATCH), DatabaseFlags::default())
                .map_err(map_mdbx)?;
            tx.create_db(Some(TABLE_HEAD), DatabaseFlags::default())
                .map_err(map_mdbx)?;
            tx.commit().map_err(map_mdbx)?;
        }
        Ok(Self {
            env,
            path: dir.to_path_buf(),
            write_lock: Mutex::new(()),
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Returns the last confirmed batch, if any.
    pub fn last_batch(&self) -> Result<Option<StoredBatch>, BatcherStoreError> {
        self.get_json(TABLE_LAST_BATCH)
    }

    /// Persist the given batch as the latest confirmed one.
    pub fn put_last_batch(&self, batch: &StoredBatch) -> Result<(), BatcherStoreError> {
        self.put_json(TABLE_LAST_BATCH, batch)
    }

    /// Returns the last L2 head the ExEx finished processing.
    pub fn head(&self) -> Result<Option<StoredHead>, BatcherStoreError> {
        self.get_json(TABLE_HEAD)
    }

    /// Persist the ExEx head.
    pub fn put_head(&self, head: StoredHead) -> Result<(), BatcherStoreError> {
        self.put_json(TABLE_HEAD, &head)
    }

    fn get_json<T: for<'de> Deserialize<'de>>(
        &self,
        table: &str,
    ) -> Result<Option<T>, BatcherStoreError> {
        let tx = self.env.begin_ro_txn().map_err(map_mdbx)?;
        let db = tx.open_db(Some(table)).map_err(map_mdbx)?;
        let raw: Option<Vec<u8>> = tx.get::<Vec<u8>>(db.dbi(), SOLE_KEY).map_err(map_mdbx)?;
        match raw {
            Some(bytes) => serde_json::from_slice(&bytes)
                .map(Some)
                .map_err(|e| BatcherStoreError::Codec(format!("decode {table}: {e}"))),
            None => Ok(None),
        }
    }

    fn put_json<T: Serialize>(&self, table: &str, value: &T) -> Result<(), BatcherStoreError> {
        let _g = self.write_lock.lock();
        let bytes = serde_json::to_vec(value)
            .map_err(|e| BatcherStoreError::Codec(format!("encode {table}: {e}")))?;
        let tx = self.env.begin_rw_txn().map_err(map_mdbx)?;
        let db = tx.open_db(Some(table)).map_err(map_mdbx)?;
        tx.put(db.dbi(), SOLE_KEY, &bytes, WriteFlags::default())
            .map_err(map_mdbx)?;
        tx.commit().map_err(map_mdbx)?;
        Ok(())
    }
}

fn map_mdbx(err: reth_libmdbx::Error) -> BatcherStoreError {
    BatcherStoreError::Mdbx(err.to_string())
}

#[derive(Debug, Error)]
pub enum BatcherStoreError {
    #[error("mdbx error: {0}")]
    Mdbx(String),
    #[error("filesystem error: {0}")]
    Io(String),
    #[error("codec error: {0}")]
    Codec(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn put_and_load() {
        let tmp = tempfile::tempdir().unwrap();
        let store = BatcherStore::open(tmp.path()).unwrap();
        assert!(store.last_batch().unwrap().is_none());

        let b = StoredBatch {
            l2_start: 10,
            l2_end: 20,
            channel_id: [7u8; 16],
            l1_tx_hash: B256::repeat_byte(0xcd),
            l1_block_number: 999,
            at_unix: 1_700_000_000,
        };
        store.put_last_batch(&b).unwrap();
        assert_eq!(store.last_batch().unwrap().unwrap(), b);

        let h = StoredHead {
            block_number: 42,
            block_hash: B256::repeat_byte(0xee),
        };
        store.put_head(h).unwrap();
        assert_eq!(store.head().unwrap().unwrap(), h);
    }
}

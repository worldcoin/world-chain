//! MDBX-backed persistence for the OP Proposer.
//!
//! Mainly used as a crash-recovery aid: on restart we can answer "what was
//! the last proposal I successfully submitted?" without having to walk the
//! DGF on L1.
//!
//! Schema:
//!
//! * `last_proposal` (single-entry table): stored under key `0x00`, encodes
//!   the last successfully submitted proposal as JSON.
//! * `head` (single-entry table): stored under key `0x00`, encodes the last
//!   ExEx-processed L2 head (`(block_number, block_hash)`).

use std::path::{Path, PathBuf};

use alloy_primitives::{Address, B256};
use parking_lot::Mutex;
use reth_libmdbx::{DatabaseFlags, Environment, Geometry, WriteFlags};
use serde::{Deserialize, Serialize};
use thiserror::Error;

const TABLE_LAST_PROPOSAL: &str = "last_proposal";
const TABLE_HEAD: &str = "head";
const SOLE_KEY: &[u8] = &[0u8];

/// Snapshot of the last successfully submitted proposal.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct StoredProposal {
    pub game_type: u32,
    pub sequence_num: u64,
    pub root: B256,
    pub tx_hash: B256,
    pub l1_block_number: u64,
    pub l1_block_hash: B256,
    pub proposer: Address,
    /// Unix seconds at which the proposal was committed.
    pub at_unix: u64,
}

/// Snapshot of the last L2 head the ExEx finished processing.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub struct StoredHead {
    pub block_number: u64,
    pub block_hash: B256,
}

/// MDBX-backed proposer store.
pub struct ProposerStore {
    env: Environment,
    path: PathBuf,
    // Serialise writes; libmdbx allows one writer at a time but this avoids
    // the open-write-txn races when the proposer loop and ExEx commit handler
    // both want to persist.
    write_lock: Mutex<()>,
}

impl ProposerStore {
    /// Open (creating if absent) the MDBX environment at `dir`.
    pub fn open(dir: &Path) -> Result<Self, ProposerStoreError> {
        std::fs::create_dir_all(dir).map_err(|e| ProposerStoreError::Io(e.to_string()))?;
        let mut builder = Environment::builder();
        builder.set_max_dbs(8);
        builder.set_geometry(Geometry {
            size: Some(0..(1usize << 30)),
            growth_step: Some(64 * 1024 * 1024),
            shrink_threshold: None,
            page_size: None,
        });
        let env = builder.open(dir).map_err(map_mdbx)?;
        // Create the tables upfront so opening a tx later finds them.
        {
            let tx = env.begin_rw_txn().map_err(map_mdbx)?;
            tx.create_db(Some(TABLE_LAST_PROPOSAL), DatabaseFlags::default())
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

    /// Returns the last submitted proposal, if any.
    pub fn last_proposal(&self) -> Result<Option<StoredProposal>, ProposerStoreError> {
        self.get_json(TABLE_LAST_PROPOSAL)
    }

    /// Persist the given proposal as the latest one.
    pub fn put_last_proposal(&self, proposal: &StoredProposal) -> Result<(), ProposerStoreError> {
        self.put_json(TABLE_LAST_PROPOSAL, proposal)
    }

    /// Returns the last L2 head the ExEx finished processing.
    pub fn head(&self) -> Result<Option<StoredHead>, ProposerStoreError> {
        self.get_json(TABLE_HEAD)
    }

    /// Persist the ExEx head.
    pub fn put_head(&self, head: StoredHead) -> Result<(), ProposerStoreError> {
        self.put_json(TABLE_HEAD, &head)
    }

    fn get_json<T: for<'de> Deserialize<'de>>(
        &self,
        table: &str,
    ) -> Result<Option<T>, ProposerStoreError> {
        let tx = self.env.begin_ro_txn().map_err(map_mdbx)?;
        let db = tx.open_db(Some(table)).map_err(map_mdbx)?;
        let raw: Option<Vec<u8>> = tx.get::<Vec<u8>>(db.dbi(), SOLE_KEY).map_err(map_mdbx)?;
        match raw {
            Some(bytes) => serde_json::from_slice(&bytes)
                .map(Some)
                .map_err(|e| ProposerStoreError::Codec(format!("decode {table}: {e}"))),
            None => Ok(None),
        }
    }

    fn put_json<T: Serialize>(&self, table: &str, value: &T) -> Result<(), ProposerStoreError> {
        let _g = self.write_lock.lock();
        let bytes = serde_json::to_vec(value)
            .map_err(|e| ProposerStoreError::Codec(format!("encode {table}: {e}")))?;
        let tx = self.env.begin_rw_txn().map_err(map_mdbx)?;
        let db = tx.open_db(Some(table)).map_err(map_mdbx)?;
        tx.put(db.dbi(), SOLE_KEY, &bytes, WriteFlags::default())
            .map_err(map_mdbx)?;
        tx.commit().map_err(map_mdbx)?;
        Ok(())
    }
}

fn map_mdbx(err: reth_libmdbx::Error) -> ProposerStoreError {
    ProposerStoreError::Mdbx(err.to_string())
}

#[derive(Debug, Error)]
pub enum ProposerStoreError {
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

    fn proposal() -> StoredProposal {
        StoredProposal {
            game_type: 0,
            sequence_num: 1234,
            root: B256::repeat_byte(0xab),
            tx_hash: B256::repeat_byte(0xcd),
            l1_block_number: 999,
            l1_block_hash: B256::repeat_byte(0x11),
            proposer: Address::repeat_byte(0x22),
            at_unix: 1_700_000_000,
        }
    }

    #[test]
    fn put_and_load_last_proposal() {
        let tmp = tempfile::tempdir().unwrap();
        let store = ProposerStore::open(tmp.path()).unwrap();
        assert!(store.last_proposal().unwrap().is_none());
        let p = proposal();
        store.put_last_proposal(&p).unwrap();
        assert_eq!(store.last_proposal().unwrap().unwrap(), p);

        let h = StoredHead {
            block_number: 42,
            block_hash: B256::repeat_byte(0xee),
        };
        store.put_head(h).unwrap();
        assert_eq!(store.head().unwrap().unwrap(), h);
    }
}

//! MDBX-backed persistence for the withdrawal cacher.
//!
//! Mirrors the [`ProposerStore`](crate::db::ProposerStore) style: a libmdbx
//! [`Environment`], tables created up front, JSON-encoded values, a
//! [`parking_lot::Mutex`] write lock, and a `map_mdbx` error mapper.
//!
//! Implements the cacher's persistence contract from
//! [`wips/wip-1006.md`][wip] §Cacher. The store is reorg-aware and idempotent.
//!
//! Schema:
//!
//! * `withdrawals`: key = 32-byte `withdrawalHash` → JSON [`WithdrawalRecord`].
//! * `withdrawals_by_block`: key = 8-byte big-endian L2 block number → JSON
//!   `Vec<B256>` of the withdrawal hashes emitted in that block. This index
//!   lets [`prune_range`](WithdrawalStore::prune_range) resolve the hashes to
//!   touch on reorg without scanning the whole `withdrawals` table. Big-endian
//!   keys keep the index lexicographically ordered by block number.
//! * `head` (single-entry): the cache tip, like
//!   [`StoredHead`](crate::db::StoredHead).
//!
//! [wip]: ../../../wips/wip-1006.md

use std::{
    ops::RangeInclusive,
    path::{Path, PathBuf},
};

use alloy_primitives::B256;
use parking_lot::Mutex;
use reth_libmdbx::{
    DatabaseFlags, Environment, Geometry, RW, Transaction, TransactionKind, WriteFlags,
    ffi::MDBX_dbi,
};
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::{
    db::StoredHead,
    withdrawal::{WithdrawalRecord, WithdrawalStatus},
};

const TABLE_WITHDRAWALS: &str = "withdrawals";
const TABLE_WITHDRAWALS_BY_BLOCK: &str = "withdrawals_by_block";
const TABLE_HEAD: &str = "head";
const SOLE_KEY: &[u8] = &[0u8];

/// Tally of cached withdrawals grouped by lifecycle status.
///
/// Seam for the future admin RPC (`wips/wip-1006.md` §Admin RPC).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct StatusCounts {
    pub cached: u64,
    pub proven: u64,
    pub finalized: u64,
    pub orphaned: u64,
}

impl StatusCounts {
    fn bump(&mut self, status: WithdrawalStatus) {
        match status {
            WithdrawalStatus::Cached => self.cached += 1,
            WithdrawalStatus::Proven => self.proven += 1,
            WithdrawalStatus::Finalized => self.finalized += 1,
            WithdrawalStatus::Orphaned => self.orphaned += 1,
        }
    }
}

/// MDBX-backed withdrawal store.
pub struct WithdrawalStore {
    env: Environment,
    path: PathBuf,
    // Serialise writes: libmdbx permits a single writer, and the cacher's
    // commit and prune paths both mutate two tables atomically within one rw
    // txn. The lock keeps those multi-table updates from racing.
    write_lock: Mutex<()>,
}

impl WithdrawalStore {
    /// Open (creating if absent) the MDBX environment at `dir`.
    pub fn open(dir: &Path) -> Result<Self, WithdrawalStoreError> {
        std::fs::create_dir_all(dir).map_err(|e| WithdrawalStoreError::Io(e.to_string()))?;
        let mut builder = Environment::builder();
        builder.set_max_dbs(8);
        builder.set_geometry(Geometry {
            size: Some(0..(1usize << 30)),
            growth_step: Some(64 * 1024 * 1024),
            shrink_threshold: None,
            page_size: None,
        });
        let env = builder.open(dir).map_err(map_mdbx)?;
        // Create the tables up front so opening a tx later finds them.
        {
            let tx = env.begin_rw_txn().map_err(map_mdbx)?;
            tx.create_db(Some(TABLE_WITHDRAWALS), DatabaseFlags::default())
                .map_err(map_mdbx)?;
            tx.create_db(Some(TABLE_WITHDRAWALS_BY_BLOCK), DatabaseFlags::default())
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

    /// Persist a freshly observed withdrawal.
    ///
    /// Idempotent per [`wips/wip-1006.md`][wip] §Cacher: re-observing an
    /// already-cached `withdrawalHash` MUST NOT create a duplicate, and an
    /// existing record is never overwritten — in particular a record already
    /// proven/finalized on L1 ([`WithdrawalStatus::is_settled_on_l1`]) is never
    /// downgraded back to `Cached`. The incoming record is simply dropped when
    /// the hash is already present.
    ///
    /// The block index is updated only when the record is newly inserted, so
    /// re-delivery of the same notification range never duplicates index
    /// entries either.
    ///
    /// [wip]: ../../../wips/wip-1006.md
    pub fn put_withdrawal(&self, record: &WithdrawalRecord) -> Result<(), WithdrawalStoreError> {
        let _g = self.write_lock.lock();
        let tx = self.env.begin_rw_txn().map_err(map_mdbx)?;
        let withdrawals = tx.open_db(Some(TABLE_WITHDRAWALS)).map_err(map_mdbx)?;

        let key = record.withdrawal_hash;
        let existing: Option<WithdrawalRecord> =
            get_json(&tx, withdrawals.dbi(), key.as_slice(), TABLE_WITHDRAWALS)?;
        if existing.is_some() {
            // Already present: drop the tx (no-op) and retain the stored
            // record, preserving any Proven/Finalized status.
            drop(tx);
            return Ok(());
        }

        // New record: write it and append its hash to the block index.
        put_json(
            &tx,
            withdrawals.dbi(),
            key.as_slice(),
            record,
            TABLE_WITHDRAWALS,
        )?;

        let by_block = tx
            .open_db(Some(TABLE_WITHDRAWALS_BY_BLOCK))
            .map_err(map_mdbx)?;
        let block_key = record.l2_block_number.to_be_bytes();
        let mut hashes: Vec<B256> =
            get_json(&tx, by_block.dbi(), &block_key, TABLE_WITHDRAWALS_BY_BLOCK)?
                .unwrap_or_default();
        if !hashes.contains(&key) {
            hashes.push(key);
        }
        put_json(
            &tx,
            by_block.dbi(),
            &block_key,
            &hashes,
            TABLE_WITHDRAWALS_BY_BLOCK,
        )?;

        tx.commit().map_err(map_mdbx)?;
        Ok(())
    }

    /// Returns the cached withdrawal for `hash`, if any.
    pub fn get_withdrawal(
        &self,
        hash: B256,
    ) -> Result<Option<WithdrawalRecord>, WithdrawalStoreError> {
        let tx = self.env.begin_ro_txn().map_err(map_mdbx)?;
        let db = tx.open_db(Some(TABLE_WITHDRAWALS)).map_err(map_mdbx)?;
        get_json(&tx, db.dbi(), hash.as_slice(), TABLE_WITHDRAWALS)
    }

    /// Apply the reorg rule from [`wips/wip-1006.md`][wip] §Cacher over the
    /// (inclusive) reverted L2 block range:
    ///
    /// * a `Cached` record whose origin block falls in `range` is deleted, and
    /// * a `Proven`/`Finalized` record over the same range is **retained** but
    ///   set to [`WithdrawalStatus::Orphaned`] (audit trail).
    ///
    /// The block-index entries for the range are removed; orphaned hashes are
    /// dropped from the index since they no longer map to a canonical block.
    ///
    /// [wip]: ../../../wips/wip-1006.md
    pub fn prune_range(&self, range: RangeInclusive<u64>) -> Result<(), WithdrawalStoreError> {
        let _g = self.write_lock.lock();
        let tx = self.env.begin_rw_txn().map_err(map_mdbx)?;
        let withdrawals = tx.open_db(Some(TABLE_WITHDRAWALS)).map_err(map_mdbx)?;
        let by_block = tx
            .open_db(Some(TABLE_WITHDRAWALS_BY_BLOCK))
            .map_err(map_mdbx)?;

        for block in range {
            let block_key = block.to_be_bytes();
            let hashes: Vec<B256> =
                match get_json(&tx, by_block.dbi(), &block_key, TABLE_WITHDRAWALS_BY_BLOCK)? {
                    Some(h) => h,
                    None => continue,
                };
            for hash in hashes {
                let record: Option<WithdrawalRecord> =
                    get_json(&tx, withdrawals.dbi(), hash.as_slice(), TABLE_WITHDRAWALS)?;
                let Some(mut record) = record else { continue };
                if record.status.is_settled_on_l1() {
                    // Retain for auditing, mark orphaned.
                    record.status = WithdrawalStatus::Orphaned;
                    put_json(
                        &tx,
                        withdrawals.dbi(),
                        hash.as_slice(),
                        &record,
                        TABLE_WITHDRAWALS,
                    )?;
                } else {
                    tx.del(withdrawals.dbi(), hash.as_slice(), None)
                        .map_err(map_mdbx)?;
                }
            }
            // The index entry no longer points at canonical state either way.
            tx.del(by_block.dbi(), block_key, None).map_err(map_mdbx)?;
        }

        tx.commit().map_err(map_mdbx)?;
        Ok(())
    }

    /// Collect every withdrawal currently in [`WithdrawalStatus::Cached`].
    ///
    /// Seam for the relayer driver, which polls cached withdrawals for
    /// prove-eligibility. Returns an owned `Vec` rather than a borrowing
    /// iterator to keep the read transaction's lifetime contained.
    pub fn cached_withdrawals(&self) -> Result<Vec<WithdrawalRecord>, WithdrawalStoreError> {
        self.collect_where(|r| r.status == WithdrawalStatus::Cached)
    }

    /// Counts of cached withdrawals grouped by status.
    ///
    /// Seam for the admin RPC (`wips/wip-1006.md` §Admin RPC).
    pub fn counts_by_status(&self) -> Result<StatusCounts, WithdrawalStoreError> {
        let mut counts = StatusCounts::default();
        for record in self.collect_where(|_| true)? {
            counts.bump(record.status);
        }
        Ok(counts)
    }

    /// Returns the last L2 head the cacher finished processing.
    pub fn head(&self) -> Result<Option<StoredHead>, WithdrawalStoreError> {
        let tx = self.env.begin_ro_txn().map_err(map_mdbx)?;
        let db = tx.open_db(Some(TABLE_HEAD)).map_err(map_mdbx)?;
        get_json(&tx, db.dbi(), SOLE_KEY, TABLE_HEAD)
    }

    /// Persist the cacher head.
    pub fn put_head(&self, head: StoredHead) -> Result<(), WithdrawalStoreError> {
        let _g = self.write_lock.lock();
        let tx = self.env.begin_rw_txn().map_err(map_mdbx)?;
        let db = tx.open_db(Some(TABLE_HEAD)).map_err(map_mdbx)?;
        put_json(&tx, db.dbi(), SOLE_KEY, &head, TABLE_HEAD)?;
        tx.commit().map_err(map_mdbx)?;
        Ok(())
    }

    /// Walk the `withdrawals` table, collecting records matching `pred`.
    fn collect_where(
        &self,
        pred: impl Fn(&WithdrawalRecord) -> bool,
    ) -> Result<Vec<WithdrawalRecord>, WithdrawalStoreError> {
        let tx = self.env.begin_ro_txn().map_err(map_mdbx)?;
        let db = tx.open_db(Some(TABLE_WITHDRAWALS)).map_err(map_mdbx)?;
        let mut cursor = tx.cursor(db.dbi()).map_err(map_mdbx)?;
        let mut out = Vec::new();
        // `iter_start` walks every (key, value) pair from the first key.
        for item in cursor.iter_start::<Vec<u8>, Vec<u8>>() {
            let (_key, bytes) = item.map_err(map_mdbx)?;
            let record: WithdrawalRecord = serde_json::from_slice(&bytes)
                .map_err(|e| WithdrawalStoreError::Codec(format!("decode withdrawals: {e}")))?;
            if pred(&record) {
                out.push(record);
            }
        }
        Ok(out)
    }

    /// Snapshot of the block index. Test/diagnostics helper that proves the
    /// block index stays consistent with the primary table.
    #[cfg(test)]
    fn block_index_snapshot(
        &self,
    ) -> Result<std::collections::BTreeMap<u64, Vec<B256>>, WithdrawalStoreError> {
        let tx = self.env.begin_ro_txn().map_err(map_mdbx)?;
        let db = tx
            .open_db(Some(TABLE_WITHDRAWALS_BY_BLOCK))
            .map_err(map_mdbx)?;
        let mut cursor = tx.cursor(db.dbi()).map_err(map_mdbx)?;
        let mut out = std::collections::BTreeMap::new();
        for item in cursor.iter_start::<Vec<u8>, Vec<u8>>() {
            let (key, bytes) = item.map_err(map_mdbx)?;
            let mut arr = [0u8; 8];
            if key.len() == 8 {
                arr.copy_from_slice(&key);
            }
            let block = u64::from_be_bytes(arr);
            let hashes: Vec<B256> = serde_json::from_slice(&bytes).map_err(|e| {
                WithdrawalStoreError::Codec(format!("decode withdrawals_by_block: {e}"))
            })?;
            out.insert(block, hashes);
        }
        Ok(out)
    }
}

fn get_json<T, K>(
    tx: &Transaction<K>,
    dbi: MDBX_dbi,
    key: &[u8],
    table: &str,
) -> Result<Option<T>, WithdrawalStoreError>
where
    T: for<'de> Deserialize<'de>,
    K: TransactionKind,
{
    let raw: Option<Vec<u8>> = tx.get::<Vec<u8>>(dbi, key).map_err(map_mdbx)?;
    match raw {
        Some(bytes) => serde_json::from_slice(&bytes)
            .map(Some)
            .map_err(|e| WithdrawalStoreError::Codec(format!("decode {table}: {e}"))),
        None => Ok(None),
    }
}

fn put_json<T: Serialize>(
    tx: &Transaction<RW>,
    dbi: MDBX_dbi,
    key: &[u8],
    value: &T,
    table: &str,
) -> Result<(), WithdrawalStoreError> {
    let bytes = serde_json::to_vec(value)
        .map_err(|e| WithdrawalStoreError::Codec(format!("encode {table}: {e}")))?;
    tx.put(dbi, key, &bytes, WriteFlags::default())
        .map_err(map_mdbx)?;
    Ok(())
}

fn map_mdbx(err: reth_libmdbx::Error) -> WithdrawalStoreError {
    WithdrawalStoreError::Mdbx(err.to_string())
}

#[derive(Debug, Error)]
pub enum WithdrawalStoreError {
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
    use crate::withdrawal::{WithdrawalTransaction, message_slot, withdrawal_hash};
    use alloy_primitives::{Address, Bytes, U256};

    fn record(seed: u8, block: u64, status: WithdrawalStatus) -> WithdrawalRecord {
        let tx = WithdrawalTransaction {
            nonce: U256::from(seed),
            sender: Address::repeat_byte(seed),
            target: Address::repeat_byte(seed.wrapping_add(1)),
            value: U256::from(seed as u64 * 10),
            gas_limit: U256::from(21_000u64),
            data: Bytes::new(),
        };
        let hash = withdrawal_hash(&tx);
        WithdrawalRecord {
            withdrawal_hash: hash,
            tx,
            l2_block_number: block,
            l2_block_hash: B256::repeat_byte(seed),
            message_slot: message_slot(hash),
            status,
            observed_at_unix: 1_700_000_000,
        }
    }

    #[test]
    fn put_get_and_head() {
        let tmp = tempfile::tempdir().unwrap();
        let store = WithdrawalStore::open(tmp.path()).unwrap();

        let r = record(1, 100, WithdrawalStatus::Cached);
        assert!(store.get_withdrawal(r.withdrawal_hash).unwrap().is_none());
        store.put_withdrawal(&r).unwrap();
        assert_eq!(store.get_withdrawal(r.withdrawal_hash).unwrap().unwrap(), r);

        let head = StoredHead {
            block_number: 100,
            block_hash: B256::repeat_byte(0xee),
        };
        store.put_head(head).unwrap();
        assert_eq!(store.head().unwrap().unwrap(), head);
    }

    /// Re-observing the same hash MUST NOT duplicate and MUST NOT downgrade a
    /// settled record back to `Cached`.
    #[test]
    fn idempotent_no_duplicate_no_downgrade() {
        let tmp = tempfile::tempdir().unwrap();
        let store = WithdrawalStore::open(tmp.path()).unwrap();

        let cached = record(2, 200, WithdrawalStatus::Cached);
        store.put_withdrawal(&cached).unwrap();
        // Re-deliver the same cached record.
        store.put_withdrawal(&cached).unwrap();

        let counts = store.counts_by_status().unwrap();
        assert_eq!(counts.cached, 1, "no duplicate insert");

        // Block index has exactly one hash for block 200.
        let idx = store.block_index_snapshot().unwrap();
        assert_eq!(idx.get(&200).unwrap().len(), 1);

        // No-downgrade guard: seed a Proven record (as the relayer would),
        // then re-observe the same withdrawal as a fresh `Cached` record (as
        // the cacher would on a re-delivered range). The stored status must
        // stay `Proven`.
        let proven = record(9, 250, WithdrawalStatus::Proven);
        store.put_withdrawal(&proven).unwrap();
        let mut as_cached = proven.clone();
        as_cached.status = WithdrawalStatus::Cached;
        store.put_withdrawal(&as_cached).unwrap();
        let stored = store
            .get_withdrawal(proven.withdrawal_hash)
            .unwrap()
            .unwrap();
        assert_eq!(
            stored.status,
            WithdrawalStatus::Proven,
            "settled record must not be downgraded"
        );
    }

    /// Reorg rule: `Cached` records in the reverted range are removed; a
    /// `Proven` record over the same range is retained and marked `Orphaned`.
    #[test]
    fn prune_range_reorg_rule() {
        let tmp = tempfile::tempdir().unwrap();
        let store = WithdrawalStore::open(tmp.path()).unwrap();

        let cached = record(3, 300, WithdrawalStatus::Cached);
        let proven = record(4, 301, WithdrawalStatus::Proven);
        let untouched = record(5, 400, WithdrawalStatus::Cached);
        store.put_withdrawal(&cached).unwrap();
        store.put_withdrawal(&proven).unwrap();
        store.put_withdrawal(&untouched).unwrap();

        store.prune_range(300..=301).unwrap();

        // Cached @300 removed.
        assert!(
            store
                .get_withdrawal(cached.withdrawal_hash)
                .unwrap()
                .is_none()
        );
        // Proven @301 retained, now Orphaned.
        let kept = store
            .get_withdrawal(proven.withdrawal_hash)
            .unwrap()
            .unwrap();
        assert_eq!(kept.status, WithdrawalStatus::Orphaned);
        // Out-of-range record untouched.
        assert!(
            store
                .get_withdrawal(untouched.withdrawal_hash)
                .unwrap()
                .is_some()
        );

        // Index for pruned blocks cleared; block 400 still present.
        let idx = store.block_index_snapshot().unwrap();
        assert!(!idx.contains_key(&300));
        assert!(!idx.contains_key(&301));
        assert!(idx.contains_key(&400));

        let counts = store.counts_by_status().unwrap();
        assert_eq!(counts.cached, 1);
        assert_eq!(counts.orphaned, 1);
    }

    #[test]
    fn cached_withdrawals_seam() {
        let tmp = tempfile::tempdir().unwrap();
        let store = WithdrawalStore::open(tmp.path()).unwrap();
        store
            .put_withdrawal(&record(6, 500, WithdrawalStatus::Cached))
            .unwrap();
        store
            .put_withdrawal(&record(7, 501, WithdrawalStatus::Cached))
            .unwrap();
        let cached = store.cached_withdrawals().unwrap();
        assert_eq!(cached.len(), 2);
        assert!(cached.iter().all(|r| r.status == WithdrawalStatus::Cached));
    }
}

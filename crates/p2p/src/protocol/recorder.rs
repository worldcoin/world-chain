//! Best-effort persistence for accepted flashblocks.
//!
//! The recorder uses a separate libmdbx database environment from the node's
//! canonical state database. It stores the exact [`FlashblocksPayloadV1`] RLP
//! bytes as the source of truth and keeps a small secondary index from block
//! number to payload id for later benchmark replay.

use alloy_primitives::Bytes;
use reth_db::{
    Database, DatabaseEnv,
    mdbx::{DatabaseArguments, create_db},
};
use reth_db_api::{
    DatabaseError,
    transaction::{DbTx, DbTxMut},
};
use std::{fs, path::PathBuf, time::Duration};
use tokio::sync::mpsc;
use tracing::{debug, error, warn};
use world_chain_primitives::primitives::FlashblocksPayloadV1;

const STORE_CHANNEL_CAPACITY: usize = 1_024;
const DB_RETRY_DELAY: Duration = Duration::from_secs(1);

type RecorderResult<T> = Result<T, FlashblocksRecorderError>;

/// Dedicated libmdbx tables used by the flashblocks recorder.
pub mod tables {
    use alloy_primitives::{BlockNumber, Bytes};
    use reth_db_api::{
        TableSet,
        table::TableInfo,
        tables,
        tables::{TableType, TableViewer},
    };
    use std::fmt;

    tables! {
        /// Stores accepted flashblocks by `(payload_id || flashblock_index_be)`.
        table Flashblocks {
            type Key = Vec<u8>;
            type Value = Bytes;
        }

        /// Maps a block number to the payload id observed on that block's base flashblock.
        table BlockNumberToPayloadId {
            type Key = BlockNumber;
            type Value = Bytes;
        }
    }
}

#[derive(Debug, thiserror::Error)]
enum FlashblocksRecorderError {
    #[error("failed to create flashblocks recorder parent directory {path}: {source}")]
    CreateParentDirectory {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("failed to open flashblocks recorder database {path}: {message}")]
    OpenDatabase { path: PathBuf, message: String },
    #[error("failed to create flashblocks recorder tables: {source}")]
    CreateTables {
        #[source]
        source: DatabaseError,
    },
    #[error("failed to begin flashblocks recorder transaction: {source}")]
    BeginTransaction {
        #[source]
        source: DatabaseError,
    },
    #[error(
        "failed to insert flashblock payload_id={payload_id} flashblock_index={flashblock_index}: {source}"
    )]
    InsertFlashblock {
        payload_id: String,
        flashblock_index: u64,
        #[source]
        source: DatabaseError,
    },
    #[error("failed to insert block_number={block_number} to payload_id={payload_id}: {source}")]
    InsertBlockNumberIndex {
        block_number: u64,
        payload_id: String,
        #[source]
        source: DatabaseError,
    },
    #[error("failed to commit flashblocks recorder transaction: {source}")]
    CommitTransaction {
        #[source]
        source: DatabaseError,
    },
}

/// Configuration for the flashblocks recorder database.
#[derive(Clone, Debug)]
pub struct FlashblocksRecorderConfig {
    /// Directory path for the dedicated libmdbx database environment.
    pub path: PathBuf,
}

impl FlashblocksRecorderConfig {
    /// Creates a flashblocks recorder configuration for `path`.
    pub fn new(path: PathBuf) -> Self {
        Self { path }
    }
}

/// Best-effort flashblocks recorder.
///
/// The recorder never applies backpressure to the p2p path. Accepted ordered
/// flashblocks are copied into a bounded channel and may be dropped if the
/// writer cannot keep up or the database is unavailable.
#[derive(Clone, Debug)]
pub struct FlashblocksRecorder {
    tx: mpsc::Sender<FlashblocksRecord>,
}

#[derive(Debug)]
struct FlashblocksRecord {
    payload: FlashblocksPayloadV1,
}

impl FlashblocksRecorder {
    /// Spawns the background writer task and returns a lightweight recorder handle.
    pub fn spawn(config: FlashblocksRecorderConfig) -> Self {
        let (tx, rx) = mpsc::channel(STORE_CHANNEL_CAPACITY);
        tokio::spawn(run_writer(config, rx));

        Self { tx }
    }

    /// Queues an accepted flashblock payload for persistence.
    pub fn record(&self, payload: &FlashblocksPayloadV1) {
        let record = FlashblocksRecord {
            payload: payload.clone(),
        };

        if let Err(err) = self.tx.try_send(record) {
            warn!(
                target: "flashblocks::recorder",
                %err,
                "dropping flashblock record because recorder channel is unavailable"
            );
        }
    }
}

async fn run_writer(config: FlashblocksRecorderConfig, mut rx: mpsc::Receiver<FlashblocksRecord>) {
    let mut db = None;

    while let Some(record) = rx.recv().await {
        if db.is_none() {
            match open_db(&config) {
                Ok(conn) => {
                    debug!(
                        target: "flashblocks::recorder",
                        path = %config.path.display(),
                        "flashblocks recorder database opened"
                    );
                    db = Some(conn);
                }
                Err(err) => {
                    error!(
                        target: "flashblocks::recorder",
                        path = %config.path.display(),
                        %err,
                        "failed to open flashblocks recorder database; dropping record"
                    );
                    tokio::time::sleep(DB_RETRY_DELAY).await;
                    continue;
                }
            }
        }

        let conn = db.as_ref().expect("connection is initialized above");
        if let Err(err) = insert_record(conn, &record) {
            error!(
                target: "flashblocks::recorder",
                path = %config.path.display(),
                %err,
                "failed to store flashblock; dropping record"
            );
            // Reopen on the next record in case the connection entered a bad state
            // or the database directory was replaced while the node was running.
            db = None;
            tokio::time::sleep(DB_RETRY_DELAY).await;
        }
    }
}

fn open_db(config: &FlashblocksRecorderConfig) -> RecorderResult<DatabaseEnv> {
    if let Some(parent) = config
        .path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        fs::create_dir_all(parent).map_err(|source| {
            FlashblocksRecorderError::CreateParentDirectory {
                path: parent.to_path_buf(),
                source,
            }
        })?;
    }

    let mut db = create_db(&config.path, DatabaseArguments::default()).map_err(|source| {
        FlashblocksRecorderError::OpenDatabase {
            path: config.path.clone(),
            message: source.to_string(),
        }
    })?;

    db.create_and_track_tables_for::<tables::Tables>()
        .map_err(|source| FlashblocksRecorderError::CreateTables { source })?;

    Ok(db)
}

fn insert_record(db: &DatabaseEnv, record: &FlashblocksRecord) -> RecorderResult<()> {
    let payload = &record.payload;
    let payload_id = payload.payload_id.0;
    let payload_key = flashblock_key(payload_id, payload.index);
    let payload_id_value = Bytes::copy_from_slice(payload_id.as_slice());
    let payload_value = Bytes::from(alloy_rlp::encode(payload));

    let tx = db
        .tx_mut()
        .map_err(|source| FlashblocksRecorderError::BeginTransaction { source })?;

    tx.put::<tables::Flashblocks>(payload_key, payload_value)
        .map_err(|source| FlashblocksRecorderError::InsertFlashblock {
            payload_id: payload.payload_id.to_string(),
            flashblock_index: payload.index,
            source,
        })?;

    if let Some(base) = &payload.base {
        tx.put::<tables::BlockNumberToPayloadId>(base.block_number, payload_id_value)
            .map_err(|source| FlashblocksRecorderError::InsertBlockNumberIndex {
                block_number: base.block_number,
                payload_id: payload.payload_id.to_string(),
                source,
            })?;
    }

    tx.commit()
        .map_err(|source| FlashblocksRecorderError::CommitTransaction { source })
}

/// Encodes the primary key as `payload_id || flashblock_index_be`.
///
/// Big-endian index encoding preserves natural index ordering during libmdbx
/// lexicographic iteration.
pub fn flashblock_key(payload_id: impl AsRef<[u8]>, index: u64) -> Vec<u8> {
    let payload_id = payload_id.as_ref();
    debug_assert_eq!(payload_id.len(), 8, "payload ids must be 8 bytes");

    let mut key = Vec::with_capacity(16);
    key.extend_from_slice(payload_id);
    key.extend_from_slice(index.to_be_bytes().as_slice());
    key
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy_primitives::{Address, B256};
    use alloy_rpc_types_engine::PayloadId;
    use reth_db_api::transaction::DbTx;
    use tempfile::tempdir;
    use world_chain_primitives::primitives::{
        ExecutionPayloadBaseV1, ExecutionPayloadFlashblockDeltaV1,
    };

    #[test]
    fn stores_and_overwrites_flashblock_records() {
        let dir = tempdir().expect("tempdir");
        let config = FlashblocksRecorderConfig::new(dir.path().join("flashblocks.mdbx"));
        let db = open_db(&config).expect("open db");

        insert_record(&db, &record(Some(42), 0)).expect("insert first");

        let replacement = record(Some(42), 0);
        let expected_payload_rlp = Bytes::from(alloy_rlp::encode(&replacement.payload));
        insert_record(&db, &replacement).expect("replace duplicate key");

        let tx = db.tx().expect("read tx");
        let payload_id = replacement.payload.payload_id.0;
        let stored_payload = tx
            .get::<tables::Flashblocks>(flashblock_key(payload_id, 0))
            .expect("read flashblock")
            .expect("flashblock exists");
        let stored_payload_id = tx
            .get::<tables::BlockNumberToPayloadId>(42)
            .expect("read block number index")
            .expect("block number index exists");
        let flashblocks_len = tx.entries::<tables::Flashblocks>().expect("entries");
        let block_index_len = tx
            .entries::<tables::BlockNumberToPayloadId>()
            .expect("entries");
        tx.commit().expect("commit read tx");

        assert_eq!(stored_payload, expected_payload_rlp);
        assert_eq!(
            stored_payload_id,
            Bytes::copy_from_slice(payload_id.as_slice())
        );
        assert_eq!(flashblocks_len, 1);
        assert_eq!(block_index_len, 1);
    }

    #[test]
    fn only_base_flashblocks_index_block_number() {
        let dir = tempdir().expect("tempdir");
        let config = FlashblocksRecorderConfig::new(dir.path().join("flashblocks.mdbx"));
        let db = open_db(&config).expect("open db");

        insert_record(&db, &record(None, 1)).expect("insert delta");

        let tx = db.tx().expect("read tx");
        let payload = tx
            .get::<tables::Flashblocks>(flashblock_key(PayloadId::new([3; 8]).0, 1))
            .expect("read flashblock");
        let block_index_len = tx
            .entries::<tables::BlockNumberToPayloadId>()
            .expect("entries");
        tx.commit().expect("commit read tx");

        assert!(payload.is_some());
        assert_eq!(block_index_len, 0);
    }

    #[test]
    fn flashblock_key_checks() {
        let payload_id = [3; 8];

        assert_eq!(flashblock_key(payload_id, 1), vec![3, 3, 3, 3, 3, 3, 3, 3, 0, 0, 0, 0, 0, 0, 0, 1]);
    }

    #[test]
    fn flashblock_key_orders_by_payload_then_index() {
        let payload_id = [3; 8];

        assert!(flashblock_key(payload_id, 1) < flashblock_key(payload_id, 2));
        assert!(flashblock_key([2; 8], 10) < flashblock_key([3; 8], 0));
    }

    fn record(block_number: Option<u64>, index: u64) -> FlashblocksRecord {
        FlashblocksRecord {
            payload: payload(block_number, index),
        }
    }

    fn payload(block_number: Option<u64>, index: u64) -> FlashblocksPayloadV1 {
        FlashblocksPayloadV1 {
            payload_id: PayloadId::new([3; 8]),
            index,
            diff: ExecutionPayloadFlashblockDeltaV1 {
                block_hash: B256::from([5; 32]),
                ..Default::default()
            },
            base: block_number.map(|block_number| ExecutionPayloadBaseV1 {
                parent_hash: B256::from([4; 32]),
                block_number,
                fee_recipient: Address::ZERO,
                ..Default::default()
            }),
            ..Default::default()
        }
    }
}

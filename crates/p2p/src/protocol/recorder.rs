//! Best-effort persistence for accepted flashblocks.
//!
//! The recorder uses a separate libmdbx database environment from the node's
//! canonical state database. It stores the exact [`FlashblocksPayloadV1`] RLP
//! bytes as the source of truth and keeps a small secondary index from block
//! number to payload id for later benchmark replay.

use alloy_primitives::{BlockNumber, Bytes, bytes::BufMut};
use alloy_rpc_types_engine::PayloadId;
use reth_codecs::DecompressError;
use reth_db::{
    Database, DatabaseEnv,
    mdbx::{DatabaseArguments, create_db},
};
use reth_db_api::{
    DatabaseError,
    cursor::{DbCursorRW, DbDupCursorRO},
    table::{Compress, Decode, Decompress, Encode},
    transaction::{DbTx, DbTxMut},
};
use reth_primitives_traits::ValueWithSubKey;
use std::{fmt, fs, path::PathBuf, time::Duration};
use tokio::sync::mpsc;
use tracing::{debug, error, warn};
use world_chain_primitives::primitives::FlashblocksPayloadV1;

const STORE_CHANNEL_CAPACITY: usize = 1_024;
const DB_RETRY_DELAY: Duration = Duration::from_secs(1);

type RecorderResult<T> = Result<T, FlashblocksRecorderError>;

/// Dedicated libmdbx tables used by the flashblocks recorder.
pub mod tables {
    use alloy_primitives::BlockNumber;
    use reth_db_api::{
        TableSet,
        table::{DupSort, TableInfo},
        tables,
        tables::{TableType, TableViewer},
    };
    use std::fmt;
    use super::{StoredFlashblock, StoredPayloadId};

    tables! {
        /// Stores accepted flashblocks by payload id and flashblock index.
        table Flashblocks {
            type Key = StoredPayloadId;
            type Value = StoredFlashblock;
            type SubKey = u64;
        }

        /// Maps a block number to the payload id observed on that block's base flashblock.
        table BlockNumberToPayloadId {
            type Key = BlockNumber;
            type Value = StoredPayloadId;
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
    #[error("failed to open flashblocks recorder dupsort cursor: {source}")]
    OpenFlashblocksCursor {
        #[source]
        source: DatabaseError,
    },
    #[error(
        "failed to seek existing flashblock payload_id={payload_id} flashblock_index={flashblock_index}: {source}"
    )]
    SeekExistingFlashblock {
        payload_id: StoredPayloadId,
        flashblock_index: u64,
        #[source]
        source: DatabaseError,
    },
    #[error(
        "failed to delete existing flashblock payload_id={payload_id} flashblock_index={flashblock_index}: {source}"
    )]
    DeleteExistingFlashblock {
        payload_id: StoredPayloadId,
        flashblock_index: u64,
        #[source]
        source: DatabaseError,
    },
    #[error(
        "failed to insert flashblock payload_id={payload_id} flashblock_index={flashblock_index}: {source}"
    )]
    InsertFlashblock {
        payload_id: StoredPayloadId,
        flashblock_index: u64,
        #[source]
        source: DatabaseError,
    },
    #[error("failed to insert block_number={block_number} to payload_id={payload_id}: {source}")]
    InsertBlockNumberIndex {
        block_number: BlockNumber,
        payload_id: StoredPayloadId,
        #[source]
        source: DatabaseError,
    },
    #[error("failed to commit flashblocks recorder transaction: {source}")]
    CommitTransaction {
        #[source]
        source: DatabaseError,
    },
}

#[derive(Debug, thiserror::Error)]
enum StoredPayloadIdDecodeError {
    #[error("stored payload id must be 8 bytes, got {len}")]
    InvalidLength { len: usize },
}

/// Database representation of an engine [`PayloadId`].
///
/// `PayloadId` wraps Alloy's `B64`, which does not implement reth's database
/// codecs. This newtype keeps the recorder schema typed while encoding payload
/// ids as their canonical 8 bytes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct StoredPayloadId(PayloadId);

impl StoredPayloadId {
    /// Creates a stored payload id from an engine payload id.
    pub const fn new(payload_id: PayloadId) -> Self {
        Self(payload_id)
    }

    /// Returns the engine payload id.
    pub const fn payload_id(self) -> PayloadId {
        self.0
    }

    /// Returns the fixed 8-byte payload id.
    pub const fn to_bytes(self) -> [u8; 8] {
        self.0.0.0
    }

    /// Returns the payload id bytes.
    pub fn as_slice(&self) -> &[u8] {
        self.0.0.as_slice()
    }
}

impl From<PayloadId> for StoredPayloadId {
    fn from(value: PayloadId) -> Self {
        Self::new(value)
    }
}

impl From<[u8; 8]> for StoredPayloadId {
    fn from(value: [u8; 8]) -> Self {
        Self::new(PayloadId::new(value))
    }
}

impl From<StoredPayloadId> for PayloadId {
    fn from(value: StoredPayloadId) -> Self {
        value.payload_id()
    }
}

impl fmt::Display for StoredPayloadId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

impl PartialOrd for StoredPayloadId {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for StoredPayloadId {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.as_slice().cmp(other.as_slice())
    }
}

impl serde::Serialize for StoredPayloadId {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        <[u8; 8] as serde::Serialize>::serialize(&self.to_bytes(), serializer)
    }
}

impl<'de> serde::Deserialize<'de> for StoredPayloadId {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let bytes = <[u8; 8] as serde::Deserialize>::deserialize(deserializer)?;
        Ok(Self::from(bytes))
    }
}

impl Encode for StoredPayloadId {
    type Encoded = [u8; 8];

    fn encode(self) -> Self::Encoded {
        self.to_bytes()
    }
}

impl Decode for StoredPayloadId {
    fn decode(value: &[u8]) -> Result<Self, DatabaseError> {
        Ok(Self::from(
            <[u8; 8]>::try_from(value).map_err(|_| DatabaseError::Decode)?,
        ))
    }
}

impl Compress for StoredPayloadId {
    type Compressed = Vec<u8>;

    fn uncompressable_ref(&self) -> Option<&[u8]> {
        Some(self.as_slice())
    }

    fn compress_to_buf<B: BufMut + AsMut<[u8]>>(&self, buf: &mut B) {
        buf.put_slice(self.as_slice());
    }
}

impl Decompress for StoredPayloadId {
    fn decompress(value: &[u8]) -> Result<Self, DecompressError> {
        let bytes = <[u8; 8]>::try_from(value).map_err(|_| {
            DecompressError::new(StoredPayloadIdDecodeError::InvalidLength { len: value.len() })
        })?;

        Ok(Self::from(bytes))
    }
}

#[derive(Debug, thiserror::Error)]
enum StoredFlashblockDecodeError {
    #[error("stored flashblock value is too short: expected at least 8 bytes, got {len}")]
    TooShort { len: usize },
}

/// Value stored in the [`tables::Flashblocks`] dupsort table.
///
/// libmdbx sorts and seeks duplicate entries by the duplicate value bytes. The
/// encoded representation therefore starts with the flashblock index in
/// big-endian order, followed by the RLP-encoded [`FlashblocksPayloadV1`].
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub struct StoredFlashblock {
    /// Flashblock index within the payload.
    pub index: u64,
    /// RLP bytes for the original [`FlashblocksPayloadV1`].
    pub payload_rlp: Bytes,
}

impl StoredFlashblock {
    /// Creates a stored flashblock value.
    pub const fn new(index: u64, payload_rlp: Bytes) -> Self {
        Self { index, payload_rlp }
    }

    /// Returns the RLP bytes for the original [`FlashblocksPayloadV1`].
    pub const fn payload_rlp(&self) -> &Bytes {
        &self.payload_rlp
    }
}

impl ValueWithSubKey for StoredFlashblock {
    type SubKey = u64;

    fn get_subkey(&self) -> Self::SubKey {
        self.index
    }
}

impl Compress for StoredFlashblock {
    type Compressed = Vec<u8>;

    fn compress_to_buf<B: BufMut + AsMut<[u8]>>(&self, buf: &mut B) {
        buf.put_slice(self.index.to_be_bytes().as_slice());
        buf.put_slice(self.payload_rlp.as_ref());
    }
}

impl Decompress for StoredFlashblock {
    fn decompress(value: &[u8]) -> Result<Self, reth_codecs::DecompressError> {
        let index_bytes = value.get(..8).ok_or_else(|| {
            DecompressError::new(StoredFlashblockDecodeError::TooShort { len: value.len() })
        })?;
        let index = u64::from_be_bytes(index_bytes.try_into().expect("slice length checked above"));
        let payload_rlp = Bytes::copy_from_slice(&value[8..]);

        Ok(Self { index, payload_rlp })
    }
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
    let payload_id = StoredPayloadId::from(payload.payload_id);
    let payload_value =
        StoredFlashblock::new(payload.index, Bytes::from(alloy_rlp::encode(payload)));

    let tx = db
        .tx_mut()
        .map_err(|source| FlashblocksRecorderError::BeginTransaction { source })?;

    upsert_flashblock(&tx, payload_id, payload_value)?;

    if let Some(base) = &payload.base {
        tx.put::<tables::BlockNumberToPayloadId>(base.block_number, payload_id)
            .map_err(|source| FlashblocksRecorderError::InsertBlockNumberIndex {
                block_number: base.block_number,
                payload_id,
                source,
            })?;
    }

    tx.commit()
        .map_err(|source| FlashblocksRecorderError::CommitTransaction { source })
}

fn upsert_flashblock<Tx>(
    tx: &Tx,
    payload_id: StoredPayloadId,
    payload_value: StoredFlashblock,
) -> RecorderResult<()>
where
    Tx: DbTxMut,
{
    let mut cursor = tx
        .cursor_dup_write::<tables::Flashblocks>()
        .map_err(|source| FlashblocksRecorderError::OpenFlashblocksCursor { source })?;

    // Duplicate values are sorted by `index_be || payload_rlp`. Remove any
    // previous value with this index before inserting so duplicate writes replace
    // instead of accumulating stale payload bytes.
    loop {
        let existing = cursor
            .seek_by_key_subkey(payload_id, payload_value.index)
            .map_err(|source| FlashblocksRecorderError::SeekExistingFlashblock {
                payload_id,
                flashblock_index: payload_value.index,
                source,
            })?;

        if existing.is_none_or(|existing| existing.index != payload_value.index) {
            break;
        }

        cursor.delete_current().map_err(|source| {
            FlashblocksRecorderError::DeleteExistingFlashblock {
                payload_id,
                flashblock_index: payload_value.index,
                source,
            }
        })?;
    }

    cursor.upsert(payload_id, &payload_value).map_err(|source| {
        FlashblocksRecorderError::InsertFlashblock {
            payload_id,
            flashblock_index: payload_value.index,
            source,
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy_primitives::{Address, B256};
    use alloy_rpc_types_engine::PayloadId;
    use reth_db_api::{cursor::DbDupCursorRO, transaction::DbTx};
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
        let second_flashblock = record(None, 1);
        let expected_second_payload_rlp = Bytes::from(alloy_rlp::encode(&second_flashblock.payload));
        insert_record(&db, &second_flashblock).expect("insert second flashblock");

        let replacement = record_with_hash(Some(42), 0, B256::from([9; 32]));
        let expected_payload_rlp = Bytes::from(alloy_rlp::encode(&replacement.payload));
        insert_record(&db, &replacement).expect("replace duplicate key");

        let tx = db.tx().expect("read tx");
        let payload_id = StoredPayloadId::from(replacement.payload.payload_id);
        let mut flashblocks = tx
            .cursor_dup_read::<tables::Flashblocks>()
            .expect("flashblocks cursor");
        let stored_payload = flashblocks
            .seek_by_key_subkey(payload_id, 0)
            .expect("read flashblock")
            .expect("flashblock exists");
        let stored_second_payload = flashblocks
            .seek_by_key_subkey(payload_id, 1)
            .expect("read second flashblock")
            .expect("second flashblock exists");
        let stored_payload_id = tx
            .get::<tables::BlockNumberToPayloadId>(42)
            .expect("read block number index")
            .expect("block number index exists");
        let flashblocks_len = tx.entries::<tables::Flashblocks>().expect("entries");
        let block_index_len = tx
            .entries::<tables::BlockNumberToPayloadId>()
            .expect("entries");
        tx.commit().expect("commit read tx");

        assert_eq!(stored_payload.index, 0);
        assert_eq!(stored_payload.payload_rlp, expected_payload_rlp);
        assert_eq!(stored_second_payload.index, 1);
        assert_eq!(stored_second_payload.payload_rlp, expected_second_payload_rlp);
        assert_eq!(stored_payload_id, payload_id);
        assert_eq!(flashblocks_len, 2);
        assert_eq!(block_index_len, 1);
    }

    #[test]
    fn only_base_flashblocks_index_block_number() {
        let dir = tempdir().expect("tempdir");
        let config = FlashblocksRecorderConfig::new(dir.path().join("flashblocks.mdbx"));
        let db = open_db(&config).expect("open db");

        insert_record(&db, &record(None, 1)).expect("insert delta");

        let tx = db.tx().expect("read tx");
        let mut flashblocks = tx
            .cursor_dup_read::<tables::Flashblocks>()
            .expect("flashblocks cursor");
        let payload = flashblocks
            .seek_by_key_subkey(StoredPayloadId::from(PayloadId::new([3; 8])), 1)
            .expect("read flashblock")
            .expect("flashblock exists");
        let block_index_len = tx
            .entries::<tables::BlockNumberToPayloadId>()
            .expect("entries");
        tx.commit().expect("commit read tx");

        assert_eq!(payload.index, 1);
        assert_eq!(block_index_len, 0);
    }

    #[test]
    fn stored_payload_id_codecs_roundtrip() {
        let payload_id = StoredPayloadId::from(PayloadId::new([3; 8]));

        assert_eq!(payload_id.encode(), [3, 3, 3, 3, 3, 3, 3, 3]);
        assert_eq!(
            StoredPayloadId::decode(&payload_id.encode()).expect("decode payload id"),
            payload_id
        );
        assert_eq!(payload_id.compress(), vec![3, 3, 3, 3, 3, 3, 3, 3]);
        assert_eq!(
            StoredPayloadId::decompress(payload_id.as_slice()).expect("decompress payload id"),
            payload_id
        );
    }

    #[test]
    fn stored_flashblock_value_prefixes_index() {
        let stored = StoredFlashblock::new(1, Bytes::from_static(&[7, 8]));
        let encoded = stored.clone().compress();

        assert_eq!(encoded, vec![0, 0, 0, 0, 0, 0, 0, 1, 7, 8]);
        assert_eq!(
            StoredFlashblock::decompress(&encoded).expect("decode stored flashblock"),
            stored
        );
    }

    fn record(block_number: Option<u64>, index: u64) -> FlashblocksRecord {
        record_with_hash(block_number, index, B256::from([5; 32]))
    }

    fn record_with_hash(
        block_number: Option<u64>,
        index: u64,
        block_hash: B256,
    ) -> FlashblocksRecord {
        FlashblocksRecord {
            payload: payload(block_number, index, block_hash),
        }
    }

    fn payload(block_number: Option<u64>, index: u64, block_hash: B256) -> FlashblocksPayloadV1 {
        FlashblocksPayloadV1 {
            payload_id: PayloadId::new([3; 8]),
            index,
            diff: ExecutionPayloadFlashblockDeltaV1 {
                block_hash,
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

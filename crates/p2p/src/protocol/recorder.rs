//! Best-effort persistence for accepted flashblocks.
//!
//! This module intentionally keeps the stored representation small. The recorder
//! persists the exact [`FlashblocksPayloadV1`] blob plus only the metadata needed
//! to query it later: payload id, flashblock index, and the block number when it
//! is present on the base flashblock.

use std::{fs, path::PathBuf, time::Duration};

use rusqlite::{Connection, params};
use tokio::sync::mpsc;
use tracing::{debug, error, warn};
use world_chain_primitives::primitives::FlashblocksPayloadV1;

const STORE_CHANNEL_CAPACITY: usize = 1_024;
const DB_RETRY_DELAY: Duration = Duration::from_secs(1);
const DB_BUSY_TIMEOUT: Duration = Duration::from_secs(5);

type RecorderResult<T> = Result<T, FlashblocksRecorderError>;

#[derive(Debug, thiserror::Error)]
enum FlashblocksRecorderError {
    #[error("failed to create flashblocks recorder directory {path}: {source}")]
    CreateDirectory {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("failed to open flashblocks recorder database {path}: {source}")]
    OpenDatabase {
        path: PathBuf,
        #[source]
        source: rusqlite::Error,
    },
    #[error("failed to configure flashblocks recorder database busy timeout {path}: {source}")]
    ConfigureBusyTimeout {
        path: PathBuf,
        #[source]
        source: rusqlite::Error,
    },
    #[error("failed to configure flashblocks recorder database journal mode {path}: {source}")]
    ConfigureJournalMode {
        path: PathBuf,
        #[source]
        source: rusqlite::Error,
    },
    #[error("failed to configure flashblocks recorder database synchronous mode {path}: {source}")]
    ConfigureSynchronousMode {
        path: PathBuf,
        #[source]
        source: rusqlite::Error,
    },
    #[error("failed to create flashblocks recorder schema {path}: {source}")]
    CreateSchema {
        path: PathBuf,
        #[source]
        source: rusqlite::Error,
    },
    #[error(
        "failed to insert flashblock record payload_id={payload_id} flashblock_index={flashblock_index}: {source}"
    )]
    InsertRecord {
        payload_id: String,
        flashblock_index: u64,
        #[source]
        source: rusqlite::Error,
    },
    #[error("{field} value {value} cannot be stored as sqlite INTEGER: {source}")]
    IntegerOutOfRange {
        field: &'static str,
        value: u64,
        #[source]
        source: std::num::TryFromIntError,
    },
}

/// Config for the flashblocks recorder.
#[derive(Clone, Debug)]
pub struct FlashblocksRecorderConfig {
    /// SQLite database path used to persist flashblocks.
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
    block_number: Option<u64>,
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
    ///
    /// Only base flashblocks carry execution-payload base data and therefore a
    /// block number. Delta flashblocks are stored with `NULL` block number.
    pub fn record(&self, payload: &FlashblocksPayloadV1) {
        let record = FlashblocksRecord {
            block_number: payload.base.as_ref().map(|base| base.block_number),
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

        let conn = db.as_mut().expect("connection is initialized above");
        if let Err(err) = insert_record(conn, &record) {
            error!(
                target: "flashblocks::recorder",
                path = %config.path.display(),
                %err,
                "failed to store flashblock; dropping record"
            );
            db = None;
            tokio::time::sleep(DB_RETRY_DELAY).await;
        }
    }
}

fn open_db(config: &FlashblocksRecorderConfig) -> RecorderResult<Connection> {
    if let Some(parent) = config
        .path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        fs::create_dir_all(parent).map_err(|source| FlashblocksRecorderError::CreateDirectory {
            path: parent.to_path_buf(),
            source,
        })?;
    }

    let conn = Connection::open(&config.path).map_err(|source| {
        FlashblocksRecorderError::OpenDatabase {
            path: config.path.clone(),
            source,
        }
    })?;
    // Set the busy timeout to handle situations where the database is locked due to other reads/writes,
    // allowing this connection to wait up to DB_BUSY_TIMEOUT before erroring.
    conn.busy_timeout(DB_BUSY_TIMEOUT).map_err(|source| {
        FlashblocksRecorderError::ConfigureBusyTimeout {
            path: config.path.clone(),
            source,
        }
    })?;

    // Switch the SQLite database to use write-ahead logging (WAL) mode.
    // WAL mode allows for concurrent reads and writes, improving performance and reliability
    // for append-heavy usage patterns like flashblock recording.
    conn.pragma_update(None, "journal_mode", "WAL")
        .map_err(|source| FlashblocksRecorderError::ConfigureJournalMode {
            path: config.path.clone(),
            source,
        })?;

    // Set the synchronous pragma to "NORMAL" to strike a balance between durability
    // and performance. This means the OS-level write cache is not flushed on every transaction,
    // but durability should remain sufficient for most flashblock persistence needs.
    conn.pragma_update(None, "synchronous", "NORMAL")
        .map_err(
            |source| FlashblocksRecorderError::ConfigureSynchronousMode {
                path: config.path.clone(),
                source,
            },
        )?;
    conn.execute_batch(
        r#"
        CREATE TABLE IF NOT EXISTS flashblocks (
            payload_id BLOB NOT NULL,
            flashblock_index INTEGER NOT NULL,
            block_number INTEGER,
            flashblock_payload_rlp BLOB NOT NULL,
            PRIMARY KEY (payload_id, flashblock_index)
        );
        "#,
    )
    .map_err(|source| FlashblocksRecorderError::CreateSchema {
        path: config.path.clone(),
        source,
    })?;
    Ok(conn)
}

fn insert_record(conn: &mut Connection, record: &FlashblocksRecord) -> RecorderResult<()> {
    let payload = &record.payload;
    let flashblock_payload_rlp = alloy_rlp::encode(payload);
    let block_number = record
        .block_number
        .map(|block_number| to_i64("block_number", block_number))
        .transpose()?;

    conn.execute(
        r#"
        INSERT OR REPLACE INTO flashblocks (
            payload_id,
            flashblock_index,
            block_number,
            flashblock_payload_rlp
        ) VALUES (?1, ?2, ?3, ?4)
        "#,
        params![
            payload.payload_id.0.to_vec(),
            to_i64("flashblock_index", payload.index)?,
            block_number,
            flashblock_payload_rlp,
        ],
    )
    .map_err(|source| FlashblocksRecorderError::InsertRecord {
        payload_id: payload.payload_id.to_string(),
        flashblock_index: payload.index,
        source,
    })?;

    Ok(())
}

fn to_i64(field: &'static str, value: u64) -> RecorderResult<i64> {
    i64::try_from(value).map_err(|source| FlashblocksRecorderError::IntegerOutOfRange {
        field,
        value,
        source,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy_primitives::{Address, B256, Bytes};
    use alloy_rpc_types_engine::PayloadId;
    use tempfile::tempdir;
    use world_chain_primitives::primitives::{
        ExecutionPayloadBaseV1, ExecutionPayloadFlashblockDeltaV1,
    };

    #[test]
    fn stores_and_overwrites_flashblock_records() {
        let dir = tempdir().expect("tempdir");
        let path = dir.path().join("flashblocks.sqlite");
        let config = FlashblocksRecorderConfig::new(path);
        let mut conn = open_db(&config).expect("open db");

        insert_record(&mut conn, &record(1, Some(42), 0)).expect("insert first");

        let replacement = record(2, Some(42), 0);
        let expected_payload_rlp = alloy_rlp::encode(&replacement.payload);
        insert_record(&mut conn, &replacement).expect("replace duplicate key");

        let columns = table_columns(&conn);
        assert_eq!(
            columns,
            vec![
                "payload_id",
                "flashblock_index",
                "block_number",
                "flashblock_payload_rlp"
            ]
        );

        let (rows, block_number, flashblock_payload_rlp): (i64, i64, Vec<u8>) = conn
            .query_row(
                "SELECT COUNT(*), block_number, flashblock_payload_rlp FROM flashblocks",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .expect("query row");

        assert_eq!(rows, 1);
        assert_eq!(block_number, 42);
        assert_eq!(flashblock_payload_rlp, expected_payload_rlp);
    }

    #[test]
    fn stores_null_block_number_for_delta_flashblocks() {
        let dir = tempdir().expect("tempdir");
        let config = FlashblocksRecorderConfig::new(dir.path().join("flashblocks.sqlite"));
        let mut conn = open_db(&config).expect("open db");

        insert_record(&mut conn, &record(1, None, 1)).expect("insert delta");

        let block_number: Option<i64> = conn
            .query_row("SELECT block_number FROM flashblocks", [], |row| row.get(0))
            .expect("query row");

        assert_eq!(block_number, None);
    }

    fn record(tx_count: usize, block_number: Option<u64>, index: u64) -> FlashblocksRecord {
        FlashblocksRecord {
            block_number,
            payload: payload(tx_count, block_number, index),
        }
    }

    fn payload(tx_count: usize, block_number: Option<u64>, index: u64) -> FlashblocksPayloadV1 {
        FlashblocksPayloadV1 {
            payload_id: PayloadId::new([3; 8]),
            index,
            diff: ExecutionPayloadFlashblockDeltaV1 {
                block_hash: B256::from([5; 32]),
                transactions: vec![Bytes::from(vec![0u8]); tx_count],
                gas_used: 21_000 * tx_count as u64,
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

    fn table_columns(conn: &Connection) -> Vec<String> {
        let mut stmt = conn.prepare("PRAGMA table_info(flashblocks)").unwrap();
        stmt.query_map([], |row| row.get::<_, String>(1))
            .unwrap()
            .map(|column| column.unwrap())
            .collect()
    }
}

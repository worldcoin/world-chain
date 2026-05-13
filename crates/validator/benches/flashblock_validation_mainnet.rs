use std::{path::PathBuf, sync::Arc};

use alloy_rlp::Decodable;
use criterion::{BatchSize, BenchmarkId, Criterion, criterion_group, criterion_main};
use eyre::eyre::{bail, ensure, eyre};
use reth_chain_state::ExecutedBlock;
use reth_db::{ClientVersion, Database, DatabaseEnv, mdbx::DatabaseArguments, open_db_read_only};
use reth_db_api::{cursor::DbCursorRO, transaction::DbTx};
use reth_node_api::NodeTypesWithDBAdapter;
use reth_node_core::node_config::NodeConfig;
use reth_optimism_primitives::OpPrimitives;
use reth_provider::{
    HeaderProvider, ProviderFactory,
    providers::{BlockchainProvider, RocksDBProvider, StaticFileProviderBuilder},
};
use reth_tasks::Runtime as RethRuntime;
use world_chain_chainspec::WorldChainSpec;
use world_chain_evm::{OpRethReceiptBuilder, WorldChainEvmConfig};
use world_chain_node::{context::WorldChainDefaultContext, node::WorldChainNode};
use world_chain_p2p::protocol::{
    handler::FlashblocksHandle,
    recorder::{StoredFlashblockKey, StoredFlashblockPayload, tables},
};
use world_chain_primitives::{ed25519_dalek::SigningKey, primitives::FlashblocksPayloadV1};
use world_chain_validator::{
    coordinator::{FlashblocksExecutionCoordinator, process_flashblock},
    flashblock_validation_metrics::FlashblockValidationMetrics,
};

/// Block loaded from the default flashblocks recorder database.
///
/// Change this constant locally when switching the captured mainnet block used
/// by the bench. The block selection is intentionally not configurable at
/// runtime so Criterion runs are deterministic and easy to compare.
const BENCH_BLOCK_NUMBER: u64 = 29669201;
const SAMPLE_SIZE: usize = 10;

/// Optional override for the World Chain mainnet datadir root.
///
/// When set, the bench resolves all on-disk paths (node DB, static files,
/// rocksdb, flashblocks recorder DB) underneath this directory, mirroring the
/// layout reth produces under the platform-default datadir. When unset, the
/// platform-default datadir for World Chain mainnet is used.
const DATADIR_ENV: &str = "WC_BENCH_DATADIR";

type MainnetNode = WorldChainNode<WorldChainDefaultContext>;
type MainnetProvider = BlockchainProvider<NodeTypesWithDBAdapter<MainnetNode, DatabaseEnv>>;

#[derive(Debug)]
struct BenchPaths {
    node_db: PathBuf,
    static_files: PathBuf,
    rocksdb: PathBuf,
    flashblocks_db: PathBuf,
}

#[derive(Debug)]
struct StoredFlashblocks {
    flashblocks: Vec<FlashblocksPayloadV1>,
}

/// Helper: creates a fresh coordinator + handle + watch channel.
///
/// A fresh coordinator is created for each Criterion iteration, but the full
/// flashblock sequence for `BENCH_BLOCK_NUMBER` is processed by the same
/// coordinator because later flashblocks depend on the state produced by earlier
/// ones.
fn fresh_coordinator(
    rt: &tokio::runtime::Runtime,
) -> (
    FlashblocksExecutionCoordinator,
    FlashblocksHandle,
    tokio::sync::watch::Sender<Option<ExecutedBlock<OpPrimitives>>>,
) {
    let _guard = rt.enter();
    let sk = SigningKey::from_bytes(&[1u8; 32]);
    let vk = sk.verifying_key();
    let handle = FlashblocksHandle::new(vk, Some(sk));
    let (pending_tx, _pending_rx) =
        tokio::sync::watch::channel::<Option<ExecutedBlock<OpPrimitives>>>(None);
    let coordinator = FlashblocksExecutionCoordinator::new(handle.clone(), pending_tx.clone());
    (coordinator, handle, pending_tx)
}

fn world_chain_mainnet_spec() -> Arc<WorldChainSpec> {
    WorldChainSpec::mainnet()
}

fn bench_paths(chain_spec: Arc<WorldChainSpec>) -> BenchPaths {
    if let Some(root) = std::env::var_os(DATADIR_ENV) {
        let root = PathBuf::from(root);
        return BenchPaths {
            node_db: root.join("db"),
            static_files: root.join("static_files"),
            rocksdb: root.join("rocksdb"),
            flashblocks_db: root.join("flashblocks").join("flashblocks.mdbx"),
        };
    }

    let datadir = NodeConfig::new(chain_spec).datadir();
    let chain_data_dir = datadir.data_dir();

    BenchPaths {
        node_db: datadir.db(),
        static_files: datadir.static_files(),
        rocksdb: datadir.rocksdb(),
        flashblocks_db: chain_data_dir.join("flashblocks").join("flashblocks.mdbx"),
    }
}

fn open_mainnet_provider(
    chain_spec: Arc<WorldChainSpec>,
    paths: &BenchPaths,
) -> eyre::Result<MainnetProvider> {
    let db = open_db_read_only(
        &paths.node_db,
        DatabaseArguments::new(ClientVersion::default()),
    )?;
    let static_files = StaticFileProviderBuilder::read_only(&paths.static_files).build()?;
    let rocksdb = RocksDBProvider::builder(&paths.rocksdb)
        .with_default_tables()
        .with_read_only(true)
        .build()?;
    let factory = ProviderFactory::<NodeTypesWithDBAdapter<MainnetNode, DatabaseEnv>>::new(
        db,
        chain_spec,
        static_files,
        rocksdb,
        RethRuntime::test(),
    )?;

    Ok(BlockchainProvider::new(factory)?)
}

fn load_flashblocks_for_block(path: PathBuf, block_number: u64) -> eyre::Result<StoredFlashblocks> {
    let db = open_db_read_only(path, DatabaseArguments::default())?;
    let tx = db.tx()?;
    let payload_id = tx
        .get::<tables::BlockNumberToPayloadId>(block_number)?
        .ok_or_else(|| eyre!("no payload id stored for block {block_number}"))?;
    let mut cursor = tx.cursor_read::<tables::FlashblocksByPayloadIdAndIndex>()?;

    let mut stored = cursor
        .walk(Some(StoredFlashblockKey::new(payload_id, 0)))?
        .take_while(|row| {
            row.as_ref()
                .map_or(true, |(key, _)| key.payload_id() == payload_id)
        })
        .map(|row| row.map_err(|err| eyre!(err)))
        .collect::<eyre::Result<Vec<_>>>()?;

    if stored.is_empty() {
        bail!("no flashblocks stored for payload id {payload_id}");
    }

    stored.sort_unstable_by_key(|(key, _)| key.index());

    let mut flashblocks = Vec::with_capacity(stored.len());
    for (expected_index, (key, stored_flashblock)) in stored.into_iter().enumerate() {
        ensure!(
            key.index() == expected_index as u64,
            "flashblock sequence for payload id {payload_id} is not contiguous: expected key index {}, got {}",
            expected_index,
            key.index()
        );

        let flashblock = decode_stored_flashblock(key.index(), stored_flashblock)?;

        ensure!(
            flashblock.payload_id == payload_id.payload_id(),
            "flashblock payload id {} does not match block index payload id {}",
            flashblock.payload_id,
            payload_id
        );
        ensure!(
            flashblock.index == expected_index as u64,
            "flashblock sequence for payload id {payload_id} is not contiguous: expected index {}, got {}",
            expected_index,
            flashblock.index
        );

        flashblocks.push(flashblock);
    }

    let base = flashblocks
        .first()
        .and_then(|flashblock| flashblock.base.as_ref())
        .ok_or_else(|| eyre!("first stored flashblock for block {block_number} has no base"))?;
    ensure!(
        base.block_number == block_number,
        "base flashblock block number {} does not match requested block {block_number}",
        base.block_number
    );

    Ok(StoredFlashblocks { flashblocks })
}

fn decode_stored_flashblock(
    stored_index: u64,
    stored_flashblock: StoredFlashblockPayload,
) -> eyre::Result<FlashblocksPayloadV1> {
    let mut payload_rlp = stored_flashblock.payload_rlp().as_ref();
    let flashblock = FlashblocksPayloadV1::decode(&mut payload_rlp)?;

    ensure!(
        payload_rlp.is_empty(),
        "stored flashblock index {stored_index} has trailing RLP bytes"
    );
    ensure!(
        flashblock.index == stored_index,
        "stored flashblock index {stored_index} does not match decoded payload index {}",
        flashblock.index
    );

    Ok(flashblock)
}

fn assert_parent_header_is_available(
    provider: &MainnetProvider,
    flashblocks: &[FlashblocksPayloadV1],
) -> eyre::Result<()> {
    let base = flashblocks
        .first()
        .and_then(|flashblock| flashblock.base.as_ref())
        .ok_or_else(|| eyre!("first stored flashblock has no base"))?;

    // This is the most useful early failure: the benchmark requires the node DB
    // to already contain the parent state of the measured block.
    ensure!(
        provider.sealed_header_by_hash(base.parent_hash)?.is_some(),
        "node database does not contain parent header {} for block {}",
        base.parent_hash,
        base.block_number
    );

    Ok(())
}

fn bench_process_flashblock_mainnet_block(c: &mut Criterion) {
    let rt = tokio::runtime::Runtime::new().expect("failed to build tokio runtime");
    let chain_spec = world_chain_mainnet_spec();
    let paths = bench_paths(chain_spec.clone());
    let evm_config = WorldChainEvmConfig::new(chain_spec.clone(), OpRethReceiptBuilder::default());
    let provider = open_mainnet_provider(chain_spec.clone(), &paths)
        .expect("failed to open default World Chain mainnet node database");
    let stored = load_flashblocks_for_block(paths.flashblocks_db, BENCH_BLOCK_NUMBER)
        .expect("failed to load stored flashblocks from default flashblocks database");
    assert_parent_header_is_available(&provider, &stored.flashblocks)
        .expect("node database is not synced to the measured block parent");

    let mut group = c.benchmark_group("flashblock_validation_process_flashblock_mainnet");
    group.sample_size(SAMPLE_SIZE);

    group.bench_function(BenchmarkId::new("block", BENCH_BLOCK_NUMBER), |b| {
        b.iter_batched(
            || {
                let (coordinator, handle, pending_tx) = fresh_coordinator(&rt);
                (
                    coordinator,
                    handle,
                    pending_tx,
                    Arc::new(FlashblockValidationMetrics::default()),
                )
            },
            |(coordinator, _handle, pending_tx, metrics)| {
                for flashblock in &stored.flashblocks {
                    process_flashblock(
                        provider.clone(),
                        &evm_config,
                        &coordinator,
                        chain_spec.clone(),
                        flashblock.clone(),
                        pending_tx.clone(),
                        metrics.clone(),
                    )
                    .expect("process_flashblock failed");
                }

                let processed = coordinator.flashblocks();
                assert_eq!(processed.flashblocks().len(), stored.flashblocks.len());
                assert_eq!(
                    coordinator.last().flashblock().index,
                    stored
                        .flashblocks
                        .last()
                        .expect("stored flashblocks are non-empty")
                        .index
                );
            },
            BatchSize::SmallInput,
        );
    });

    group.finish();
}

criterion_group!(benches, bench_process_flashblock_mainnet_block);
criterion_main!(benches);

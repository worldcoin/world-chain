use std::{path::PathBuf, sync::Arc};

use alloy_rlp::Decodable;
use criterion::{BatchSize, BenchmarkId, Criterion, criterion_group, criterion_main};
use eyre::eyre::{bail, ensure, eyre};
use reth_chain_state::ExecutedBlock;
use reth_chainspec::ForkCondition;
use reth_db::{ClientVersion, Database, DatabaseEnv, mdbx::DatabaseArguments, open_db_read_only};
use reth_db_api::{cursor::DbDupCursorRO, transaction::DbTx};
use reth_node_api::NodeTypesWithDBAdapter;
use reth_node_core::node_config::NodeConfig;
use reth_optimism_chainspec::{OpChainSpec, OpHardfork, WORLDCHAIN_MAINNET};
use reth_optimism_evm::{OpEvmConfig, OpRethReceiptBuilder};
use reth_optimism_primitives::OpPrimitives;
use reth_provider::{
    HeaderProvider, ProviderFactory,
    providers::{BlockchainProvider, RocksDBProvider, StaticFileProviderBuilder},
};
use reth_tasks::Runtime as RethRuntime;
use world_chain_builder::{
    coordinator::{FlashblocksExecutionCoordinator, process_flashblock},
    flashblock_validation_metrics::FlashblockValidationMetrics,
};
use world_chain_node::{context::WorldChainDefaultContext, node::WorldChainNode};
use world_chain_p2p::protocol::{
    handler::FlashblocksHandle,
    recorder::{StoredFlashblock, tables},
};
use world_chain_primitives::{ed25519_dalek::SigningKey, primitives::FlashblocksPayloadV1};

/// Block loaded from the default flashblocks recorder database.
///
/// Change this constant locally when switching the captured mainnet block used
/// by the bench. There are intentionally no CLI flags or environment variables
/// so Criterion runs are deterministic and easy to compare.
const BENCH_BLOCK_NUMBER: u64 = 100;
const SAMPLE_SIZE: usize = 10;
const JOVIAN_UPGRADE_TIMESTAMP_MAINNET: u64 = 1777593600;

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

fn world_chain_mainnet_spec() -> Arc<OpChainSpec> {
    let mut chain_spec = WORLDCHAIN_MAINNET.clone();

    // Match the production CLI behavior for World Chain mainnet. Keeping the
    // hardfork schedule aligned matters because validation builds the next EVM
    // environment from the parent header and chain spec.
    Arc::make_mut(&mut chain_spec).inner.hardforks.insert(
        OpHardfork::Jovian,
        ForkCondition::Timestamp(JOVIAN_UPGRADE_TIMESTAMP_MAINNET),
    );

    chain_spec
}

fn default_bench_paths(chain_spec: Arc<OpChainSpec>) -> BenchPaths {
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
    chain_spec: Arc<OpChainSpec>,
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
    let mut cursor = tx.cursor_dup_read::<tables::Flashblocks>()?;

    let mut stored = cursor
        .walk_dup(Some(payload_id), None)?
        .map(|row| row.map(|(_, value)| value).map_err(|err| eyre!(err)))
        .collect::<eyre::Result<Vec<_>>>()?;

    if stored.is_empty() {
        bail!("no flashblocks stored for payload id {payload_id}");
    }

    stored.sort_unstable_by_key(|stored| stored.index);

    let mut flashblocks = Vec::with_capacity(stored.len());
    for (expected_index, stored_flashblock) in stored.into_iter().enumerate() {
        let flashblock = decode_stored_flashblock(stored_flashblock)?;

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
    stored_flashblock: StoredFlashblock,
) -> eyre::Result<FlashblocksPayloadV1> {
    let stored_index = stored_flashblock.index;
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
    let paths = default_bench_paths(chain_spec.clone());
    let evm_config = OpEvmConfig::new(chain_spec.clone(), OpRethReceiptBuilder::default());
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

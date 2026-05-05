use criterion::{BenchmarkId, Criterion, criterion_group, criterion_main};
use futures::StreamExt;
use reth_chain_state::ExecutedBlock;
use reth_optimism_node::utils::optimism_payload_attributes;
use reth_optimism_primitives::OpPrimitives;
use reth_provider::{StateProvider, StateProviderFactory};
use std::sync::Arc;
use world_chain_node::context::WorldChainDefaultContext;
use world_chain_test_utils::{
    builder::{
        CHAIN_SPEC, EVM_CONFIG, build_flashblock_fixture_eth_transfers_with_provider,
        build_flashblock_fixture_fib_with_provider,
        build_flashblock_fixture_world_id_like_bn254_with_provider,
        build_flashblock_sequence_fixture_eth_transfers_with_provider,
        build_flashblock_sequence_fixture_fib_with_provider,
        build_flashblock_sequence_fixture_world_id_like_bn254_with_provider,
    },
    e2e_harness::setup::setup_with_block_uncompressed_size_limit,
};

use world_chain_builder::{
    coordinator::{FlashblocksExecutionCoordinator, process_flashblock, run_flashblock_processor},
    flashblock_validation_metrics::FlashblockValidationMetrics,
};

use world_chain_p2p::protocol::handler::FlashblocksHandle;
use world_chain_primitives::{ed25519_dalek::SigningKey, primitives::FlashblocksPayloadV1};

const TX_COUNTS: [usize; 3] = [50, 500, 1000];
const WORLD_ID_TX_COUNTS: [usize; 3] = [10, 25, 50];
const FLASHBLOCK_SEQUENCE_PARAMS: [(usize, usize); 3] = [
    (4, 50),  // 200 total txs across 4 flashblocks
    (4, 125), // 500 total txs across 4 flashblocks
    (4, 250), // 1000 total txs across 4 flashblocks
];
const WORLD_ID_FLASHBLOCK_SEQUENCE_PARAMS: [(usize, usize); 3] = [
    (4, 5),  // 20 total txs across 4 flashblocks
    (4, 10), // 40 total txs across 4 flashblocks
    (4, 12), // 48 total txs across 4 flashblocks
];

/// Helper: creates a fresh coordinator + handle + watch channel.
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

fn bench_process_flashblock_case<F>(
    c: &mut Criterion,
    group_name: &str,
    is_bal_enabled: bool,
    tx_counts: &[usize],
    build_flashblock: F,
) where
    F: Fn(&dyn StateProvider, usize, bool) -> FlashblocksPayloadV1,
{
    let rt = tokio::runtime::Runtime::new().expect("failed to build tokio runtime");
    let mut group = c.benchmark_group(group_name);
    group.sample_size(20);

    for &tx_count in tx_counts {
        // Spin up a real world-chain testing node first so that the fixture is
        // built against the same state database that `process_flashblock` will
        // later execute against. Using the mock `TestStateProvider` here would
        // produce state roots that disagree with what the live node computes.
        let (_, nodes, _, _, _) = rt
            .block_on(setup_with_block_uncompressed_size_limit::<
                WorldChainDefaultContext,
            >(
                1,
                optimism_payload_attributes,
                true,
                None,
                CHAIN_SPEC.clone(),
            ))
            .unwrap();
        let node = &nodes[0];
        let provider = node.node.inner.provider().clone();
        let latest = provider
            .latest()
            .expect("failed to obtain latest state provider from node");

        let flashblock = build_flashblock(latest.as_ref(), tx_count, is_bal_enabled);

        group.bench_with_input(BenchmarkId::new("txs", tx_count), &tx_count, |b, &_n| {
            b.iter(|| {
                let (coordinator, _handle, pending_tx) = fresh_coordinator(&rt);
                process_flashblock(
                    provider.clone(),
                    &EVM_CONFIG,
                    &coordinator,
                    CHAIN_SPEC.clone(),
                    flashblock.clone(),
                    pending_tx,
                    Arc::new(FlashblockValidationMetrics::default()),
                )
                .expect("process_flashblock failed");
            });
        });
    }

    group.finish();
}

fn bench_launch_flashblock_sequence_case<F>(
    c: &mut Criterion,
    group_name: &str,
    is_bal_enabled: bool,
    sequence_params: &[(usize, usize)],
    build_sequence: F,
) where
    F: Fn(&dyn StateProvider, usize, usize, bool) -> Vec<FlashblocksPayloadV1>,
{
    let rt = tokio::runtime::Runtime::new().expect("failed to build tokio runtime");
    let mut group = c.benchmark_group(group_name);
    group.sample_size(20);

    for &(num_fb, txs_per_fb) in sequence_params {
        // Spin up a real world-chain testing node first so that the sequence
        // fixture is built against the same state database that
        // `run_flashblock_processor` will later execute against. Using the mock
        // `BenchProvider` here would produce state roots that disagree with
        // what the live node computes and would make this bench duplicate the
        // synthetic one in `coordinator.rs`.
        let (_, nodes, _, _, _) = rt
            .block_on(setup_with_block_uncompressed_size_limit::<
                WorldChainDefaultContext,
            >(
                1,
                optimism_payload_attributes,
                true,
                None,
                CHAIN_SPEC.clone(),
            ))
            .unwrap();
        let node = &nodes[0];
        let provider = node.node.inner.provider().clone();
        let latest = provider
            .latest()
            .expect("failed to obtain latest state provider from node");

        let sequence = build_sequence(latest.as_ref(), num_fb, txs_per_fb, is_bal_enabled);
        let label = format!("{num_fb}fb_x_{txs_per_fb}tx");

        group.bench_function(BenchmarkId::new("stream", &label), |b| {
            b.iter(|| {
                let (coordinator, handle, pending_tx) = fresh_coordinator(&rt);
                let coordinator = Arc::new(coordinator);
                let pending_tx_clone = pending_tx.clone();

                // Create the stream first - this subscribes to the broadcast channel.
                // Bound to N+1 items: 1 initial Canon(genesis) + N Pending flashblocks.
                let stream = handle
                    .event_stream(provider.clone(), move |event| {
                        FlashblocksExecutionCoordinator::event_hook(event, &pending_tx_clone)
                    })
                    .take(sequence.len() + 1);

                // Send flashblocks on the broadcast channel. The subscription
                // was created above, so these are buffered until the stream is polled.
                for fb in &sequence {
                    handle.flashblocks_tx().send(fb.clone()).ok();
                }

                // Drive through the same function that `launch` calls.
                rt.block_on(run_flashblock_processor(
                    coordinator.clone(),
                    stream,
                    provider.clone(),
                    EVM_CONFIG.clone(),
                    CHAIN_SPEC.clone(),
                    pending_tx,
                ));

                let processed = coordinator.flashblocks();
                assert_eq!(processed.flashblocks().len(), sequence.len());
                assert_eq!(
                    coordinator.last().flashblock().index,
                    sequence.last().expect("sequence is non-empty").index
                );
            });
        });
    }

    group.finish();
}

// ---------------------------------------------------------------------------
// Single flashblock benchmark
// ---------------------------------------------------------------------------

fn bench_process_flashblock_eth_transfers_live(c: &mut Criterion) {
    bench_process_flashblock_case(
        c,
        "flashblock_validation_process_flashblock_eth_transfers_live",
        false,
        &TX_COUNTS,
        |p: &dyn StateProvider, n, b| build_flashblock_fixture_eth_transfers_with_provider(p, n, b),
    );
}

fn bench_process_flashblock_eth_transfers_with_bal_live(c: &mut Criterion) {
    bench_process_flashblock_case(
        c,
        "flashblock_validation_process_flashblock_eth_transfers_with_bal_live",
        true,
        &TX_COUNTS,
        |p: &dyn StateProvider, n, b| build_flashblock_fixture_eth_transfers_with_provider(p, n, b),
    );
}

fn bench_process_flashblock_fib_live(c: &mut Criterion) {
    bench_process_flashblock_case(
        c,
        "flashblock_validation_process_flashblock_fib_live",
        false,
        &TX_COUNTS,
        |p: &dyn StateProvider, n, b| build_flashblock_fixture_fib_with_provider(p, n, b),
    );
}

fn bench_process_flashblock_fib_with_bal_live(c: &mut Criterion) {
    bench_process_flashblock_case(
        c,
        "flashblock_validation_process_flashblock_fib_with_bal_live",
        true,
        &TX_COUNTS,
        |p: &dyn StateProvider, n, b| build_flashblock_fixture_fib_with_provider(p, n, b),
    );
}

fn bench_process_flashblock_world_id_like_bn254_live(c: &mut Criterion) {
    bench_process_flashblock_case(
        c,
        "flashblock_validation_process_flashblock_world_id_like_bn254_live",
        false,
        &WORLD_ID_TX_COUNTS,
        |p: &dyn StateProvider, n, b| {
            build_flashblock_fixture_world_id_like_bn254_with_provider(p, n, b)
        },
    );
}

fn bench_process_flashblock_world_id_like_bn254_with_bal_live(c: &mut Criterion) {
    bench_process_flashblock_case(
        c,
        "flashblock_validation_process_flashblock_world_id_like_bn254_with_bal_live",
        true,
        &WORLD_ID_TX_COUNTS,
        |p: &dyn StateProvider, n, b| {
            build_flashblock_fixture_world_id_like_bn254_with_provider(p, n, b)
        },
    );
}

// ---------------------------------------------------------------------------
// Multi-flashblock via launch path (stream-driven through run_flashblock_processor)
// ---------------------------------------------------------------------------

fn bench_launch_flashblock_sequence_eth_transfers_live(c: &mut Criterion) {
    bench_launch_flashblock_sequence_case(
        c,
        "flashblock_validation_launch_flashblock_sequence_eth_transfers_live",
        false,
        &FLASHBLOCK_SEQUENCE_PARAMS,
        |p: &dyn StateProvider, num_fb, txs_per_fb, bal| {
            build_flashblock_sequence_fixture_eth_transfers_with_provider(
                p, num_fb, txs_per_fb, bal,
            )
        },
    );
}

fn bench_launch_flashblock_sequence_eth_transfers_with_bal_live(c: &mut Criterion) {
    bench_launch_flashblock_sequence_case(
        c,
        "flashblock_validation_launch_flashblock_sequence_eth_transfers_with_bal_live",
        true,
        &FLASHBLOCK_SEQUENCE_PARAMS,
        |p: &dyn StateProvider, num_fb, txs_per_fb, bal| {
            build_flashblock_sequence_fixture_eth_transfers_with_provider(
                p, num_fb, txs_per_fb, bal,
            )
        },
    );
}

fn bench_launch_flashblock_sequence_fib_live(c: &mut Criterion) {
    bench_launch_flashblock_sequence_case(
        c,
        "flashblock_validation_launch_flashblock_sequence_fib_live",
        false,
        &FLASHBLOCK_SEQUENCE_PARAMS,
        |p: &dyn StateProvider, num_fb, txs_per_fb, bal| {
            build_flashblock_sequence_fixture_fib_with_provider(p, num_fb, txs_per_fb, bal)
        },
    );
}

fn bench_launch_flashblock_sequence_fib_with_bal_live(c: &mut Criterion) {
    bench_launch_flashblock_sequence_case(
        c,
        "flashblock_validation_launch_flashblock_sequence_fib_with_bal_live",
        true,
        &FLASHBLOCK_SEQUENCE_PARAMS,
        |p: &dyn StateProvider, num_fb, txs_per_fb, bal| {
            build_flashblock_sequence_fixture_fib_with_provider(p, num_fb, txs_per_fb, bal)
        },
    );
}

fn bench_launch_flashblock_sequence_world_id_like_bn254_live(c: &mut Criterion) {
    bench_launch_flashblock_sequence_case(
        c,
        "flashblock_validation_launch_flashblock_sequence_world_id_like_bn254_live",
        false,
        &WORLD_ID_FLASHBLOCK_SEQUENCE_PARAMS,
        |p: &dyn StateProvider, num_fb, txs_per_fb, bal| {
            build_flashblock_sequence_fixture_world_id_like_bn254_with_provider(
                p, num_fb, txs_per_fb, bal,
            )
        },
    );
}

fn bench_launch_flashblock_sequence_world_id_like_bn254_with_bal_live(c: &mut Criterion) {
    bench_launch_flashblock_sequence_case(
        c,
        "flashblock_validation_launch_flashblock_sequence_world_id_like_bn254_with_bal_live",
        true,
        &WORLD_ID_FLASHBLOCK_SEQUENCE_PARAMS,
        |p: &dyn StateProvider, num_fb, txs_per_fb, bal| {
            build_flashblock_sequence_fixture_world_id_like_bn254_with_provider(
                p, num_fb, txs_per_fb, bal,
            )
        },
    );
}

criterion_group!(
    benches,
    bench_process_flashblock_eth_transfers_live,
    bench_process_flashblock_eth_transfers_with_bal_live,
    bench_process_flashblock_fib_live,
    bench_process_flashblock_fib_with_bal_live,
    bench_process_flashblock_world_id_like_bn254_live,
    bench_process_flashblock_world_id_like_bn254_with_bal_live,
    bench_launch_flashblock_sequence_eth_transfers_live,
    bench_launch_flashblock_sequence_eth_transfers_with_bal_live,
    bench_launch_flashblock_sequence_fib_live,
    bench_launch_flashblock_sequence_fib_with_bal_live,
    bench_launch_flashblock_sequence_world_id_like_bn254_live,
    bench_launch_flashblock_sequence_world_id_like_bn254_with_bal_live,
);
criterion_main!(benches);

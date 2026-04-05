use std::sync::Arc;

use criterion::{BenchmarkId, Criterion, criterion_group, criterion_main};
use futures::StreamExt;
use reth_chain_state::ExecutedBlock;
use reth_optimism_primitives::OpPrimitives;
use world_chain_test_utils::builder::{
    BenchProvider, CHAIN_SPEC, EVM_CONFIG, build_flashblock_fixture_eth_transfers,
    build_flashblock_fixture_fib, build_flashblock_fixture_world_id_like_bn254,
    build_flashblock_sequence_fixture_eth_transfers, build_flashblock_sequence_fixture_fib,
    build_flashblock_sequence_fixture_world_id_like_bn254,
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
    F: Fn(usize, bool) -> FlashblocksPayloadV1,
{
    let rt = tokio::runtime::Runtime::new().expect("failed to build tokio runtime");
    let mut group = c.benchmark_group(group_name);
    group.sample_size(20);

    for &tx_count in tx_counts {
        let flashblock = build_flashblock(tx_count, is_bal_enabled);
        let provider = BenchProvider::new();

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
    F: Fn(usize, usize, bool) -> Vec<FlashblocksPayloadV1>,
{
    let rt = tokio::runtime::Runtime::new().expect("failed to build tokio runtime");
    let mut group = c.benchmark_group(group_name);
    group.sample_size(20);

    for &(num_fb, txs_per_fb) in sequence_params {
        let sequence = build_sequence(num_fb, txs_per_fb, is_bal_enabled);
        let provider = BenchProvider::new();
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

fn bench_process_flashblock_eth_transfers(c: &mut Criterion) {
    bench_process_flashblock_case(
        c,
        "process_flashblock_eth_transfers",
        false,
        &TX_COUNTS,
        build_flashblock_fixture_eth_transfers,
    );
}

fn bench_process_flashblock_eth_transfers_with_bal(c: &mut Criterion) {
    bench_process_flashblock_case(
        c,
        "process_flashblock_eth_transfers_with_bal",
        true,
        &TX_COUNTS,
        build_flashblock_fixture_eth_transfers,
    );
}

fn bench_process_flashblock_fib(c: &mut Criterion) {
    bench_process_flashblock_case(
        c,
        "process_flashblock_fib",
        false,
        &TX_COUNTS,
        build_flashblock_fixture_fib,
    );
}

fn bench_process_flashblock_fib_with_bal(c: &mut Criterion) {
    bench_process_flashblock_case(
        c,
        "process_flashblock_fib_with_bal",
        true,
        &TX_COUNTS,
        build_flashblock_fixture_fib,
    );
}

fn bench_process_flashblock_world_id_like_bn254(c: &mut Criterion) {
    bench_process_flashblock_case(
        c,
        "process_flashblock_world_id_like_bn254",
        false,
        &WORLD_ID_TX_COUNTS,
        build_flashblock_fixture_world_id_like_bn254,
    );
}

fn bench_process_flashblock_world_id_like_bn254_with_bal(c: &mut Criterion) {
    bench_process_flashblock_case(
        c,
        "process_flashblock_world_id_like_bn254_with_bal",
        true,
        &WORLD_ID_TX_COUNTS,
        build_flashblock_fixture_world_id_like_bn254,
    );
}

// ---------------------------------------------------------------------------
// Multi-flashblock via launch path (stream-driven through run_flashblock_processor)
// ---------------------------------------------------------------------------

fn bench_launch_flashblock_sequence_eth_transfers(c: &mut Criterion) {
    bench_launch_flashblock_sequence_case(
        c,
        "launch_flashblock_sequence_eth_transfers",
        false,
        &FLASHBLOCK_SEQUENCE_PARAMS,
        build_flashblock_sequence_fixture_eth_transfers,
    );
}

fn bench_launch_flashblock_sequence_eth_transfers_with_bal(c: &mut Criterion) {
    bench_launch_flashblock_sequence_case(
        c,
        "launch_flashblock_sequence_eth_transfers_with_bal",
        true,
        &FLASHBLOCK_SEQUENCE_PARAMS,
        build_flashblock_sequence_fixture_eth_transfers,
    );
}

fn bench_launch_flashblock_sequence_fib(c: &mut Criterion) {
    bench_launch_flashblock_sequence_case(
        c,
        "launch_flashblock_sequence_fib",
        false,
        &FLASHBLOCK_SEQUENCE_PARAMS,
        build_flashblock_sequence_fixture_fib,
    );
}

fn bench_launch_flashblock_sequence_fib_with_bal(c: &mut Criterion) {
    bench_launch_flashblock_sequence_case(
        c,
        "launch_flashblock_sequence_fib_with_bal",
        true,
        &FLASHBLOCK_SEQUENCE_PARAMS,
        build_flashblock_sequence_fixture_fib,
    );
}

fn bench_launch_flashblock_sequence_world_id_like_bn254(c: &mut Criterion) {
    bench_launch_flashblock_sequence_case(
        c,
        "launch_flashblock_sequence_world_id_like_bn254",
        false,
        &WORLD_ID_FLASHBLOCK_SEQUENCE_PARAMS,
        build_flashblock_sequence_fixture_world_id_like_bn254,
    );
}

fn bench_launch_flashblock_sequence_world_id_like_bn254_with_bal(c: &mut Criterion) {
    bench_launch_flashblock_sequence_case(
        c,
        "launch_flashblock_sequence_world_id_like_bn254_with_bal",
        true,
        &WORLD_ID_FLASHBLOCK_SEQUENCE_PARAMS,
        build_flashblock_sequence_fixture_world_id_like_bn254,
    );
}

criterion_group!(
    benches,
    bench_process_flashblock_eth_transfers,
    bench_process_flashblock_eth_transfers_with_bal,
    bench_process_flashblock_fib,
    bench_process_flashblock_fib_with_bal,
    bench_process_flashblock_world_id_like_bn254,
    bench_process_flashblock_world_id_like_bn254_with_bal,
    bench_launch_flashblock_sequence_eth_transfers,
    bench_launch_flashblock_sequence_eth_transfers_with_bal,
    bench_launch_flashblock_sequence_fib,
    bench_launch_flashblock_sequence_fib_with_bal,
    bench_launch_flashblock_sequence_world_id_like_bn254,
    bench_launch_flashblock_sequence_world_id_like_bn254_with_bal,
);
criterion_main!(benches);

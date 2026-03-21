use std::sync::Arc;

use criterion::{BenchmarkId, Criterion, criterion_group, criterion_main};
use futures::StreamExt;
use reth_chain_state::ExecutedBlock;
use reth_optimism_primitives::OpPrimitives;
use world_chain_builder::coordinator::{
    FlashblocksExecutionCoordinator, process_flashblock, run_flashblock_processor,
};
use world_chain_p2p::protocol::handler::FlashblocksHandle;
use world_chain_primitives::ed25519_dalek::SigningKey;
use world_chain_test_utils::builder::{
    BenchProvider, CHAIN_SPEC, EVM_CONFIG, build_flashblock_fixture,
    build_flashblock_sequence_fixture,
};

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

// ---------------------------------------------------------------------------
// Single flashblock benchmark
// ---------------------------------------------------------------------------

fn bench_process_flashblock(c: &mut Criterion) {
    let rt = tokio::runtime::Runtime::new().expect("failed to build tokio runtime");
    let mut group = c.benchmark_group("process_flashblock");
    group.sample_size(20);

    for tx_count in [50, 500, 1000] {
        let flashblock = build_flashblock_fixture(tx_count);
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
                )
                .expect("process_flashblock failed");
            });
        });
    }
    group.finish();
}

// ---------------------------------------------------------------------------
// Multi-flashblock via launch path (stream-driven through run_flashblock_processor)
// ---------------------------------------------------------------------------

fn bench_launch_flashblock_sequence(c: &mut Criterion) {
    let rt = tokio::runtime::Runtime::new().expect("failed to build tokio runtime");
    let mut group = c.benchmark_group("launch_flashblock_sequence");
    group.sample_size(20);

    let params: &[(usize, usize)] = &[
        (4, 50),  // 200 total txs across 4 flashblocks
        (4, 125), // 500 total txs across 4 flashblocks
        (4, 250), // 1000 total txs across 4 flashblocks
    ];

    for &(num_fb, txs_per_fb) in params {
        let sequence = build_flashblock_sequence_fixture(num_fb, txs_per_fb);
        let provider = BenchProvider::new();
        let label = format!("{}fb_x_{}tx", num_fb, txs_per_fb);

        group.bench_function(BenchmarkId::new("stream", &label), |b| {
            b.iter(|| {
                let (coordinator, handle, pending_tx) = fresh_coordinator(&rt);
                let pending_tx_clone = pending_tx.clone();

                // Create the stream first --- this subscribes to the broadcast channel.
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
                    Arc::new(coordinator),
                    stream,
                    provider.clone(),
                    EVM_CONFIG.clone(),
                    CHAIN_SPEC.clone(),
                    pending_tx,
                ));
            });
        });
    }
    group.finish();
}

criterion_group!(
    benches,
    bench_process_flashblock,
    bench_launch_flashblock_sequence
);
criterion_main!(benches);

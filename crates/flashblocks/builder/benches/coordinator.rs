use criterion::{BenchmarkId, Criterion, criterion_group, criterion_main};
use flashblocks_builder::test_utils::{
    BenchProvider, CHAIN_SPEC, EVM_CONFIG, build_flashblock_fixture_eth_transfers,
    build_flashblock_fixture_fib, build_flashblock_sequence_fixture_eth_transfers,
    build_flashblock_sequence_fixture_fib,
};
use flashblocks_engine::{
    pending_validator::PendingFlashblockValidator,
    processor::{PendingBlockExecutionContext, process_flashblock},
};
use flashblocks_p2p::protocol::handler::FlashblocksHandle;
use flashblocks_primitives::{ed25519_dalek::SigningKey, primitives::FlashblocksPayloadV1};
use futures::StreamExt;
use tokio::task::JoinSet;

const TX_COUNTS: [usize; 3] = [50, 500, 1000];
const FLASHBLOCK_SEQUENCE_PARAMS: [(usize, usize); 3] = [
    (4, 50),  // 200 total txs across 4 flashblocks
    (4, 125), // 500 total txs across 4 flashblocks
    (4, 250), // 1000 total txs across 4 flashblocks
];

fn make_validator() -> PendingFlashblockValidator<BenchProvider> {
    PendingFlashblockValidator::new(CHAIN_SPEC.clone(), EVM_CONFIG.clone(), BenchProvider::new())
}

fn bench_process_flashblock_case<F>(
    c: &mut Criterion,
    group_name: &str,
    is_bal_enabled: bool,
    build_flashblock: F,
) where
    F: Fn(usize, bool) -> FlashblocksPayloadV1,
{
    let rt = tokio::runtime::Runtime::new().expect("failed to build tokio runtime");
    let mut group = c.benchmark_group(group_name);
    group.sample_size(20);

    for tx_count in TX_COUNTS {
        let flashblock = build_flashblock(tx_count, is_bal_enabled);
        let provider = BenchProvider::new();
        let validator = make_validator();

        group.bench_with_input(BenchmarkId::new("txs", tx_count), &tx_count, |b, &_n| {
            b.iter(|| {
                let _guard = rt.enter();
                let mut ctx = PendingBlockExecutionContext::default();
                let mut trie_warmup = JoinSet::new();

                process_flashblock(
                    &mut ctx,
                    &validator,
                    &provider,
                    &None,
                    &mut trie_warmup,
                    flashblock.clone(),
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
    build_sequence: F,
) where
    F: Fn(usize, usize, bool) -> Vec<FlashblocksPayloadV1>,
{
    let rt = tokio::runtime::Runtime::new().expect("failed to build tokio runtime");
    let mut group = c.benchmark_group(group_name);
    group.sample_size(20);

    for (num_fb, txs_per_fb) in FLASHBLOCK_SEQUENCE_PARAMS {
        let sequence = build_sequence(num_fb, txs_per_fb, is_bal_enabled);
        let provider = BenchProvider::new();
        let label = format!("{num_fb}fb_x_{txs_per_fb}tx");

        group.bench_function(BenchmarkId::new("stream", &label), |b| {
            b.iter(|| {
                let _guard = rt.enter();
                let sk = SigningKey::from_bytes(&[1u8; 32]);
                let vk = sk.verifying_key();
                let handle = FlashblocksHandle::new(vk, Some(sk));
                let validator = make_validator();

                // Create the stream — subscribes to the broadcast channel.
                let stream = handle
                    .event_stream(
                        provider.clone(),
                        |_: &flashblocks_p2p::protocol::event::WorldChainEvent<()>| None,
                    )
                    .take(sequence.len() + 1);

                // Buffer flashblocks into the broadcast channel.
                for fb in &sequence {
                    handle.flashblocks_tx().send(fb.clone()).ok();
                }

                // Drive the stream through the same logic the processor uses.
                rt.block_on(flashblocks_engine::processor::run_flashblock_processor(
                    &validator, stream, &provider, &None,
                ));
            });
        });
    }

    group.finish();
}

// ---------------------------------------------------------------------------
// Single flashblock benchmarks
// ---------------------------------------------------------------------------

fn bench_process_flashblock_eth_transfers(c: &mut Criterion) {
    bench_process_flashblock_case(
        c,
        "process_flashblock_eth_transfers",
        false,
        build_flashblock_fixture_eth_transfers,
    );
}

fn bench_process_flashblock_eth_transfers_with_bal(c: &mut Criterion) {
    bench_process_flashblock_case(
        c,
        "process_flashblock_eth_transfers_with_bal",
        true,
        build_flashblock_fixture_eth_transfers,
    );
}

fn bench_process_flashblock_fib(c: &mut Criterion) {
    bench_process_flashblock_case(
        c,
        "process_flashblock_fib",
        false,
        build_flashblock_fixture_fib,
    );
}

fn bench_process_flashblock_fib_with_bal(c: &mut Criterion) {
    bench_process_flashblock_case(
        c,
        "process_flashblock_fib_with_bal",
        true,
        build_flashblock_fixture_fib,
    );
}

// ---------------------------------------------------------------------------
// Multi-flashblock (stream-driven)
// ---------------------------------------------------------------------------

fn bench_launch_flashblock_sequence_eth_transfers(c: &mut Criterion) {
    bench_launch_flashblock_sequence_case(
        c,
        "launch_flashblock_sequence_eth_transfers",
        false,
        build_flashblock_sequence_fixture_eth_transfers,
    );
}

fn bench_launch_flashblock_sequence_eth_transfers_with_bal(c: &mut Criterion) {
    bench_launch_flashblock_sequence_case(
        c,
        "launch_flashblock_sequence_eth_transfers_with_bal",
        true,
        build_flashblock_sequence_fixture_eth_transfers,
    );
}

fn bench_launch_flashblock_sequence_fib(c: &mut Criterion) {
    bench_launch_flashblock_sequence_case(
        c,
        "launch_flashblock_sequence_fib",
        false,
        build_flashblock_sequence_fixture_fib,
    );
}

fn bench_launch_flashblock_sequence_fib_with_bal(c: &mut Criterion) {
    bench_launch_flashblock_sequence_case(
        c,
        "launch_flashblock_sequence_fib_with_bal",
        true,
        build_flashblock_sequence_fixture_fib,
    );
}

criterion_group!(
    benches,
    bench_process_flashblock_eth_transfers,
    bench_process_flashblock_eth_transfers_with_bal,
    bench_process_flashblock_fib,
    bench_process_flashblock_fib_with_bal,
    bench_launch_flashblock_sequence_eth_transfers,
    bench_launch_flashblock_sequence_eth_transfers_with_bal,
    bench_launch_flashblock_sequence_fib,
    bench_launch_flashblock_sequence_fib_with_bal,
);
criterion_main!(benches);

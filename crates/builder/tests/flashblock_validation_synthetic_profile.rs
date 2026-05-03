use std::sync::Arc;

use reth_chain_state::ExecutedBlock;
use world_chain_primitives::OpPrimitives;
use world_chain_builder::{
    coordinator::{FlashblocksExecutionCoordinator, process_flashblock},
    flashblock_validation_metrics::FlashblockValidationMetrics,
};
use world_chain_p2p::protocol::handler::FlashblocksHandle;
use world_chain_primitives::{ed25519_dalek::SigningKey, primitives::FlashblocksPayloadV1};
use world_chain_test_utils::builder::{
    BenchProvider, CHAIN_SPEC, EVM_CONFIG, build_flashblock_fixture_eth_transfers,
    build_flashblock_fixture_fib, build_flashblock_fixture_world_id_like_bn254,
};

const MAX_DEFAULT_TX_COUNT: usize = 1000;
const MAX_WORLD_ID_LIKE_BN254_TX_COUNT: usize = 50;

fn fresh_coordinator(
    rt: &tokio::runtime::Runtime,
) -> (
    FlashblocksExecutionCoordinator,
    tokio::sync::watch::Receiver<Option<ExecutedBlock<OpPrimitives>>>,
    tokio::sync::watch::Sender<Option<ExecutedBlock<OpPrimitives>>>,
) {
    let _guard = rt.enter();
    let sk = SigningKey::from_bytes(&[1u8; 32]);
    let vk = sk.verifying_key();
    let handle = FlashblocksHandle::new(vk, Some(sk));
    let (pending_tx, pending_rx) =
        tokio::sync::watch::channel::<Option<ExecutedBlock<OpPrimitives>>>(None);
    let coordinator = FlashblocksExecutionCoordinator::new(handle, pending_tx.clone());
    (coordinator, pending_rx, pending_tx)
}

#[test]
#[ignore = "profiling harness for samply; run explicitly"]
fn profile_process_flashblock_eth_transfers_with_bal_1000_txs() {
    run_profile_process_flashblock_case(
        MAX_DEFAULT_TX_COUNT,
        true,
        build_flashblock_fixture_eth_transfers,
    );
}

#[test]
#[ignore = "profiling harness for samply; run explicitly"]
fn profile_process_flashblock_eth_transfers_without_bal_1000_txs() {
    run_profile_process_flashblock_case(
        MAX_DEFAULT_TX_COUNT,
        false,
        build_flashblock_fixture_eth_transfers,
    );
}

#[test]
#[ignore = "profiling harness for samply; run explicitly"]
fn profile_process_flashblock_fib_with_bal_1000_txs() {
    run_profile_process_flashblock_case(MAX_DEFAULT_TX_COUNT, true, build_flashblock_fixture_fib);
}

#[test]
#[ignore = "profiling harness for samply; run explicitly"]
fn profile_process_flashblock_fib_without_bal_1000_txs() {
    run_profile_process_flashblock_case(MAX_DEFAULT_TX_COUNT, false, build_flashblock_fixture_fib);
}

#[test]
#[ignore = "profiling harness for samply; run explicitly"]
fn profile_process_flashblock_world_id_like_bn254_with_bal_50_txs() {
    run_profile_process_flashblock_case(
        MAX_WORLD_ID_LIKE_BN254_TX_COUNT,
        true,
        build_flashblock_fixture_world_id_like_bn254,
    );
}

#[test]
#[ignore = "profiling harness for samply; run explicitly"]
fn profile_process_flashblock_world_id_like_bn254_without_bal_50_txs() {
    run_profile_process_flashblock_case(
        MAX_WORLD_ID_LIKE_BN254_TX_COUNT,
        false,
        build_flashblock_fixture_world_id_like_bn254,
    );
}

fn run_profile_process_flashblock_case<F>(num_txs: usize, bal: bool, build_flashblock: F)
where
    F: Fn(usize, bool) -> FlashblocksPayloadV1,
{
    let rt = tokio::runtime::Runtime::new().expect("failed to build tokio runtime");
    let provider = BenchProvider::new();
    let flashblock = build_flashblock(num_txs, bal);
    let expected_index = flashblock.index;
    let (coordinator, pending_rx, pending_tx) = fresh_coordinator(&rt);

    process_flashblock(
        provider,
        &EVM_CONFIG,
        &coordinator,
        CHAIN_SPEC.clone(),
        flashblock,
        pending_tx,
        Arc::new(FlashblockValidationMetrics::default()),
    )
    .expect("process_flashblock failed");

    assert_eq!(coordinator.flashblocks().flashblocks().len(), 1);
    assert_eq!(coordinator.last().flashblock.index, expected_index);
    assert!(
        pending_rx.borrow().is_some(),
        "pending block should be published"
    );
}

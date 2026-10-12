use std::sync::Arc;

use alloy_eip7928::{AccountChanges, BalanceChange, BlockAccessIndex};
use alloy_primitives::{Address, U256};
use reth_chain_state::ExecutedBlock;
use reth_optimism_primitives::OpPrimitives;
use reth_provider::StateRootProvider;
use reth_trie_common::HashedPostState;
use world_chain_evm::execution::bal::{BalExecutorError, BalValidationError};
use world_chain_p2p::protocol::handler::FlashblocksHandle;
use world_chain_primitives::{
    access_list::access_list_hash, ed25519_dalek::SigningKey, primitives::FlashblocksPayloadV1,
};
use world_chain_test_utils::builder::{
    BenchProvider, CHAIN_SPEC, EVM_CONFIG, build_flashblock_fixture_eth_transfers,
    build_flashblock_sequence_fixture_eth_transfers,
};
use world_chain_validator::{
    coordinator::{FlashblocksExecutionCoordinator, process_flashblock},
    flashblock_validation_metrics::FlashblockValidationMetrics,
};

fn test_coordinator() -> (
    FlashblocksExecutionCoordinator,
    tokio::sync::watch::Sender<Option<ExecutedBlock<OpPrimitives>>>,
    tokio::sync::watch::Receiver<Option<ExecutedBlock<OpPrimitives>>>,
) {
    let sk = SigningKey::from_bytes(&[1u8; 32]);
    let handle = FlashblocksHandle::new(sk.verifying_key(), Some(sk));
    let (pending_tx, pending_rx) = tokio::sync::watch::channel(None);
    let coordinator = FlashblocksExecutionCoordinator::new(handle, pending_tx.clone());
    (coordinator, pending_tx, pending_rx)
}

fn flashblock_with_injected_balance() -> FlashblocksPayloadV1 {
    let provider = BenchProvider::new();
    let mut flashblock = build_flashblock_fixture_eth_transfers(1, true);
    let (coordinator, pending_tx, _) = test_coordinator();
    process_flashblock(
        provider.clone(),
        &EVM_CONFIG,
        &coordinator,
        CHAIN_SPEC.clone(),
        flashblock.clone(),
        pending_tx,
        Arc::new(FlashblockValidationMetrics::default()),
    )
    .expect("honest flashblock is valid");
    let mut header = coordinator
        .latest_payload()
        .expect("built payload")
        .block()
        .header()
        .clone();

    let data = flashblock
        .diff
        .access_list_data
        .as_mut()
        .expect("BAL sidecar");
    let untouched = Address::repeat_byte(0xff);
    assert!(
        data.access_list
            .changes
            .iter()
            .all(|account| account.address != untouched)
    );
    data.access_list
        .changes
        .push(
            AccountChanges::new(untouched).with_balance_change(BalanceChange::new(
                BlockAccessIndex::new(data.access_list.max_tx_index),
                U256::from(10u64.pow(18)),
            )),
        );
    data.access_list
        .changes
        .sort_unstable_by_key(|account| account.address);
    assert_ne!(access_list_hash(&data.access_list), data.access_list_hash);

    // Match the forged state root and block hash while keeping the honest execution's BAL hash.
    let hashed_state = data
        .access_list
        .clone()
        .into_hashed_post_state(&provider, HashedPostState::default())
        .expect("claimed post-state");
    flashblock.diff.state_root = provider
        .state_root(hashed_state)
        .expect("claimed state root");
    assert_ne!(flashblock.diff.state_root, header.state_root);
    header.state_root = flashblock.diff.state_root;
    flashblock.diff.block_hash = header.hash_slow();
    flashblock
}

#[tokio::test]
async fn accepts_valid_flashblock_sequences_with_and_without_access_lists() {
    for bal_enabled in [false, true] {
        let (coordinator, pending_tx, pending_rx) = test_coordinator();
        let sequence = build_flashblock_sequence_fixture_eth_transfers(2, 1, bal_enabled);
        for flashblock in sequence {
            let expected_hash = flashblock.diff.block_hash;
            process_flashblock(
                BenchProvider::new(),
                &EVM_CONFIG,
                &coordinator,
                CHAIN_SPEC.clone(),
                flashblock,
                pending_tx.clone(),
                Arc::new(FlashblockValidationMetrics::default()),
            )
            .expect("valid flashblock");
            assert_eq!(
                coordinator
                    .latest_payload()
                    .expect("built payload")
                    .block()
                    .hash(),
                expected_hash
            );
            assert!(pending_rx.borrow().is_some());
        }
    }
}

#[tokio::test]
async fn rejects_unexecuted_balance_change_with_honest_access_list_hash() {
    let flashblock = flashblock_with_injected_balance();
    let data = flashblock
        .diff
        .access_list_data
        .as_ref()
        .expect("BAL sidecar");
    let expected_hash = data.access_list_hash;
    let received_hash = access_list_hash(&data.access_list);
    let (coordinator, pending_tx, pending_rx) = test_coordinator();
    let initial_flashblocks = coordinator.flashblocks();
    let (events_tx, mut events_rx) = tokio::sync::broadcast::channel(16);
    coordinator.register_payload_events(events_tx);

    let error = process_flashblock(
        BenchProvider::new(),
        &EVM_CONFIG,
        &coordinator,
        CHAIN_SPEC.clone(),
        flashblock,
        pending_tx,
        Arc::new(FlashblockValidationMetrics::default()),
    )
    .expect_err("unexecuted balance change must be rejected");

    let BalExecutorError::BalValidationError(error) = error.downcast().expect("BAL error") else {
        panic!("expected BAL validation error");
    };
    assert!(
        matches!(error.as_ref(), BalValidationError::ReceivedBalHashMismatch { expected, got }
        if *expected == expected_hash && *got == received_hash)
    );
    assert!(coordinator.latest_payload().is_none());
    assert!(pending_rx.borrow().is_none());
    assert_eq!(coordinator.flashblocks(), initial_flashblocks);
    assert!(matches!(
        events_rx.try_recv(),
        Err(tokio::sync::broadcast::error::TryRecvError::Empty)
    ));
}

#[tokio::test]
async fn rejects_unexecuted_balance_change_with_matching_received_hash() {
    let mut flashblock = flashblock_with_injected_balance();
    let data = flashblock
        .diff
        .access_list_data
        .as_mut()
        .expect("BAL sidecar");
    data.access_list_hash = access_list_hash(&data.access_list);
    let (coordinator, pending_tx, pending_rx) = test_coordinator();

    let error = process_flashblock(
        BenchProvider::new(),
        &EVM_CONFIG,
        &coordinator,
        CHAIN_SPEC.clone(),
        flashblock,
        pending_tx,
        Arc::new(FlashblockValidationMetrics::default()),
    )
    .expect_err("claimed changes must also match re-execution");

    let BalExecutorError::BalValidationError(error) = error.downcast().expect("BAL error") else {
        panic!("expected BAL validation error");
    };
    assert!(matches!(
        error.as_ref(),
        BalValidationError::BalHashMismatch { .. }
    ));
    assert!(coordinator.latest_payload().is_none());
    assert!(pending_rx.borrow().is_none());
}

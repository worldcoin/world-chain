use std::sync::Arc;

use alloy_eip7928::{
    AccountChanges, BalanceChange, BlockAccessIndex, BlockAccessListGasError, SlotChanges,
    StorageChange,
};
use alloy_primitives::{Address, U256};
use alloy_rpc_types_engine::PayloadId;
use reth_chain_state::ExecutedBlock;
use reth_optimism_primitives::OpPrimitives;
use reth_provider::StateRootProvider;
use reth_trie_common::HashedPostState;
use world_chain_evm::execution::bal::{BalExecutorError, BalValidationError, CommittedState};
use world_chain_p2p::protocol::{
    event::{ChainEvent, WorldChainEvent},
    handler::FlashblocksHandle,
};
use world_chain_primitives::{
    access_list::access_list_hash, ed25519_dalek::SigningKey, primitives::FlashblocksPayloadV1,
};
use world_chain_test_utils::builder::{
    BLOCK_EXECUTION_CTX, BenchProvider, CHAIN_SPEC, EVM_CONFIG, EVM_ENV, SEALED_HEADER,
    build_flashblock_fixture_eth_transfers, build_flashblock_sequence_fixture_eth_transfers,
};
use world_chain_validator::{
    coordinator::{FlashblocksExecutionCoordinator, process_flashblock, run_flashblock_processor},
    execution_strategy::{ExecutionStrategy, FlashblocksBalExecutionStrategy, ValidationCtx},
    flashblock_validation_metrics::{
        FlashblockValidationAttemptMetrics, FlashblockValidationMetrics,
    },
    state_root_strategy::SyncStateRootStrategy,
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
async fn processor_accepts_consecutive_bal_flashblocks() {
    let (coordinator, pending_tx, pending_rx) = test_coordinator();
    let coordinator = Arc::new(coordinator);
    let sequence = build_flashblock_sequence_fixture_eth_transfers(2, 1, true);
    let expected_hash = sequence.last().expect("second flashblock").diff.block_hash;
    let events = sequence
        .into_iter()
        .map(|flashblock| WorldChainEvent::<()>::Chain(ChainEvent::Pending(Arc::new(flashblock))));
    run_flashblock_processor(
        coordinator.clone(),
        futures::stream::iter(events),
        BenchProvider::new(),
        EVM_CONFIG.clone(),
        CHAIN_SPEC.clone(),
        pending_tx,
    )
    .await;

    assert_eq!(
        coordinator
            .latest_payload()
            .expect("second flashblock accepted")
            .block()
            .hash(),
        expected_hash
    );
    assert!(pending_rx.borrow().is_some());
}

#[tokio::test]
async fn processor_uses_epoch_gas_limit_for_continuation_with_embedded_base() {
    let (coordinator, pending_tx, pending_rx) = test_coordinator();
    let coordinator = Arc::new(coordinator);
    let mut sequence = build_flashblock_sequence_fixture_eth_transfers(2, 1, true);
    let mut conflicting_base = sequence[0].base.clone().expect("epoch base");
    conflicting_base.gas_limit = 0;
    sequence[1].base = Some(conflicting_base);
    let expected_hash = sequence[1].diff.block_hash;
    let events = sequence
        .into_iter()
        .map(|flashblock| WorldChainEvent::<()>::Chain(ChainEvent::Pending(Arc::new(flashblock))));

    run_flashblock_processor(
        coordinator.clone(),
        futures::stream::iter(events),
        BenchProvider::new(),
        EVM_CONFIG.clone(),
        CHAIN_SPEC.clone(),
        pending_tx,
    )
    .await;

    assert_eq!(
        coordinator
            .latest_payload()
            .expect("continuation accepted using the epoch base")
            .block()
            .hash(),
        expected_hash
    );
    assert_eq!(
        pending_rx
            .borrow()
            .as_ref()
            .expect("continuation published as pending")
            .recovered_block()
            .hash(),
        expected_hash
    );
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

fn rejected_bal_error(flashblock: FlashblocksPayloadV1) -> Box<BalValidationError> {
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
    .expect_err("invalid BAL must be rejected");

    let BalExecutorError::BalValidationError(error) = error.downcast().expect("BAL error") else {
        panic!("expected BAL validation error");
    };
    assert!(coordinator.latest_payload().is_none());
    assert!(pending_rx.borrow().is_none());
    assert_eq!(coordinator.flashblocks(), initial_flashblocks);
    assert!(matches!(
        events_rx.try_recv(),
        Err(tokio::sync::broadcast::error::TryRecvError::Empty)
    ));
    error
}

#[tokio::test]
async fn rejects_duplicate_accounts_through_reexecution() {
    let mut flashblock = build_flashblock_fixture_eth_transfers(1, true);
    let data = flashblock
        .diff
        .access_list_data
        .as_mut()
        .expect("BAL sidecar");
    data.access_list
        .changes
        .insert(0, data.access_list.changes[0].clone());
    data.access_list_hash = access_list_hash(&data.access_list);

    assert!(matches!(
        rejected_bal_error(flashblock).as_ref(),
        BalValidationError::BalHashMismatch { .. }
    ));
}

#[tokio::test]
async fn rejects_noncanonical_account_order_through_reexecution() {
    let mut flashblock = build_flashblock_fixture_eth_transfers(1, true);
    let data = flashblock
        .diff
        .access_list_data
        .as_mut()
        .expect("BAL sidecar");
    data.access_list.changes.reverse();
    data.access_list_hash = access_list_hash(&data.access_list);

    assert!(matches!(
        rejected_bal_error(flashblock).as_ref(),
        BalValidationError::BalHashMismatch { .. }
    ));
}

#[tokio::test]
async fn rejects_internal_index_above_the_flashblock_maximum_through_reexecution() {
    let mut flashblock = build_flashblock_fixture_eth_transfers(1, true);
    let data = flashblock
        .diff
        .access_list_data
        .as_mut()
        .expect("BAL sidecar");
    let index = data.access_list.max_tx_index + 1;
    let account = data
        .access_list
        .changes
        .iter_mut()
        .find(|account| !account.balance_changes.is_empty())
        .expect("balance changes");
    account
        .balance_changes
        .last_mut()
        .expect("balance change")
        .block_access_index = BlockAccessIndex::new(index);
    data.access_list_hash = access_list_hash(&data.access_list);

    assert!(matches!(
        rejected_bal_error(flashblock).as_ref(),
        BalValidationError::BalHashMismatch { .. }
    ));
}

#[tokio::test]
async fn rejects_minimum_index_that_does_not_match_committed_transactions() {
    let mut flashblock = build_flashblock_fixture_eth_transfers(1, true);
    let data = flashblock
        .diff
        .access_list_data
        .as_mut()
        .expect("BAL sidecar");
    data.access_list.min_tx_index = 1;
    data.access_list_hash = access_list_hash(&data.access_list);

    assert!(matches!(
        rejected_bal_error(flashblock).as_ref(),
        BalValidationError::BalIndexMismatch {
            index: "min",
            expected: 0,
            got: 1
        }
    ));
}

#[tokio::test]
async fn rejects_maximum_index_that_does_not_match_diff_transactions() {
    let mut flashblock = build_flashblock_fixture_eth_transfers(1, true);
    let data = flashblock
        .diff
        .access_list_data
        .as_mut()
        .expect("BAL sidecar");
    assert_eq!(data.access_list.max_tx_index, 2);
    data.access_list.max_tx_index = 3;
    data.access_list_hash = access_list_hash(&data.access_list);

    assert!(matches!(
        rejected_bal_error(flashblock).as_ref(),
        BalValidationError::BalIndexMismatch {
            index: "max",
            expected: 2,
            got: 3
        }
    ));
}

#[tokio::test]
async fn rejects_item_cost_above_the_block_gas_limit_before_hashing_or_parent_lookup() {
    let mut flashblock = build_flashblock_fixture_eth_transfers(1, true);
    let data = flashblock
        .diff
        .access_list_data
        .as_mut()
        .expect("BAL sidecar");
    let items = alloy_eip7928::total_bal_items(&data.access_list.changes);
    let gas_limit = items * alloy_eip7928::ITEM_COST as u64 - 1;
    data.access_list_hash = alloy_primitives::B256::ZERO;
    let base = flashblock.base.as_mut().expect("base payload");
    base.gas_limit = gas_limit;
    base.parent_hash = alloy_primitives::B256::repeat_byte(0xff);

    assert!(matches!(rejected_bal_error(flashblock).as_ref(),
        BalValidationError::BalGasLimitExceeded(error) if *error == BlockAccessListGasError::new(items, gas_limit)));
}

#[test]
fn rejects_read_write_overlap_through_reexecution_without_the_coordinator() {
    let mut flashblock = build_flashblock_fixture_eth_transfers(1, true);
    let data = flashblock
        .diff
        .access_list_data
        .as_mut()
        .expect("BAL sidecar");
    let account = &mut data.access_list.changes[0];
    let slot = U256::from(1);
    account.storage_reads = vec![slot];
    account.storage_changes = vec![SlotChanges::new(
        slot,
        vec![StorageChange::new(BlockAccessIndex::new(1), U256::from(1))],
    )];
    data.access_list_hash = access_list_hash(&data.access_list);
    let mut metrics = FlashblockValidationAttemptMetrics::default();

    let error = FlashblocksBalExecutionStrategy
        .execute(
            ValidationCtx {
                parent: &SEALED_HEADER,
                attempt_metrics: &mut metrics,
                chain_spec: CHAIN_SPEC.clone(),
                evm_env: EVM_ENV.clone(),
                execution_context: BLOCK_EXECUTION_CTX.clone(),
                state_root_strategy: SyncStateRootStrategy,
            },
            BenchProvider::new(),
            flashblock.diff,
            CommittedState::try_from(None).expect("no previous flashblock"),
            PayloadId::new([0; 8]),
        )
        .expect_err("the received sidecar must match canonical re-execution");

    let BalExecutorError::BalValidationError(error) = error else {
        panic!("expected BAL validation error");
    };
    assert!(matches!(
        error.as_ref(),
        BalValidationError::BalHashMismatch { .. }
    ));
}

#[test]
fn rejects_excessive_item_cost_without_the_coordinator() {
    let flashblock = build_flashblock_fixture_eth_transfers(1, true);
    let data = flashblock
        .diff
        .access_list_data
        .as_ref()
        .expect("BAL sidecar");
    let items = alloy_eip7928::total_bal_items(&data.access_list.changes);
    let gas_limit = items * alloy_eip7928::ITEM_COST as u64 - 1;
    let mut evm_env = EVM_ENV.clone();
    evm_env.block_env.gas_limit = gas_limit;
    let mut metrics = FlashblockValidationAttemptMetrics::default();

    let error = FlashblocksBalExecutionStrategy
        .execute(
            ValidationCtx {
                parent: &SEALED_HEADER,
                attempt_metrics: &mut metrics,
                chain_spec: CHAIN_SPEC.clone(),
                evm_env,
                execution_context: BLOCK_EXECUTION_CTX.clone(),
                state_root_strategy: SyncStateRootStrategy,
            },
            BenchProvider::new(),
            flashblock.diff,
            CommittedState::try_from(None).expect("no previous flashblock"),
            PayloadId::new([0; 8]),
        )
        .expect_err("direct execution must enforce the BAL item budget");

    let BalExecutorError::BalValidationError(error) = error else {
        panic!("expected BAL validation error");
    };
    assert!(matches!(error.as_ref(),
        BalValidationError::BalGasLimitExceeded(error) if *error == BlockAccessListGasError::new(items, gas_limit)));
}

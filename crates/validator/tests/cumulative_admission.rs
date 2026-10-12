use alloy_consensus::BlockHeader;
use alloy_eips::Encodable2718;
use alloy_op_evm::block::OpBlockExecutionError;
use alloy_primitives::{Address, B256, U256};
use alloy_rpc_types_engine::PayloadId;
use op_revm::constants::{
    DA_FOOTPRINT_GAS_SCALAR_OFFSET, DA_FOOTPRINT_GAS_SCALAR_SLOT, L1_BLOCK_CONTRACT,
};
use reth_evm::{
    block::{BlockExecutionError, BlockValidationError},
    execute::BlockBuilderOutcome,
};
use reth_optimism_node::OpBuiltPayload;
use reth_optimism_primitives::{OpPrimitives, OpTransactionSigned};
use reth_primitives_traits::Recovered;
use revm_database::BundleState;
use world_chain_evm::execution::bal::{BalExecutorError, CommittedState};
use world_chain_primitives::{
    access_list::FlashblockAccessListData, primitives::ExecutionPayloadFlashblockDeltaV1,
};
use world_chain_test_utils::builder::{
    ALICE, BLOCK_EXECUTION_CTX, CHAIN_SPEC, EVM_ENV, GENESIS, SEALED_HEADER, TestStateProvider,
    TxOp, execute_serial_with_provider,
};
use world_chain_validator::{
    execution_strategy::{
        ExecutionStrategy, FlashblocksBalExecutionStrategy, FlashblocksLegacyExecutionStrategy,
        ValidationCtx,
    },
    flashblock_validation_metrics::FlashblockValidationAttemptMetrics,
    state_root_strategy::SyncStateRootStrategy,
};

use ValidationPath::{Parallel, Sequential};

const TX_GAS_LIMIT: u64 = 100_000;
const TRANSFER_GAS_USED: u64 = 21_000;
// The second transfer can declare 100,000 gas after the first has consumed 21,000.
const EXACT_GAS_BLOCK_LIMIT: u64 = TRANSFER_GAS_USED + TX_GAS_LIMIT;

#[test]
fn rejects_insufficient_gas_within_one_flashblock() {
    let provider = TestStateProvider::new();
    let transactions = two_transfers();
    let flashblock = build_flashblock(&provider, &transactions, None);
    assert_eq!(flashblock.outcome.block.gas_used(), 2 * TRANSFER_GAS_USED);
    let block_gas_limit = EXACT_GAS_BLOCK_LIMIT - 1;

    for path in [Sequential, Parallel] {
        let error = validate(
            &provider,
            &flashblock,
            block_gas_limit,
            path,
            initial_state(),
        )
        .expect_err("the second transaction declares more gas than remains");
        assert_gas_limit_error(error, TX_GAS_LIMIT, TX_GAS_LIMIT - 1);
    }
}

#[test]
fn accepts_exact_gas_limit_within_one_flashblock() {
    let provider = TestStateProvider::new();
    let transactions = two_transfers();
    let flashblock = build_flashblock(&provider, &transactions, None);

    for path in [Sequential, Parallel] {
        validate(
            &provider,
            &flashblock,
            EXACT_GAS_BLOCK_LIMIT,
            path,
            initial_state(),
        )
        .expect("the second transaction's declared gas exactly fits the remaining budget");
    }
}

#[test]
fn rejects_insufficient_gas_after_previous_flashblock() {
    let provider = TestStateProvider::new();
    let transactions = two_transfers();
    let first = build_flashblock(&provider, &transactions[..1], None);
    let second = build_flashblock(&provider, &transactions[1..], Some(&first));
    let block_gas_limit = EXACT_GAS_BLOCK_LIMIT - 1;

    for path in [Sequential, Parallel] {
        let committed = commit_first_flashblock(&provider, &first, block_gas_limit, path);
        let error = validate(&provider, &second, block_gas_limit, path, committed)
            .expect_err("the previous flashblock leaves only 99,999 gas available");
        assert_gas_limit_error(error, TX_GAS_LIMIT, TX_GAS_LIMIT - 1);
    }
}

#[test]
fn accepts_exact_gas_limit_after_previous_flashblock() {
    let provider = TestStateProvider::new();
    let transactions = two_transfers();
    let first = build_flashblock(&provider, &transactions[..1], None);
    let second = build_flashblock(&provider, &transactions[1..], Some(&first));

    for path in [Sequential, Parallel] {
        let committed = commit_first_flashblock(&provider, &first, EXACT_GAS_BLOCK_LIMIT, path);
        validate(&provider, &second, EXACT_GAS_BLOCK_LIMIT, path, committed)
            .expect("the previous flashblock leaves exactly 100,000 gas available");
    }
}

#[test]
fn rejects_insufficient_da_within_one_flashblock() {
    let provider = provider_with_da_scalar(2000);
    let transactions = two_transfers();
    let flashblock = build_flashblock(&provider, &transactions, None);
    let total_da_footprint = flashblock
        .outcome
        .block
        .blob_gas_used()
        .expect("Jovian block");
    let block_gas_limit = total_da_footprint - 1;
    assert!(
        block_gas_limit >= EXACT_GAS_BLOCK_LIMIT,
        "compute gas must fit"
    );

    for path in [Sequential, Parallel] {
        let error = validate(
            &provider,
            &flashblock,
            block_gas_limit,
            path,
            initial_state(),
        )
        .expect_err("the two transactions exceed the DA budget by one");
        assert_da_limit_exceeded_by_one(error);
    }
}

#[test]
fn accepts_exact_da_limit_within_one_flashblock() {
    let provider = provider_with_da_scalar(2000);
    let transactions = two_transfers();
    let flashblock = build_flashblock(&provider, &transactions, None);
    let block_gas_limit = flashblock
        .outcome
        .block
        .blob_gas_used()
        .expect("Jovian block");

    for path in [Sequential, Parallel] {
        validate(
            &provider,
            &flashblock,
            block_gas_limit,
            path,
            initial_state(),
        )
        .expect("the two transactions' total DA footprint exactly fits the block");
    }
}

#[test]
fn rejects_insufficient_da_after_previous_flashblock() {
    let provider = provider_with_da_scalar(2000);
    let transactions = two_transfers();
    let first = build_flashblock(&provider, &transactions[..1], None);
    let second = build_flashblock(&provider, &transactions[1..], Some(&first));
    let total_da_footprint = second.outcome.block.blob_gas_used().expect("Jovian block");
    let block_gas_limit = total_da_footprint - 1;
    assert!(
        block_gas_limit >= EXACT_GAS_BLOCK_LIMIT,
        "compute gas must fit"
    );

    for path in [Sequential, Parallel] {
        let committed = commit_first_flashblock(&provider, &first, block_gas_limit, path);
        let error = validate(&provider, &second, block_gas_limit, path, committed)
            .expect_err("the previous flashblock's DA usage must count toward the block budget");
        assert_da_limit_exceeded_by_one(error);
    }
}

#[test]
fn accepts_exact_da_limit_after_previous_flashblock() {
    let provider = provider_with_da_scalar(2000);
    let transactions = two_transfers();
    let first = build_flashblock(&provider, &transactions[..1], None);
    let second = build_flashblock(&provider, &transactions[1..], Some(&first));
    let block_gas_limit = second.outcome.block.blob_gas_used().expect("Jovian block");

    for path in [Sequential, Parallel] {
        let committed = commit_first_flashblock(&provider, &first, block_gas_limit, path);
        validate(&provider, &second, block_gas_limit, path, committed)
            .expect("both flashblocks' total DA footprint exactly fits the block");
    }
}

#[test]
fn uses_committed_pre_refund_gas_for_admission() {
    let provider = TestStateProvider::new();
    let transactions = two_transfers();
    let first = build_flashblock(&provider, &transactions[..1], None);
    let second = build_flashblock(&provider, &transactions[1..], Some(&first));

    for path in [Sequential, Parallel] {
        let mut committed = commit_first_flashblock(&provider, &first, EXACT_GAS_BLOCK_LIMIT, path);
        // Model one gas of prior post-exec refund: receipt gas is 21,000, real compute is 21,001.
        committed.evm_gas_used += 1;
        assert_eq!(committed.gas_used, TRANSFER_GAS_USED);
        assert_eq!(committed.evm_gas_used, TRANSFER_GAS_USED + 1);

        let error = validate(&provider, &second, EXACT_GAS_BLOCK_LIMIT, path, committed)
            .expect_err("the pre-refund compute budget leaves only 99,999 gas available");
        assert_gas_limit_error(error, TX_GAS_LIMIT, TX_GAS_LIMIT - 1);
    }
}

#[derive(Clone, Copy, Debug)]
enum ValidationPath {
    Sequential,
    Parallel,
}

struct FlashblockFixture {
    outcome: BlockBuilderOutcome<OpPrimitives>,
    access_list_data: FlashblockAccessListData,
    bundle: BundleState,
    transactions: Vec<Recovered<OpTransactionSigned>>,
}

fn two_transfers() -> [Recovered<OpTransactionSigned>; 2] {
    let transfer = TxOp::Transfer {
        from: ALICE.clone(),
        to: Address::repeat_byte(0x11),
        value: U256::from(100),
    };
    [
        transfer.to_signed_tx_with_gas_limit(0, TX_GAS_LIMIT),
        transfer.to_signed_tx_with_gas_limit(1, TX_GAS_LIMIT),
    ]
}

fn build_flashblock(
    provider: &TestStateProvider,
    transactions: &[Recovered<OpTransactionSigned>],
    previous: Option<&FlashblockFixture>,
) -> FlashblockFixture {
    let previous_outcome =
        previous.map(|fixture| (fixture.outcome.clone(), fixture.bundle.clone()));
    let (outcome, access_list_data, bundle) =
        execute_serial_with_provider(provider, previous_outcome, transactions)
            .expect("build reference flashblock with the fixture's generous gas limit");
    FlashblockFixture {
        outcome,
        access_list_data,
        bundle,
        transactions: transactions.to_vec(),
    }
}

fn initial_state() -> CommittedState {
    CommittedState::try_from(None).expect("no previous flashblock")
}

fn commit_first_flashblock(
    provider: &TestStateProvider,
    first: &FlashblockFixture,
    block_gas_limit: u64,
    path: ValidationPath,
) -> CommittedState {
    let payload = validate(provider, first, block_gas_limit, path, initial_state())
        .expect("the first flashblock fits within the block budget");
    CommittedState::try_from(Some(&payload)).expect("state committed by the first flashblock")
}

fn validate(
    provider: &TestStateProvider,
    fixture: &FlashblockFixture,
    block_gas_limit: u64,
    path: ValidationPath,
    committed: CommittedState,
) -> Result<OpBuiltPayload, BalExecutorError> {
    // Change only the gas limit and matching block hash, preserve the reference execution's roots.
    let mut header = fixture.outcome.block.header().clone();
    header.gas_limit = block_gas_limit;
    let diff = ExecutionPayloadFlashblockDeltaV1 {
        state_root: header.state_root,
        receipts_root: header.receipts_root,
        logs_bloom: header.logs_bloom,
        gas_used: header.gas_used,
        block_hash: header.hash_slow(),
        transactions: fixture
            .transactions
            .iter()
            .map(|tx| tx.encoded_2718().into())
            .collect(),
        withdrawals: vec![],
        withdrawals_root: header.withdrawals_root.unwrap_or_default(),
        access_list_data: Some(fixture.access_list_data.clone()),
    };
    let mut metrics = FlashblockValidationAttemptMetrics::default();
    let mut evm_env = EVM_ENV.clone();
    evm_env.block_env.gas_limit = block_gas_limit;
    let ctx = ValidationCtx {
        parent: &SEALED_HEADER,
        attempt_metrics: &mut metrics,
        chain_spec: CHAIN_SPEC.clone(),
        evm_env,
        execution_context: BLOCK_EXECUTION_CTX.clone(),
        state_root_strategy: SyncStateRootStrategy,
    };
    match path {
        Sequential => FlashblocksLegacyExecutionStrategy.execute(
            ctx,
            provider.clone(),
            diff,
            committed,
            PayloadId::default(),
        ),
        Parallel => FlashblocksBalExecutionStrategy.execute(
            ctx,
            provider.clone(),
            diff,
            committed,
            PayloadId::default(),
        ),
    }
}

fn provider_with_da_scalar(da_scalar: u16) -> TestStateProvider {
    let mut genesis = GENESIS.clone();
    let account = genesis.alloc.entry(L1_BLOCK_CONTRACT).or_default();
    account.nonce = Some(1);
    let mut scalar_slot = [0u8; 32];
    scalar_slot[DA_FOOTPRINT_GAS_SCALAR_OFFSET..DA_FOOTPRINT_GAS_SCALAR_OFFSET + 2]
        .copy_from_slice(&da_scalar.to_be_bytes());
    account.storage.get_or_insert_with(Default::default).insert(
        B256::from(DA_FOOTPRINT_GAS_SCALAR_SLOT),
        B256::from(scalar_slot),
    );
    TestStateProvider::from_genesis(&genesis)
}

fn assert_gas_limit_error(
    error: BalExecutorError,
    expected_tx_gas: u64,
    expected_remaining_gas: u64,
) {
    let BalExecutorError::BlockExecutionError(BlockExecutionError::Validation(
        BlockValidationError::TransactionGasLimitMoreThanAvailableBlockGas {
            transaction_gas_limit,
            block_available_gas,
        },
    )) = error
    else {
        panic!("expected cumulative gas admission error: {error:?}");
    };
    assert_eq!(transaction_gas_limit, expected_tx_gas);
    assert_eq!(block_available_gas, expected_remaining_gas);
}

fn assert_da_limit_exceeded_by_one(error: BalExecutorError) {
    let BalExecutorError::BlockExecutionError(BlockExecutionError::Validation(
        BlockValidationError::Other(error),
    )) = error
    else {
        panic!("expected cumulative DA admission error: {error:?}");
    };
    let Some(OpBlockExecutionError::TransactionDaFootprintAboveGasLimit {
        transaction_da_footprint,
        available_block_da_footprint,
    }) = error.downcast_ref::<OpBlockExecutionError>()
    else {
        panic!("expected DA footprint error: {error:?}");
    };
    assert_eq!(*transaction_da_footprint, *available_block_da_footprint + 1);
}

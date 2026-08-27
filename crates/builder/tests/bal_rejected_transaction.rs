use alloy_consensus::BlockHeader;
use alloy_op_evm::{OpBlockExecutor, OpEvmFactory, OpTx};
use alloy_primitives::U256;
use crossbeam_channel::bounded;
use reth_evm::{
    EvmFactory,
    block::{BlockExecutionError, BlockValidationError},
    execute::{BlockBuilder, BlockBuilderOutcome},
};
use reth_optimism_primitives::{OpPrimitives, OpTransactionSigned};
use reth_primitives_traits::Recovered;
use reth_revm::{State, database::StateProviderDatabase};
use revm::database::BundleState;
use world_chain_builder::payload_builder_metrics::PayloadBuildAttemptMetrics;
use world_chain_evm::{
    BlockBuilderExt, OpRethReceiptBuilder, execution::bal::BalBlockBuilder,
    utils::cache_prestate_from_bundle,
};
use world_chain_primitives::access_list::{FlashblockAccessListData, access_list_hash};
use world_chain_test_utils::builder::{
    ALICE, BLOCK_EXECUTION_CTX, BOB, CHAIN_SPEC, EVM_ENV, SEALED_HEADER, TestStateProvider, TxOp,
    create_test_state_provider, execute_serial_with_provider,
};

type TestResult<T> = Result<T, Box<dyn std::error::Error + Send + Sync>>;

fn finish_next_flashblock(
    state_provider: &TestStateProvider,
    previous: (BlockBuilderOutcome<OpPrimitives>, BundleState),
    attempted_transaction: Option<Recovered<OpTransactionSigned>>,
) -> TestResult<(BlockBuilderOutcome<OpPrimitives>, FlashblockAccessListData)> {
    let (previous_outcome, bundle) = previous;
    let previous_transactions = previous_outcome
        .block
        .clone_transactions_recovered()
        .collect();
    let previous_gas_used = previous_outcome.block.gas_used();
    let previous_receipts = previous_outcome.execution_result.receipts.clone();

    let db = StateProviderDatabase::new(state_provider);
    let mut state = State::builder()
        .with_database(db)
        .with_cached_prestate(cache_prestate_from_bundle(&bundle))
        .with_bundle_update()
        .with_bal_builder()
        .build();

    let evm = OpEvmFactory::<OpTx>::default().create_evm(&mut state, EVM_ENV.clone());
    let mut executor = OpBlockExecutor::new(
        evm,
        BLOCK_EXECUTION_CTX.clone(),
        CHAIN_SPEC.clone(),
        OpRethReceiptBuilder::default(),
    );
    executor.gas_used = previous_gas_used;
    executor.receipts = previous_receipts;

    let (access_list_tx, access_list_rx) = bounded(1);
    let mut builder = BalBlockBuilder::<OpRethReceiptBuilder, OpPrimitives, _>::new(
        BLOCK_EXECUTION_CTX.clone(),
        &SEALED_HEADER,
        executor,
        previous_transactions,
        CHAIN_SPEC.clone(),
        access_list_tx,
        bundle,
    );

    if let Some(transaction) = attempted_transaction {
        let error = builder
            .execute_transaction_with_result_closure(transaction, |_| {})
            .expect_err("the duplicate nonce transaction should be rejected");

        assert!(
            matches!(
                &error,
                BlockExecutionError::Validation(BlockValidationError::InvalidTx {
                    error,
                    ..
                }) if error.is_nonce_too_low()
            ),
            "expected a nonce-too-low validation error, got {error:?}"
        );
    }

    let (outcome, _) =
        builder.finish_with_bundle(state_provider, PayloadBuildAttemptMetrics::default())?;
    let access_list = access_list_rx.recv()?;
    let access_list_hash = access_list_hash(&access_list);

    Ok((
        outcome,
        FlashblockAccessListData {
            access_list,
            access_list_hash,
        },
    ))
}

#[test]
fn rejected_transaction_does_not_pollute_flashblock_access_list() -> TestResult<()> {
    let state_provider = create_test_state_provider();
    let transaction = TxOp::Transfer {
        from: ALICE.clone(),
        to: BOB.address(),
        value: U256::from(1),
    }
    .to_signed_tx(0);

    let (previous_outcome, _, previous_bundle) = execute_serial_with_provider(
        state_provider.as_ref(),
        None,
        std::slice::from_ref(&transaction),
    )?;
    let previous = (previous_outcome, previous_bundle);

    let (clean_outcome, clean_bal) =
        finish_next_flashblock(state_provider.as_ref(), previous.clone(), None)?;
    let (rejected_outcome, rejected_bal) =
        finish_next_flashblock(state_provider.as_ref(), previous, Some(transaction))?;

    assert_eq!(
        rejected_outcome.block.hash(),
        clean_outcome.block.hash(),
        "rejecting a candidate must not change the finished block"
    );
    assert_eq!(
        rejected_bal, clean_bal,
        "rejecting a candidate must not change the emitted access list"
    );

    Ok(())
}

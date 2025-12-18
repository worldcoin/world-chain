//! Property tests for chaos contract interactions and BAL validation.

use std::sync::Arc;

use alloy_primitives::U256;
use alloy_rpc_types_engine::PayloadId;
use flashblocks_builder::{
    bal_executor::CommittedState,
    bal_validator::{FlashblocksBlockValidator, decode_transactions_with_indices},
};
use flashblocks_primitives::primitives::ExecutionPayloadFlashblockDeltaV1;
use proptest::prelude::*;
use reth_optimism_evm::OpRethReceiptBuilder;
use reth_optimism_node::OpBuiltPayload;
use reth_provider::StateProvider;
use revm::database::BundleState;

use crate::fixtures::{
    BLOCK_EXECUTION_CTX, CHAIN_SPEC, EVM_CONFIG, EVM_ENV, SEALED_HEADER, create_test_state_provider,
};

/// Execute transactions in parallel using FlashblocksBlockValidator
pub fn validate(
    diff: &ExecutionPayloadFlashblockDeltaV1,
    committed_state: CommittedState<OpRethReceiptBuilder>,
) -> Result<OpBuiltPayload, Box<dyn std::error::Error + Send + Sync>> {
    let state_provider = create_test_state_provider();
    // The transaction offset is the number of previously committed transactions offset 1.
    let transactions_offset = committed_state.transactions.len() as u16 + 1;

    let executor_transactions =
        decode_transactions_with_indices(&diff.transactions, transactions_offset)?;

    let validator = FlashblocksBlockValidator::<OpRethReceiptBuilder> {
        chain_spec: CHAIN_SPEC.clone(),
        evm_config: EVM_CONFIG.clone(),
        execution_context: BLOCK_EXECUTION_CTX.clone(),
        evm_env: EVM_ENV.clone(),
        committed_state,
        executor_transactions,
    };

    let payload_id = PayloadId::default();
    let payload = validator.validate(
        state_provider as Arc<dyn StateProvider>,
        diff.clone(),
        &SEALED_HEADER,
        payload_id,
    )?;

    Ok(payload)
}

/// Creates a default CommittedState from an Option.
fn unwrap_committed_state(
    state: Option<CommittedState<OpRethReceiptBuilder>>,
) -> CommittedState<OpRethReceiptBuilder> {
    state.unwrap_or_else(|| CommittedState {
        gas_used: 0,
        fees: U256::ZERO,
        receipts: vec![],
        transactions: vec![],
        bundle: BundleState::default(),
    })
}

mod property_tests {
    use std::path::PathBuf;

    use flashblocks_builder::bal_executor::BalExecutorError;
    use proptest::{prelude::Strategy, prop_assert, proptest};

    use std::io::Write;

    use crate::{
        fixtures::{
            ALICE, BOB, ChaosTarget, TxOp, arb_execution_payload, arb_execution_payload_sequence,
            build_chained_payloads,
        },
        proptest::{unwrap_committed_state, validate},
    };

    pub fn debug_output<T: serde::Serialize>(value: &T) {
        let dir: PathBuf = env!("CARGO_MANIFEST_DIR").into();
        let path: PathBuf = dir.join("tests/proptest-regressions/debug_error.json");

        let file = std::fs::File::create(&path).expect("Unable to create debug output file");
        let mut writer = std::io::BufWriter::new(file);
        serde_json::to_writer_pretty(&mut writer, &value).expect("Unable to write debug output");
        writer.flush().expect("Unable to flush debug output");
    }

    /// Test that validates transactions across multiple flashblocks where the same sender
    /// makes transactions in both flashblocks. This tests that nonce handling is correct
    /// when the validator processes the second flashblock with committed state from the first.
    #[test]
    fn test_multi_flashblock_same_sender() {
        // Create a sequence where ALICE sends transactions in both flashblocks
        // Flashblock 1: ALICE sends tx with nonce 0, 1, 2, 3
        // Flashblock 2: ALICE sends tx with nonce 4, 5, 6, 7
        let sequence: Vec<(TxOp, u64)> = vec![
            // Flashblock 1 transactions
            (
                TxOp::Transfer {
                    from: ALICE.clone(),
                    to: alloy_primitives::Address::random(),
                    value: alloy_primitives::U256::from(100),
                },
                0,
            ),
            (
                TxOp::Transfer {
                    from: ALICE.clone(),
                    to: alloy_primitives::Address::random(),
                    value: alloy_primitives::U256::from(100),
                },
                1,
            ),
            (
                TxOp::Transfer {
                    from: ALICE.clone(),
                    to: alloy_primitives::Address::random(),
                    value: alloy_primitives::U256::from(100),
                },
                2,
            ),
            (
                TxOp::Transfer {
                    from: ALICE.clone(),
                    to: alloy_primitives::Address::random(),
                    value: alloy_primitives::U256::from(100),
                },
                3,
            ),
            // Flashblock 2 transactions
            (
                TxOp::Transfer {
                    from: ALICE.clone(),
                    to: alloy_primitives::Address::random(),
                    value: alloy_primitives::U256::from(100),
                },
                4,
            ),
            (
                TxOp::Transfer {
                    from: ALICE.clone(),
                    to: alloy_primitives::Address::random(),
                    value: alloy_primitives::U256::from(100),
                },
                5,
            ),
            (
                TxOp::Transfer {
                    from: ALICE.clone(),
                    to: alloy_primitives::Address::random(),
                    value: alloy_primitives::U256::from(100),
                },
                6,
            ),
            (
                TxOp::Transfer {
                    from: ALICE.clone(),
                    to: alloy_primitives::Address::random(),
                    value: alloy_primitives::U256::from(100),
                },
                7,
            ),
        ];

        // Build payloads with 2 flashblocks (4 txs each)
        let payloads = build_chained_payloads(sequence, 2).expect("Failed to build payloads");

        assert_eq!(payloads.len(), 2, "Expected 2 flashblocks");

        // Validate each flashblock
        for (i, (diff, committed_state)) in payloads.into_iter().enumerate() {
            let committed = unwrap_committed_state(committed_state);
            eprintln!(
                "Validating flashblock {} with {} committed txs",
                i,
                committed.transactions.len()
            );

            let result = validate(&diff, committed);
            if let Err(err) = &result {
                if let Some(e) = err.downcast_ref::<BalExecutorError>() {
                    debug_output(e);
                }

                panic!("Flashblock {} validation failed: {:?}", i, err);
            }
        }
    }

    /// Test with fib calls that involve storage operations across multiple flashblocks
    #[test]
    fn test_multi_flashblock_with_storage() {
        // Create a sequence where we call fib() which modifies storage
        let sequence: Vec<(TxOp, u64)> = vec![
            // Flashblock 1: ALICE calls fib(5) on the proxy, BOB does a transfer
            (
                TxOp::Fib {
                    from: ALICE.clone(),
                    n: 5,
                    target: ChaosTarget::Proxy,
                },
                0,
            ),
            (
                TxOp::Transfer {
                    from: BOB.clone(),
                    to: alloy_primitives::Address::random(),
                    value: alloy_primitives::U256::from(100),
                },
                0,
            ),
            // Flashblock 2: ALICE calls fib(8) on the proxy (should see storage from fib(5))
            (
                TxOp::Fib {
                    from: ALICE.clone(),
                    n: 8,
                    target: ChaosTarget::Proxy,
                },
                1,
            ),
            (
                TxOp::Transfer {
                    from: BOB.clone(),
                    to: alloy_primitives::Address::random(),
                    value: alloy_primitives::U256::from(100),
                },
                1,
            ),
        ];

        let payloads = build_chained_payloads(sequence, 2).expect("Failed to build payloads");

        assert_eq!(payloads.len(), 2, "Expected 2 flashblocks");

        for (i, (diff, committed_state)) in payloads.into_iter().enumerate() {
            let committed = unwrap_committed_state(committed_state);
            eprintln!(
                "Validating flashblock {} with {} committed txs",
                i,
                committed.transactions.len()
            );

            let result = validate(&diff, committed);
            if let Err(err) = &result {
                if let Some(e) = err.downcast_ref::<BalExecutorError>() {
                    debug_output(e);
                }

                panic!("Flashblock {} validation failed: {:?}", i, err);
            }
        }
    }

    proptest! {
                #![proptest_config(crate::proptest::ProptestConfig::with_cases(1))]

                #[test]
                fn prop_validate_many(payloads in (1usize..200, 1usize..20).prop_flat_map(|(max_txs, max_flashblocks)| { arb_execution_payload_sequence(max_txs, max_flashblocks) })) {
                    for (diff, committed_state) in payloads.into_iter() {
                        let committed = unwrap_committed_state(committed_state);
                        let payload = validate(&diff, committed);
                        if let Err(err) = &payload
                            && let Some(e) = err.downcast_ref::<BalExecutorError>() {
                                debug_output(e);
                            }
                        prop_assert!(payload.is_ok(), "Parallel execution failed: {:#?}", payload.err());
                    }
                }

                #[test]
                fn prop_validate_single(payload in arb_execution_payload(10)) {
                    let (diff, committed_state) = payload;
                    let committed = unwrap_committed_state(committed_state);
                    let payload = validate(&diff, committed);
                    if let Err(err) = &payload
                        && let Some(e) = err.downcast_ref::<BalExecutorError>() {
                            debug_output(e);
                        }
                    prop_assert!(payload.is_ok(), "Parallel execution failed: {:#?}", payload.err());
                }
    }
}

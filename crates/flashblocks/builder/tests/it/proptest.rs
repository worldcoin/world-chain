//! Property tests for chaos contract interactions and BAL validation.

use std::sync::Arc;

use alloy_primitives::U256;
use alloy_rpc_types_engine::PayloadId;
use flashblocks_builder::{
    executor::{BalExecutorError, CommittedState},
    validator::{
        FlashblockBlockValidator, FlashblocksBlockValidator, FlashblocksValidatorCtx,
        decode_transactions,
    },
};
use flashblocks_primitives::primitives::ExecutionPayloadFlashblockDeltaV1;
use proptest::prelude::*;
use reth_optimism_evm::OpRethReceiptBuilder;
use reth_optimism_node::OpBuiltPayload;
use reth_provider::StateProvider;
use revm::database::BundleState;

use crate::fixtures::{
    BLOCK_EXECUTION_CTX, CHAIN_SPEC, EVM_CONFIG, EVM_ENV, SEALED_HEADER, arb_execution_payload,
    arb_execution_payload_sequence, create_test_state_provider,
};

/// Execute transactions in parallel using FlashblocksBlockValidator
pub fn validate(
    diff: &ExecutionPayloadFlashblockDeltaV1,
    committed_state: CommittedState<OpRethReceiptBuilder>,
) -> Result<OpBuiltPayload, Box<dyn std::error::Error + Send + Sync>> {
    let state_provider = create_test_state_provider();
    let start_index = diff
        .access_list_data
        .as_ref()
        .unwrap()
        .access_list
        .min_tx_index;

    let executor_transactions = decode_transactions(&diff.transactions, start_index as u16)?;

    let validation_ctx = FlashblocksValidatorCtx {
        chain_spec: CHAIN_SPEC.clone(),
        evm_config: EVM_CONFIG.clone(),
        execution_context: BLOCK_EXECUTION_CTX.clone(),
        executor_transactions,
        committed_state,
        evm_env: EVM_ENV.clone(),
    };

    let validator = FlashblocksBlockValidator::<OpRethReceiptBuilder>::new(validation_ctx);

    let payload_id = PayloadId::new([0u8; 8]);
    let payload = validator.validate_flashblock_parallel(
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn debug_output<T: std::fmt::Debug>(value: &T) {
        let dir: PathBuf = env!("CARGO_MANIFEST_DIR").into();
        let file_path = dir.join("tests/proptest-regressions/debug_output.txt");
        std::fs::write(file_path, format!("{:#?}", value)).expect("Unable to write debug output");
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(1))]

        #[test]
        fn prop_validate_many(payloads in (1usize..200, 1usize..20).prop_flat_map(|(max_txs, max_flashblocks)| { arb_execution_payload_sequence(4, 2) })) {

            for (diff, committed_state) in payloads.into_iter(){
                let committed = unwrap_committed_state(committed_state);
                let payload = validate(&diff, committed);

                if let Err(e) = &payload {
                    if let Some(bal_err) = e.downcast_ref::<BalExecutorError>() {
                        debug_output(bal_err);
                    }
                }

                prop_assert!(payload.is_ok(), "Parallel execution failed: {:#?}", payload.err());

            }
        }

        #[test]
        fn prop_validate_single(payload in arb_execution_payload(10)) {
            let (diff, committed_state) = payload;
            let committed = unwrap_committed_state(committed_state);
            let payload = validate(&diff, committed);

            if let Err(e) = &payload {
                if let Some(bal_err) = e.downcast_ref::<BalExecutorError>() {
                    debug_output(bal_err);
                }
            }

            prop_assert!(payload.is_ok(), "Parallel execution failed: {:#?}", payload.err());
        }
    }
}

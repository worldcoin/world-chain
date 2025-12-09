//! Property tests for chaos contract interactions and BAL validation.

use std::sync::Arc;

use alloy_primitives::U256;
use alloy_rpc_types_engine::PayloadId;
use flashblocks_builder::{
    executor::CommittedState,
    validator::{FlashblocksBlockValidator, decode_transactions_with_indices},
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
    // min_tx_index is the start_index from BlockAccessIndexCounter, but actual
    // transaction indices start at start_index + 1 because next_index() increments
    // before returning.
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

    let payload_id = PayloadId::new([0u8; 8]);
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

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use flashblocks_builder::executor::BalExecutorError;
    use proptest::{prelude::Strategy, prop_assert, proptest};
    use std::io::Write;

    use crate::{
        fixtures::{arb_execution_payload, arb_execution_payload_sequence},
        proptest::{unwrap_committed_state, validate},
    };

    pub fn debug_output<T: std::fmt::Debug>(value: &T) {
        let dir: PathBuf = env!("CARGO_MANIFEST_DIR").into();
        let file_path = dir.join("tests/proptest-regressions/debug_output.txt");
        let file = std::fs::File::create(&file_path).expect("Unable to create debug output file");

        let mut writer = std::io::BufWriter::new(file);
        writeln!(writer, "{:#?}", value).expect("Unable to write debug output");
    }

    proptest! {
        #![proptest_config(crate::proptest::ProptestConfig::with_cases(1))]

        #[test]
        fn prop_validate_many(payloads in (1usize..50, 1usize..5).prop_flat_map(|(max_txs, max_flashblocks)| { arb_execution_payload_sequence(max_txs, max_flashblocks) })) {
            for (diff, committed_state) in payloads.into_iter(){
                let committed = unwrap_committed_state(committed_state);
                let payload = validate(&diff, committed);
                if let Err(err) = &payload {
                    let e = &err.downcast_ref::<BalExecutorError>();
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

            prop_assert!(payload.is_ok(), "Parallel execution failed: {:#?}", payload.err());
        }
    }
}

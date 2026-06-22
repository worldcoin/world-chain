use alloy_consensus::{
    Header,
    transaction::{Recovered, SignerRecoverable},
};
use alloy_eips::Decodable2718;
use reth_primitives_traits::SealedHeader;
use std::{marker::PhantomData, sync::Arc, time::Instant};

use alloy_primitives::Bytes;

use alloy_op_evm::OpBlockExecutionCtx;
use alloy_rpc_types_engine::PayloadId;
use eyre::eyre::bail;
use reth_chain_state::{DeferredTrieData, ExecutedBlock};
use world_chain_primitives::primitives::ExecutionPayloadFlashblockDeltaV1;

use reth_evm::{ConfigureEvm, EvmEnvFor};
use reth_node_api::{BuiltPayload, BuiltPayloadExecutedBlock};
use reth_optimism_evm::OpRethReceiptBuilder;
use reth_optimism_node::OpBuiltPayload;
use reth_optimism_primitives::{OpPrimitives, OpTransactionSigned};
use reth_provider::StateProviderFactory;
use tracing::error;
use world_chain_chainspec::WorldChainSpec;

use crate::{
    execution_strategy::{ExecutionStrategy, ValidationCtx},
    flashblock_validation_metrics::{
        FlashblockValidationAttemptMetrics, FlashblockValidationMetrics, metered_fn,
    },
    state_root_strategy::FlashblockTypes,
};
use world_chain_evm::{
    PayloadBuildStage,
    execution::bal::{BalExecutorError, CommittedState},
};

pub fn into_executed_payload(
    payload: BuiltPayloadExecutedBlock<OpPrimitives>,
) -> ExecutedBlock<OpPrimitives> {
    let trie_data = DeferredTrieData::sort(payload.hashed_state, payload.trie_updates);
    ExecutedBlock::new(payload.recovered_block, payload.execution_output, trie_data)
}

pub struct FlashblocksBlockValidator<Evm: ConfigureEvm, T: FlashblockTypes<Evm>> {
    pub chain_spec: Arc<WorldChainSpec>,
    pub evm_env: EvmEnvFor<Evm>,
    pub execution_context: OpBlockExecutionCtx,
    pub flashblock_validation_metrics: Arc<FlashblockValidationMetrics>,
    pub execution_strategy: T::Execution,
    _phantom: PhantomData<fn() -> T>,
}

impl<Evm: ConfigureEvm + Clone, T: FlashblockTypes<Evm>> FlashblocksBlockValidator<Evm, T> {
    pub fn new(
        chain_spec: Arc<WorldChainSpec>,
        evm_env: EvmEnvFor<Evm>,
        execution_context: OpBlockExecutionCtx,
        flashblock_validation_metrics: Arc<FlashblockValidationMetrics>,
        execution_strategy: T::Execution,
    ) -> Self {
        Self {
            chain_spec,
            evm_env,
            execution_context,
            flashblock_validation_metrics,
            execution_strategy,
            _phantom: PhantomData,
        }
    }

    pub fn validate_flashblock_with_state(
        &self,
        client: impl StateProviderFactory + Clone + Sync + 'static,
        diff: ExecutionPayloadFlashblockDeltaV1,
        parent: &SealedHeader<Header>,
        payload_id: PayloadId,
        state: Option<&OpBuiltPayload<OpPrimitives>>,
    ) -> Result<OpBuiltPayload, BalExecutorError> {
        let has_bal = diff.access_list_data.is_some();
        let diff_clone = diff.clone();
        let validation_started = Instant::now();
        let mut attempt_metrics = FlashblockValidationAttemptMetrics::default();

        let result = (|| -> Result<OpBuiltPayload, BalExecutorError> {
            let committed_state = CommittedState::<OpRethReceiptBuilder>::try_from(state)?;

            let next_payload = metered_fn(
                tracing::trace_span!(
                    target: "flashblocks::coordinator",
                    "validate",
                    id = %payload_id,
                    path = if has_bal { "bal" } else { "legacy" },
                    tx_count = diff.transactions.len(),
                    duration_ms = tracing::field::Empty,
                ),
                metrics::histogram!("flashblocks.validate", "access_list" => has_bal.to_string()),
                |_span| {
                    self.execution_strategy.execute(
                        ValidationCtx {
                            parent,
                            attempt_metrics: &mut attempt_metrics,
                            chain_spec: self.chain_spec.clone(),
                            evm_env: self.evm_env.clone(),
                            execution_context: self.execution_context.clone(),
                            state_root_strategy: T::StateRoot::default(),
                        },
                        client,
                        diff,
                        committed_state,
                        payload_id,
                    )
                },
            )?;

            let executed = next_payload
                .executed_block()
                .ok_or(BalExecutorError::MissingExecutedBlock)
                .inspect_err(|e| {
                    error!(
                        target: "flashblocks::coordinator",
                        ?payload_id,
                        error = ?e,
                        "Missing executed block for payload"
                    )
                })?;

            let executed = into_executed_payload(executed);
            self.validate_payload(&executed, diff_clone)
                .map_err(|e| BalExecutorError::Other(Box::from(e.to_string())))
                .inspect_err(|e| {
                    error!(
                        target: "flashblocks::coordinator",
                        ?payload_id,
                        error = ?e,
                        "Payload validation failed"
                    )
                })?;

            Ok(next_payload)
        })();

        match result {
            Ok(next_payload) => {
                attempt_metrics
                    .record_stage_duration(PayloadBuildStage::Total, validation_started.elapsed());
                attempt_metrics.publish(self.flashblock_validation_metrics.as_ref());
                Ok(next_payload)
            }
            Err(err) => {
                self.flashblock_validation_metrics
                    .increment_validation_errors();
                Err(err)
            }
        }
    }

    pub fn validate_payload(
        &self,
        block: &ExecutedBlock<OpPrimitives>,
        diff: ExecutionPayloadFlashblockDeltaV1,
    ) -> eyre::Result<()> {
        // make sure the block matches the diff
        if block.sealed_block().hash() != diff.block_hash {
            bail!(
                "Block hash mismatch: expected {}, got {}",
                diff.block_hash,
                block.sealed_block().hash()
            );
        }

        Ok(())
    }
}

/// Decodes transactions from raw bytes and recovers signer addresses.
pub fn decode_transactions_with_indices(
    encoded_transactions: &[Bytes],
    start_index: u16,
) -> Result<Vec<(u16, Recovered<OpTransactionSigned>)>, BalExecutorError> {
    encoded_transactions
        .iter()
        .enumerate()
        .map(|(i, tx)| {
            let tx_envelope = OpTransactionSigned::decode_2718(&mut tx.as_ref())
                .map_err(BalExecutorError::other)?;

            let signer = tx_envelope
                .recover_signer()
                .map_err(BalExecutorError::other)?;
            let recovered = Recovered::new_unchecked(tx_envelope, signer);

            Ok((start_index + i as u16, recovered))
        })
        .collect()
}

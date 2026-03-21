//! [`PendingPayloadValidator`] implementation for flashblock diff validation.
//!
//! Provides [`PendingFlashblockValidator`], which validates incoming flashblock
//! diffs against accumulated state using either the BAL (Block Access List)
//! parallel path or the legacy sequential path depending on whether the diff
//! includes access list data.
//!
//! # Architecture
//!
//! The validator dispatches to two paths:
//!
//! - **BAL path** (`access_list_data.is_some()`): Delegates to
//!   [`FlashblocksBlockValidator`] which performs parallel transaction execution
//!   using the temporal database and access list for dependency resolution.
//!
//! - **Legacy path** (`access_list_data.is_none()`): Builds the block through
//!   the standard [`build()`](flashblocks_builder::payload_builder::build)
//!   pipeline with `OpPayloadBuilderCtx`, executing transactions sequentially.
//!
//! Both paths return a [`ValidatedFlashblock`] containing the recovered block,
//! execution output, and unsorted trie inputs for deferred state root computation.

use std::sync::Arc;

use alloy_consensus::{BlockHeader, Header};
use alloy_eips::{Decodable2718, eip2718::WithEncoded, eip4895::Withdrawals};
use alloy_op_evm::OpBlockExecutionCtx;
use alloy_rpc_types_engine::PayloadId;
use flashblocks_builder::{
    bal_executor::CommittedState,
    bal_validator::{FlashblocksBlockValidator, decode_transactions_with_indices},
    payload_builder::build,
    traits::{context::OpPayloadBuilderCtxBuilder, context_builder::PayloadBuilderCtxBuilder},
};
use flashblocks_primitives::primitives::{
    ExecutionPayloadBaseV1, ExecutionPayloadFlashblockDeltaV1,
};
use op_alloy_consensus::{OpTxEnvelope, encode_holocene_extra_data};
use reth::{
    payload::EthPayloadBuilderAttributes,
    revm::{cancelled::CancelOnDrop, database::StateProviderDatabase},
};
use reth_basic_payload_builder::PayloadConfig;
use reth_chain_state::ExecutedBlock;
use reth_evm::ConfigureEvm;
use reth_node_api::BuiltPayload as _;
use reth_optimism_chainspec::OpChainSpec;
use reth_optimism_evm::{OpEvmConfig, OpNextBlockEnvAttributes, OpRethReceiptBuilder};
use reth_optimism_node::{OpBuiltPayload, OpPayloadBuilderAttributes};
use reth_optimism_primitives::OpPrimitives;
use reth_payload_util::BestPayloadTransactions;
use reth_primitives::SealedHeader;
use reth_provider::{ChainSpecProvider, HeaderProvider, StateProviderFactory};
use reth_transaction_pool::{EthPooledTransaction, noop::NoopTransactionPool};

use crate::validator::{
    DiffValidationOutcome, FlashblockValidationError, PendingPayloadValidator, ValidatedFlashblock,
};

/// Validates flashblock diffs using either the BAL parallel execution path or
/// the legacy sequential path.
///
/// This is the concrete [`PendingPayloadValidator`] implementation used by the
/// [`WorldChainPayloadProcessor`](crate::processor::WorldChainPayloadProcessor).
///
/// # Type Parameters
///
/// * `Provider` -- state provider factory for database and header access.
pub struct PendingFlashblockValidator<Provider> {
    /// Chain specification for the OP Stack chain.
    pub chain_spec: Arc<OpChainSpec>,
    /// EVM configuration used to derive block environments.
    pub evm_config: OpEvmConfig,
    /// State provider factory for database lookups and state root computation.
    pub provider: Provider,
}

impl<Provider> PendingFlashblockValidator<Provider> {
    pub fn new(chain_spec: Arc<OpChainSpec>, evm_config: OpEvmConfig, provider: Provider) -> Self {
        Self {
            chain_spec,
            evm_config,
            provider,
        }
    }
}

/// Extracts [`CommittedState`] from an optional [`ExecutedBlock`].
///
/// Adapts the pattern from `CommittedState::try_from(Option<&OpBuiltPayload>)` to
/// work with [`ExecutedBlock`] directly, extracting gas used, bundle state,
/// receipts, and transactions.
fn committed_state_from_block(
    committed: Option<&ExecutedBlock<OpPrimitives>>,
) -> CommittedState<OpRethReceiptBuilder> {
    let Some(block) = committed else {
        return CommittedState {
            gas_used: 0,
            fees: alloy_primitives::U256::ZERO,
            bundle: Default::default(),
            receipts: vec![],
            transactions: vec![],
        };
    };
    let gas_used = block.recovered_block().header().gas_used();
    let bundle = block.execution_output.state.clone();
    let transactions = block
        .recovered_block()
        .clone_transactions_recovered()
        .enumerate()
        .map(|(i, tx)| (i as u16, tx))
        .collect();
    let receipts = block
        .execution_output
        .result
        .receipts
        .iter()
        .cloned()
        .enumerate()
        .map(|(i, r)| (i as u16, r))
        .collect();
    CommittedState {
        gas_used,
        fees: alloy_primitives::U256::ZERO,
        bundle,
        receipts,
        transactions,
    }
}

/// Extracts a [`ValidatedFlashblock`] from an [`OpBuiltPayload`].
///
/// Unwraps the executed block from the payload and extracts the unsorted
/// hashed state and trie updates for deferred state root computation.
fn extract_validated(
    payload: OpBuiltPayload,
    anchor_hash: alloy_primitives::B256,
) -> Result<ValidatedFlashblock, FlashblockValidationError> {
    let executed = payload.executed_block().ok_or_else(|| {
        FlashblockValidationError::Other(Box::from("missing executed block in payload"))
    })?;
    let hashed_state = match executed.hashed_state {
        either::Left(unsorted) => unsorted,
        either::Right(_) => Arc::default(),
    };
    let trie_updates = match executed.trie_updates {
        either::Left(unsorted) => unsorted,
        either::Right(_) => Arc::default(),
    };
    Ok(ValidatedFlashblock {
        recovered_block: executed.recovered_block,
        execution_output: executed.execution_output,
        hashed_state,
        trie_updates,
        anchor_hash,
    })
}

impl<Provider> PendingPayloadValidator for PendingFlashblockValidator<Provider>
where
    Provider: StateProviderFactory
        + HeaderProvider<Header = alloy_consensus::Header>
        + ChainSpecProvider<ChainSpec = OpChainSpec>
        + Clone
        + Send
        + Sync
        + 'static,
{
    fn validate_diff(
        &self,
        committed: Option<&ExecutedBlock<OpPrimitives>>,
        diff: ExecutionPayloadFlashblockDeltaV1,
        parent: &SealedHeader<Header>,
        payload_id: PayloadId,
        base: &ExecutionPayloadBaseV1,
    ) -> DiffValidationOutcome {
        let committed_state = committed_state_from_block(committed);
        let anchor_hash = parent.hash();

        let execution_context = OpBlockExecutionCtx {
            parent_hash: base.parent_hash,
            parent_beacon_block_root: Some(base.parent_beacon_block_root),
            extra_data: base.extra_data.clone(),
        };

        let next_block_context = OpNextBlockEnvAttributes {
            timestamp: base.timestamp,
            suggested_fee_recipient: base.fee_recipient,
            prev_randao: base.prev_randao,
            gas_limit: base.gas_limit,
            parent_beacon_block_root: Some(base.parent_beacon_block_root),
            extra_data: base.extra_data.clone(),
        };

        let evm_env = self
            .evm_config
            .next_evm_env(parent.header(), &next_block_context)
            .map_err(FlashblockValidationError::other)?;

        let transactions_offset = committed_state.transactions.len() + 1;

        let payload = if diff.access_list_data.is_some() {
            let sealed_header = Arc::new(parent.clone());
            let executor_transactions =
                decode_transactions_with_indices(&diff.transactions, transactions_offset as u16)
                    .map_err(FlashblockValidationError::other)?;

            let block_validator = FlashblocksBlockValidator {
                chain_spec: self.chain_spec.clone(),
                evm_config: self.evm_config.clone(),
                execution_context,
                executor_transactions,
                committed_state,
                evm_env,
            };

            block_validator
                .validate(self.provider.clone(), diff, &sealed_header, payload_id)
                .map_err(FlashblockValidationError::other)?
        } else {
            self.validate_legacy(committed_state, diff, parent, payload_id, base)?
        };

        extract_validated(payload, anchor_hash)
    }
}

impl<Provider> PendingFlashblockValidator<Provider>
where
    Provider: StateProviderFactory
        + HeaderProvider<Header = alloy_consensus::Header>
        + ChainSpecProvider<ChainSpec = OpChainSpec>
        + Clone
        + 'static,
{
    /// Legacy sequential validation path (no BAL).
    ///
    /// Decodes transactions as [`OpTxEnvelope`], constructs
    /// [`OpPayloadBuilderAttributes`], and delegates to the standard
    /// [`build()`](flashblocks_builder::payload_builder::build) pipeline.
    fn validate_legacy(
        &self,
        _committed_state: CommittedState<OpRethReceiptBuilder>,
        diff: ExecutionPayloadFlashblockDeltaV1,
        parent: &SealedHeader<Header>,
        payload_id: PayloadId,
        base: &ExecutionPayloadBaseV1,
    ) -> Result<OpBuiltPayload, FlashblockValidationError> {
        let transactions = diff
            .transactions
            .iter()
            .map(|b| {
                let tx: OpTxEnvelope = Decodable2718::decode_2718_exact(b)
                    .map_err(FlashblockValidationError::other)?;
                Ok(WithEncoded::new(b.clone(), tx))
            })
            .collect::<Result<Vec<_>, FlashblockValidationError>>()?;

        let eth_attrs = EthPayloadBuilderAttributes {
            id: payload_id,
            parent: base.parent_hash,
            timestamp: base.timestamp,
            suggested_fee_recipient: base.fee_recipient,
            prev_randao: base.prev_randao,
            withdrawals: Withdrawals(diff.withdrawals.clone()),
            parent_beacon_block_root: Some(base.parent_beacon_block_root),
        };

        let eip1559 = encode_holocene_extra_data(
            Default::default(),
            self.chain_spec.base_fee_params_at_timestamp(base.timestamp),
        )
        .map_err(FlashblockValidationError::other)?;

        let attributes = OpPayloadBuilderAttributes {
            payload_attributes: eth_attrs,
            no_tx_pool: true,
            transactions,
            gas_limit: None,
            eip_1559_params: Some(
                eip1559[1..=8]
                    .try_into()
                    .map_err(FlashblockValidationError::other)?,
            ),
            min_base_fee: None,
        };

        let state_provider = self
            .provider
            .state_by_block_hash(parent.hash())
            .map_err(FlashblockValidationError::Provider)?;

        let config = PayloadConfig::new(Arc::new(parent.clone()), attributes);
        let cancel = CancelOnDrop::default();

        let builder_ctx = OpPayloadBuilderCtxBuilder.build(
            self.provider.clone(),
            self.evm_config.clone(),
            Default::default(),
            config,
            &cancel,
            None,
        );

        let best = |_| BestPayloadTransactions::new(vec![].into_iter());
        let db = StateProviderDatabase::new(state_provider);

        let outcome = build(
            self.provider.clone(),
            best,
            Option::<NoopTransactionPool<EthPooledTransaction>>::None,
            db,
            &builder_ctx,
            None,
            false,
        )
        .map_err(FlashblockValidationError::other)?;

        match outcome.0 {
            reth_basic_payload_builder::BuildOutcomeKind::Better { payload } => Ok(payload),
            reth_basic_payload_builder::BuildOutcomeKind::Freeze(payload) => Ok(payload),
            _ => Err(FlashblockValidationError::Other(Box::from(
                "unexpected build outcome",
            ))),
        }
    }
}

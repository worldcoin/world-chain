use crate::{
    ctx::PayloadBuilderCtx,
    payload::{ExecutionPayloadBaseV1, ExecutionPayloadFlashblockDeltaV1, FlashblocksPayloadV1},
};
use alloy_consensus::{Header, EMPTY_OMMER_ROOT_HASH};
use alloy_eips::merge::BEACON_NONCE;
use alloy_primitives::{map::HashMap, Address, B256, U256};
use reth_chainspec::EthChainSpec;
use reth_evm::{block::BlockExecutor, execute::BlockBuilder, ConfigureEvm, Database};
use reth_execution_types::ExecutionOutcome;
use reth_optimism_consensus::calculate_receipt_root_no_memo_optimism;
use reth_optimism_forks::OpHardforks;
use reth_optimism_payload_builder::{builder::ExecutionInfo, payload::OpBuiltPayload};
use reth_optimism_primitives::{OpPrimitives, OpReceipt, OpTransactionSigned};
use reth_payload_builder_primitives::PayloadBuilderError;
use reth_payload_primitives::PayloadBuilderAttributes;
use reth_primitives::BlockBody;
use reth_primitives_traits::{proofs, Block as _, SignedTransaction};
use reth_provider::{
    HashedPostStateProvider, ProviderError, StateRootProvider, StorageRootProvider,
};
use revm::{
    context::Block as _,
    database::{states::bundle_state::BundleRetention, BundleState, State},
};
use std::sync::Arc;
use tracing::{debug, warn};

pub fn build_block<Ctx, Evm, Builder, ChainSpec, DB, P>(
    mut _state: State<DB>,
    _ctx: &Ctx,
    _builder: Builder,
    _info: &mut ExecutionInfo,
) -> Result<(OpBuiltPayload, FlashblocksPayloadV1, BundleState), PayloadBuilderError>
where
    Ctx: PayloadBuilderCtx,
    Evm: ConfigureEvm,
    Builder: BlockBuilder,
    ChainSpec: EthChainSpec + OpHardforks,
    DB: Database<Error = ProviderError> + AsRef<P>,
    P: StateRootProvider + HashedPostStateProvider + StorageRootProvider,
{
    todo!()
}

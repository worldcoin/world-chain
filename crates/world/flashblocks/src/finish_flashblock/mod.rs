use alloy_eips::eip7685::Requests;
// use alloy_eips::eip6110;
use alloy_evm::eth::eip6110;
use reth::revm::State;
use reth_chainspec::EthereumHardforks;
use reth_evm::block::BlockExecutionError;
use reth_evm::execute::{BlockAssembler, BlockBuilder, BlockBuilderOutcome};
use reth_evm::{ConfigureEvm, Database, Evm};
use reth_primitives::NodePrimitives;
use reth_provider::StateProvider;

use crate::payload_builder_ctx::PayloadBuilderCtx;

pub fn finish_flashblock<Ctx>(ctx: &Ctx, builder: &mut impl BlockBuilder)
where
    Ctx: PayloadBuilderCtx,
{
}

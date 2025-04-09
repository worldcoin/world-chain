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

// TODO: impl Flashblock for BasicBlock builder
pub trait Flashblock<N: NodePrimitives> {
    fn finish_flashblock(
        self,
        state: impl StateProvider,
    ) -> Result<BlockBuilderOutcome<N>, BlockExecutionError>;
}

pub fn finish_flashblock<Ctx>(ctx: &Ctx, builder: &mut impl BlockBuilder)
where
    Ctx: PayloadBuilderCtx,
{
    let evm = ctx.evm();

    let assembler = evm.block_assembler();

    // let mut executor = evm.executor_for_block(db, block)

    let receipts = vec![];

    let executor = builder.executor_mut();

    let requests = if ctx
        .spec()
        .is_prague_active_at_timestamp(evm.block().timestamp)
    {
        // Collect all EIP-6110 deposits
        let deposit_requests = eip6110::parse_deposits_from_receipts(ctx.spec(), receipts)?;

        let mut requests = Requests::default();

        if !deposit_requests.is_empty() {
            requests.push_request_with_type(eip6110::DEPOSIT_REQUEST_TYPE, deposit_requests);
        }

        requests.extend(
            self.system_caller
                .apply_post_execution_changes(&mut self.evm)?,
        );
        requests
    } else {
        Requests::default()
    };

    // executor.effective_gas_price

    // let builder = ctx.block_builder(db);

    // assembler.assemble_block(input)
}

// let (db, evm_env) = evm.finish();
//
// // merge all transitions into bundle state
// db.merge_transitions(BundleRetention::Reverts);
//
// // calculate the state root
// let hashed_state = state.hashed_post_state(&db.bundle_state);
// let (state_root, trie_updates) = state
//     .state_root_with_updates(hashed_state.clone())
//     .map_err(BlockExecutionError::other)?;
//
// let (transactions, senders) =
//     self.transactions.into_iter().map(|tx| tx.into_parts()).unzip();
//
// let block = self.assembler.assemble_block(BlockAssemblerInput {
//     evm_env,
//     execution_ctx: self.ctx,
//     parent: self.parent,
//     transactions,
//     output: &result,
//     bundle_state: &db.bundle_state,
//     state_provider: &state,
//     state_root,
// })?;
//
// let block = RecoveredBlock::new_unhashed(block, senders);
//
// Ok(BlockBuilderOutcome { execution_result: result, hashed_state, trie_updates, block })

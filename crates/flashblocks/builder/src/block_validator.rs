use std::sync::Arc;

use alloy_op_evm::{OpBlockExecutionCtx, block::receipt_builder::OpReceiptBuilder};
use alloy_rpc_types_engine::PayloadId;
use flashblocks_primitives::primitives::ExecutionPayloadFlashblockDeltaV1;
use reth::revm::{State, database::StateProviderDatabase};
use reth_evm::{Evm, EvmEnvFor};
use reth_node_api::{BuiltPayloadExecutedBlock, NodePrimitives};
use reth_optimism_chainspec::OpChainSpec;
use reth_optimism_evm::OpEvmConfig;
use reth_optimism_node::OpBuiltPayload;
use reth_optimism_primitives::OpTransactionSigned;
use reth_primitives::{Recovered, SealedHeader};
use reth_provider::StateProvider;
use revm::{DatabaseRef, database::BundleState};

use crate::{
    access_list::BlockAccessIndex, block_executor::{BalBlockExecutor, BalExecutorError},
    executor::{bal_builder_db::BalBuilderDb, bal_executor::CommittedState, bundle_db::BundleDb, temporal_db::TemporalDbFactory},
};

pub trait FlashblockBlockValidator<N: NodePrimitives> {
    /// Validates a block in parallel using BAL.
    fn validate_flashblock_parallel(
        &self,
        state_provider: Arc<dyn StateProvider>,
        diff: ExecutionPayloadFlashblockDeltaV1,
        parent: &SealedHeader<N::BlockHeader>,
        payload_id: PayloadId,
    ) -> Result<OpBuiltPayload, BalExecutorError>;

    /// Validates a block sequentially without using BAL.
    fn validate_flashblock_sequential(
        &self,
        state_provider: Arc<dyn StateProvider>,
        diff: ExecutionPayloadFlashblockDeltaV1,
        parent: &SealedHeader<N::BlockHeader>,
        payload_id: PayloadId,
    ) -> Result<OpBuiltPayload, BalExecutorError>;
}

pub struct FlashblocksValidatorCtx<R: OpReceiptBuilder> {
    pub chain_spec: Arc<OpChainSpec>,
    pub committed_state: CommittedState<R>,
    pub evm_env: EvmEnvFor<OpEvmConfig>,
    pub evm_config: OpEvmConfig,
    pub execution_context: OpBlockExecutionCtx,
    pub executor_transactions: Vec<(BlockAccessIndex, Recovered<OpTransactionSigned>)>,
}

pub struct TemporalExecutorFactory<'a, DB: DatabaseRef> {
    pub database: TemporalDbFactory<'a, DB>,
    pub bundle_db: BundleDb<DB>,
    pub underlying_db: DB
}

impl<'a, DB: DatabaseRef> TemporalExecutorFactory<'a, DB> {
    pub fn new(
        database: TemporalDbFactory<'a, DB>,
        bundle_db: BundleDb<DB>,
        underlying_db: DB,
    ) -> Self {
        Self { database, bundle_db, underlying_db }
    }

    pub fn create_executor<E: Evm<DB = BalBuilderDb<DB>>, R: OpReceiptBuilder>(&self, index: u64) -> BalBlockExecutor<E, R> {
        let temporal_db = self.database.db(index);
        let bundle_wrapped = BundleDb::new(temporal_db, BundleState::default()); // TODO:

        let state = State::builder().with_database_ref(
            bundle_wrapped
        ).with_bundle_update().build();


        todo!()
    }
}

pub struct FlashblocksBlockValidator<R: OpReceiptBuilder> {
    pub ctx: FlashblocksValidatorCtx<R>,
}

impl<R: OpReceiptBuilder> FlashblocksBlockValidator<R> {
    pub fn new(ctx: FlashblocksValidatorCtx<R>) -> Self {
        Self { ctx }
    }
}

impl<R: OpReceiptBuilder, N: NodePrimitives> FlashblockBlockValidator<N>
    for FlashblocksBlockValidator<R>
{
    fn validate_flashblock_parallel(
        &self,
        state_provider: Arc<dyn StateProvider>,
        diff: ExecutionPayloadFlashblockDeltaV1,
        parent: &SealedHeader<N::BlockHeader>,
        payload_id: PayloadId,
    ) -> Result<OpBuiltPayload, BalExecutorError> {
        unimplemented!()
    }

    fn validate_flashblock_sequential(
        &self,
        state_provider: Arc<dyn StateProvider>,
        diff: ExecutionPayloadFlashblockDeltaV1,
        parent: &SealedHeader<N::BlockHeader>,
        payload_id: PayloadId,
    ) -> Result<OpBuiltPayload, BalExecutorError> {
        // Implementation goes here
        unimplemented!()
    }
}

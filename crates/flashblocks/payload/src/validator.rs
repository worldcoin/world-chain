use alloy_eips::eip2718::Decodable2718;
use alloy_evm::EvmEnv;
use flashblocks_primitives::primitives::{FlashblockBlockAccessList, FlashblocksPayloadV1};
use op_alloy_rpc_types_engine::OpExecutionData;
use reth::{
    api::{
        Block, BlockBody, ConfigureEvm, EngineTypes, InvalidPayloadAttributesError,
        NewPayloadError, PayloadTypes,
    },
    builder::rpc::EngineValidator,
    rpc::types::engine::{ExecutionData, PayloadError},
};
use reth_chain_state::ExecutedBlockWithTrieUpdates;
use reth_engine_tree::tree::payload_validator::{TreeCtx, ValidationOutcome};
use reth_optimism_node::{OpEngineTypes, OpEvmConfig};
use reth_optimism_primitives::{OpPrimitives, OpTransactionSigned};
use reth_primitives::{NodePrimitives, RecoveredBlock};
use reth_provider::{
    BlockReader, DatabaseProviderFactory, HashedPostStateProvider, StateProvider,
    StateProviderFactory, StateReader,
};

#[derive(Debug, Clone)]
pub struct BalEngineValidator<P> {
    provider: P,
    block_access_list: FlashblockBlockAccessList,
    evm_config: OpEvmConfig,
}

impl<P> EngineValidator<OpEngineTypes, OpPrimitives> for BalEngineValidator<P>
where
    P: DatabaseProviderFactory<Provider: BlockReader>
        + BlockReader<Header = <OpPrimitives as NodePrimitives>::BlockHeader>
        + StateProviderFactory
        + StateReader
        + HashedPostStateProvider
        + Clone
        + 'static,
{
    /// Validates the payload attributes with respect to the header.
    ///
    /// By default, this enforces that the payload attributes timestamp is greater than the
    /// timestamp according to:
    ///   > 7. Client software MUST ensure that payloadAttributes.timestamp is greater than
    ///   > timestamp
    ///   > of a block referenced by forkchoiceState.headBlockHash.
    ///
    /// See also: <https://github.com/ethereum/execution-apis/blob/main/src/engine/common.md#specification-1>
    fn validate_payload_attributes_against_header(
        &self,
        attr: &<OpEngineTypes as PayloadTypes>::PayloadAttributes,
        header: &<OpPrimitives as NodePrimitives>::BlockHeader,
    ) -> Result<(), InvalidPayloadAttributesError> {
        todo!()
    }

    /// Ensures that the given payload does not violate any consensus rules that concern the block's
    /// layout.
    ///
    /// This function must convert the payload into the executable block and pre-validate its
    /// fields.
    ///
    /// Implementers should ensure that the checks are done in the order that conforms with the
    /// engine-API specification.
    fn ensure_well_formed_payload(
        &self,
        payload: <OpEngineTypes as PayloadTypes>::ExecutionData,
    ) -> Result<RecoveredBlock<<OpPrimitives as NodePrimitives>::Block>, NewPayloadError> {
        todo!()
    }

    /// Validates a payload received from engine API.
    fn validate_payload(
        &mut self,
        payload: <OpEngineTypes as PayloadTypes>::ExecutionData,
        ctx: TreeCtx<'_, OpPrimitives>,
    ) -> ValidationOutcome<OpPrimitives> {

        // // spin up an exe
    }

    /// Validates a block downloaded from the network.
    fn validate_block(
        &mut self,
        block: RecoveredBlock<<OpPrimitives as NodePrimitives>::Block>,
        ctx: TreeCtx<'_, OpPrimitives>,
    ) -> ValidationOutcome<OpPrimitives> {
        todo!()
    }
}

pub struct ParallelBlockExecutor {
    env: EvmEnv,
    /// TODO: This needs to be an aggregate of all access lists
    access_list: FlashblockBlockAccessList,
    provider: Box<dyn StateProvider>,
}

impl ParallelBlockExecutor {
    pub fn new(
        env: EvmEnv,
        access_list: FlashblockBlockAccessList,
        provider: impl StateProvider + 'static,
    ) -> Self {
        Self {
            env,
            access_list,
            provider: Box::new(provider),
        }
    }

    pub fn execute_block(
        &self,
        payload: OpExecutionData,
    ) -> eyre::Result<ExecutedBlockWithTrieUpdates<OpPrimitives>> {
        let block = payload
            .payload
            .try_into_block_with(|mut tx| {
                OpTransactionSigned::decode_2718(&mut tx.as_ref())
                    .map_err(|e| PayloadError::Decode(e.into()))
            })
            .expect("valid payload");

        for (i, tx) in block.body().transactions_iter().enumerate() {
            let changes_for_tx = self.access_list.accounts();
        }
        // For each transaction in the payload, spawn a new task to execute it in its own EVM instance
        // with the shared env and access list.
        // Collect results and merge them into a single ExecutedBlockWithTrieUpdates.
        todo!()
    }
}

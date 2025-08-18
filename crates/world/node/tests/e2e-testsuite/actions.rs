use alloy_rpc_types::{Header, Transaction, TransactionRequest};
use alloy_rpc_types_engine::{ForkchoiceState, PayloadStatusEnum};
use eyre::eyre::{eyre, Result};
use futures::future::BoxFuture;
use op_alloy_rpc_types_engine::OpExecutionPayloadEnvelopeV3;
use reth::rpc::api::{EngineApiClient, EthApiClient};
use reth_e2e_test_utils::testsuite::{actions::Action, Environment};
use reth_optimism_node::{OpEngineTypes, OpPayloadAttributes};
use revm_primitives::{Bytes, B256};
use rollup_boost::Authorization;
use tracing::debug;
use world_chain_builder_flashblocks::rpc::engine::FlashblocksEngineApiExtClient;

/// Mine a single block with the given transactions and verify the block was created
/// successfully.
#[derive(Debug)]
pub struct AssertMineBlock<T>
where
    T: Fn(OpPayloadAttributes) -> Authorization + Clone + Send + Sync,
{
    /// The node index to mine
    pub node_idx: usize,
    #[allow(dead_code)]
    /// Transactions to include in the block
    pub transactions: Vec<Bytes>,
    #[allow(dead_code)]
    /// Expected block hash (optional)
    pub expected_hash: Option<B256>,
    /// Block's payload attributes
    // TODO: refactor once we have actions to generate payload attributes.
    pub payload_attributes: OpPayloadAttributes,
    /// Authorization Generator
    pub authorization_generator: T,
    /// Sender to return the mined payload
    pub tx: tokio::sync::mpsc::Sender<OpExecutionPayloadEnvelopeV3>,
}

impl<T> AssertMineBlock<T>
where
    T: Fn(OpPayloadAttributes) -> Authorization + Clone + Send + Sync,
{
    /// Create a new `AssertMineBlock` action
    pub async fn new(
        node_idx: usize,
        transactions: Vec<Bytes>,
        expected_hash: Option<B256>,
        payload_attributes: OpPayloadAttributes,
        authorization_generator: T,
        tx: tokio::sync::mpsc::Sender<OpExecutionPayloadEnvelopeV3>,
    ) -> Self {
        Self {
            node_idx,
            transactions,
            expected_hash,
            payload_attributes,
            authorization_generator,
            tx,
        }
    }
}

impl<T> Action<OpEngineTypes> for AssertMineBlock<T>
where
    T: Fn(OpPayloadAttributes) -> Authorization + Clone + Send + Sync + 'static,
{
    fn execute<'a>(
        &'a mut self,
        env: &'a mut Environment<OpEngineTypes>,
    ) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            if self.node_idx >= env.node_clients.len() {
                return Err(eyre!("Node index out of bounds: {}", self.node_idx));
            }

            let node_client = &env.node_clients[self.node_idx];
            let rpc_client = &node_client.rpc;
            let engine_client = node_client.engine.http_client();

            // get the latest block to use as parent
            let latest_block = EthApiClient::<
                TransactionRequest,
                Transaction,
                alloy_rpc_types_eth::Block,
                alloy_consensus::Receipt,
                Header,
            >::block_by_number(
                rpc_client, alloy_eips::BlockNumberOrTag::Latest, false
            )
            .await?;

            let latest_block = latest_block.ok_or_else(|| eyre!("Latest block not found"))?;
            let parent_hash = latest_block.header.hash_slow();

            debug!("Latest block hash: {parent_hash}");

            // create a simple forkchoice state with the latest block as head
            let fork_choice_state = ForkchoiceState {
                head_block_hash: parent_hash,
                safe_block_hash: parent_hash,
                finalized_block_hash: parent_hash,
            };

            let fcu_result =
                FlashblocksEngineApiExtClient::<OpEngineTypes>::flashblocks_fork_choice_updated_v3(
                    &engine_client,
                    fork_choice_state,
                    Some(self.payload_attributes.clone()),
                    Some((self.authorization_generator)(
                        self.payload_attributes.clone(),
                    )),
                )
                .await?;

            debug!("FCU result: {:?}", fcu_result);

            // wait the deadline interval
            std::thread::sleep(std::time::Duration::from_millis(2000));

            // check if we got a valid payload ID
            match fcu_result.payload_status.status {
                PayloadStatusEnum::Valid => {
                    if let Some(payload_id) = fcu_result.payload_id {
                        debug!("Got payload ID: {payload_id}");

                        // get the payload that was built
                        let engine_payload = EngineApiClient::<OpEngineTypes>::get_payload_v3(
                            &engine_client,
                            payload_id,
                        )
                        .await?;

                        let _ = self
                            .tx
                            .send(engine_payload)
                            .await
                            .map_err(|e| eyre!("Failed to send payload via channel: {}", e))?;

                        debug!("Mined block with payload ID: {}", payload_id);
                        Ok(())
                    } else {
                        Err(eyre!("No payload ID returned from forkchoiceUpdated"))
                    }
                }
                _ => Err(eyre!(
                    "Payload status not valid: {:?}",
                    fcu_result.payload_status
                )),
            }
        })
    }
}

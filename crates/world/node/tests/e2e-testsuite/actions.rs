#![allow(dead_code)]
use alloy_eips::{BlockId, BlockNumberOrTag, eip1559};
use alloy_rpc_types::{Block, Header, Transaction, TransactionRequest};
use alloy_rpc_types_engine::{
    ExecutionPayloadEnvelopeV3, ForkchoiceState, PayloadAttributes, PayloadStatusEnum,
};
use eyre::eyre::{Result, eyre};
use flashblocks_primitives::p2p::Authorization;
use flashblocks_rpc::{
    engine::{FlashblocksEngineApiExtClient, OpEngineApiExt},
    op::OpApiExtClient,
};
use futures::future::BoxFuture;
use op_alloy_consensus::encode_holocene_extra_data;
use op_alloy_rpc_types::OpTransactionReceipt;
use op_alloy_rpc_types_engine::OpExecutionPayloadEnvelopeV3;
use reth::rpc::{
    api::{EngineApiClient, EthApiClient},
    eth::EthApi,
};
use reth_e2e_test_utils::testsuite::{
    BlockInfo, Environment,
    actions::{
        Action, GenerateNextPayload, Sequence, UpdateBlockInfo, ValidateFork,
        expect_fcu_not_syncing_or_accepted, fork, validate_fcu_response,
    },
};
use reth_node_api::{EngineTypes, PayloadTypes};
use reth_optimism_node::{OpEngineTypes, OpPayloadAttributes};
use reth_optimism_primitives::OpReceipt;
use reth_primitives::TransactionSigned;
use revm_primitives::{Address, B256, Bytes, FixedBytes, U256};
use std::{
    fmt::Debug,
    marker::PhantomData,
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tokio::time::sleep;
use tracing::{debug, error, info};

use crate::setup::{CHAIN_SPEC, TX_SET_L1_BLOCK};

/// Mine a single block with the given transactions and verify the block was created
/// successfully.
#[derive(Debug, Clone)]
pub struct AssertMineBlock<T> {
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
    /// The block interval
    pub block_interval: Duration,
    /// Whether to send `flashblocks_forkchoiceUpdatedV3`
    pub flashblocks: bool,
    /// Whether to fetch the payload after mining
    pub fetch_payload: bool,
    /// Sender to return the mined payload
    pub tx: tokio::sync::mpsc::Sender<OpExecutionPayloadEnvelopeV3>,
}

impl<T> AssertMineBlock<T>
where
    T: Fn(OpPayloadAttributes) -> Authorization + Clone + Send + Sync,
{
    /// Create a new `AssertMineBlock` action
    #[expect(clippy::too_many_arguments)]
    pub async fn new(
        node_idx: usize,
        transactions: Vec<Bytes>,
        expected_hash: Option<B256>,
        payload_attributes: OpPayloadAttributes,
        authorization_generator: T,
        block_interval: Duration,
        flashblocks: bool,
        fetch_payload: bool,
        tx: tokio::sync::mpsc::Sender<OpExecutionPayloadEnvelopeV3>,
    ) -> Self {
        Self {
            node_idx,
            transactions,
            expected_hash,
            payload_attributes,
            authorization_generator,
            block_interval,
            flashblocks,
            fetch_payload,
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
                TransactionSigned,
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

            let fcu_result = if self.flashblocks {
                FlashblocksEngineApiExtClient::<OpEngineTypes>::flashblocks_fork_choice_updated_v3(
                    &engine_client,
                    fork_choice_state,
                    Some(self.payload_attributes.clone()),
                    Some((self.authorization_generator)(
                        self.payload_attributes.clone(),
                    )),
                )
                .await?
            } else {
                EngineApiClient::<OpEngineTypes>::fork_choice_updated_v3(
                    &engine_client,
                    fork_choice_state,
                    Some(self.payload_attributes.clone()),
                )
                .await?
            };

            debug!("FCU result: {:?}", fcu_result);

            if self.fetch_payload {
                // wait the deadline interval
                std::thread::sleep(self.block_interval);

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

                            self.tx
                                .send(engine_payload)
                                .await
                                .map_err(|e| eyre!("Failed to send payload via channel: {}", e))?;

                            debug!("Mined block with payload ID: {}", payload_id);
                            return Ok(());
                        } else {
                            return Err(eyre!("No payload ID returned from forkchoiceUpdated"));
                        }
                    }
                    _ => {
                        return Err(eyre!(
                            "Payload status not valid: {:?}",
                            fcu_result.payload_status
                        ));
                    }
                }
            };

            Ok(())
        })
    }
}

/// Pick the next block producer based on the latest block information for flashblocks workflow.
#[derive(Debug, Default)]
pub struct PickNextFlashblocksProducer {}

impl PickNextFlashblocksProducer {
    /// Create a new `PickNextFlashblocksProducer` action
    pub const fn new() -> Self {
        Self {}
    }
}

impl Action<OpEngineTypes> for PickNextFlashblocksProducer {
    fn execute<'a>(
        &'a mut self,
        env: &'a mut reth_e2e_test_utils::testsuite::Environment<OpEngineTypes>,
    ) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            let num_clients = env.node_clients.len();
            if num_clients == 0 {
                return Err(eyre!("No node clients available"));
            }

            // simple round-robin selection based on next block number
            let next_producer_idx = env.last_producer_idx;

            env.last_producer_idx = next_producer_idx;

            Ok(())
        })
    }
}

#[derive(Clone)]
/// Generate payload attributes for flashblocks workflow
pub struct GenerateFlashblocksPayloadAttributes<F>
where
    F: Fn(OpPayloadAttributes) -> Authorization + Send + Sync + Clone + 'static,
{
    /// Authorization generator function
    pub authorization_generator: F,
}

impl<F> GenerateFlashblocksPayloadAttributes<F>
where
    F: Fn(OpPayloadAttributes) -> Authorization + Send + Sync + Clone + 'static,
{
    /// Create a new action with authorization generator
    pub fn new(authorization_generator: F) -> Self {
        Self {
            authorization_generator,
        }
    }
}

impl<F> Action<OpEngineTypes> for GenerateFlashblocksPayloadAttributes<F>
where
    F: Fn(OpPayloadAttributes) -> Authorization + Send + Sync + Clone + 'static,
{
    fn execute<'a>(
        &'a mut self,
        env: &'a mut reth_e2e_test_utils::testsuite::Environment<OpEngineTypes>,
    ) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            let latest_block = env
                .current_block_info()
                .ok_or_else(|| eyre!("No latest block information available"))?;
            let block_number = latest_block.number;

            let timestamp = SystemTime::UNIX_EPOCH
                .duration_since(UNIX_EPOCH)
                .expect("Time went backwards")
                .as_secs()
                + env.block_timestamp_increment;

            let eip1559 = encode_holocene_extra_data(
                Default::default(),
                CHAIN_SPEC.base_fee_params_at_timestamp(timestamp),
            )?;
            let payload_attributes = OpPayloadAttributes {
                payload_attributes: alloy_rpc_types_engine::PayloadAttributes {
                    timestamp,
                    prev_randao: B256::random(),
                    suggested_fee_recipient: revm_primitives::Address::random(),
                    withdrawals: Some(vec![]),
                    parent_beacon_block_root: Some(B256::ZERO),
                },
                transactions: None,
                no_tx_pool: Some(false),
                eip_1559_params: Some(eip1559[1..=8].try_into()?),
                gas_limit: Some(30_000_000),
                min_base_fee: None,
            };

            env.active_node_state_mut()?.payload_attributes.insert(
                latest_block.number + 1,
                payload_attributes.payload_attributes.clone(),
            );

            debug!(
                "Stored flashblocks payload attributes for block {}",
                block_number + 1
            );
            Ok(())
        })
    }
}

/// Action that retrieves the built flashblocks payload
#[derive(Debug, Default)]
pub struct RetrieveFlashblocksPayload {}

impl RetrieveFlashblocksPayload {
    /// Create a new `RetrieveFlashblocksPayload` action
    pub const fn new() -> Self {
        Self {}
    }
}

impl Action<OpEngineTypes> for RetrieveFlashblocksPayload {
    fn execute<'a>(
        &'a mut self,
        env: &'a mut reth_e2e_test_utils::testsuite::Environment<OpEngineTypes>,
    ) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            let latest_block = env
                .current_block_info()
                .ok_or_else(|| eyre!("No latest block information available"))?;

            let payload_id = env
                .active_node_state()?
                .next_payload_id
                .ok_or_else(|| eyre!("No payload ID available"))?;

            // Wait for payload to be built
            sleep(Duration::from_millis(2000)).await;

            let built_payload_envelope =
                reth::rpc::api::EngineApiClient::<OpEngineTypes>::get_payload_v3(
                    &env.node_clients[0].engine.http_client(),
                    payload_id,
                )
                .await?;

            // Store the payload envelope
            env.active_node_state_mut()?.latest_payload_envelope = Some(built_payload_envelope);

            debug!(
                "Retrieved flashblocks payload for block {}",
                latest_block.number + 1
            );
            Ok(())
        })
    }
}

/// Action that updates environment state using the flashblocks payload
#[derive(Debug, Default)]
pub struct UpdateBlockInfoToFlashblocksPayload {}

impl UpdateBlockInfoToFlashblocksPayload {
    /// Create a new action
    pub const fn new() -> Self {
        Self {}
    }
}

impl Action<OpEngineTypes> for UpdateBlockInfoToFlashblocksPayload {
    fn execute<'a>(
        &'a mut self,
        env: &'a mut reth_e2e_test_utils::testsuite::Environment<OpEngineTypes>,
    ) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            let payload_envelope = env
                .active_node_state()?
                .latest_payload_envelope
                .as_ref()
                .ok_or_else(|| eyre!("No execution payload envelope available"))?;

            let execution_payload = &payload_envelope.execution_payload;
            let block_hash = execution_payload.payload_inner.payload_inner.block_hash;
            let block_number = execution_payload.payload_inner.payload_inner.block_number;
            let block_timestamp = execution_payload.payload_inner.payload_inner.timestamp;

            // Update environment with the new block information from the payload
            env.set_current_block_info(reth_e2e_test_utils::testsuite::BlockInfo {
                hash: block_hash,
                number: block_number,
                timestamp: block_timestamp,
            })?;

            env.active_node_state_mut()?.latest_header_time = block_timestamp;
            env.active_node_state_mut()?
                .latest_fork_choice_state
                .head_block_hash = block_hash;
            info!(
                "Updated environment to newly produced flashblocks block {} (hash: {})",
                block_number, block_hash
            );

            Ok(())
        })
    }
}

/// Action that produces a sequence of blocks using flashblocks
#[derive(Debug)]
pub struct ProduceBlocksWithFlashblocks<F>
where
    F: Fn(OpPayloadAttributes, FixedBytes<32>) -> Authorization + Clone + Send + Sync + 'static,
{
    /// Number of blocks to produce
    pub num_blocks: u64,
    /// Authorization generator function
    pub authorization_generator: F,
}

impl<F> ProduceBlocksWithFlashblocks<F>
where
    F: Fn(OpPayloadAttributes, FixedBytes<32>) -> Authorization + Clone + Send + Sync + 'static,
{
    /// Create a new `ProduceBlocksWithFlashblocks` action
    pub fn new(num_blocks: u64, authorization_generator: F) -> Self {
        Self {
            num_blocks,
            authorization_generator,
        }
    }
}

/// Store payload attributes for the next block.
#[derive(Debug)]
pub struct GeneratePayloadAttributes(u64);

impl Default for GeneratePayloadAttributes {
    fn default() -> Self {
        let now = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .expect("Time went backwards")
            .as_secs();

        Self(now)
    }
}
impl Action<OpEngineTypes> for GeneratePayloadAttributes {
    fn execute<'a>(
        &'a mut self,
        env: &'a mut Environment<OpEngineTypes>,
    ) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            let latest_block = env
                .current_block_info()
                .ok_or_else(|| eyre!("No latest block information available"))?;

            let timestamp = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("Time went backwards")
                .as_secs()
                + env.block_timestamp_increment;
                
            let latest_block = latest_block.number;

            info!(
                "Generating payload attributes for next block... {} -> {}",
                latest_block, timestamp
            );

            let payload_attributes = PayloadAttributes {
                timestamp,
                prev_randao: B256::random(),
                suggested_fee_recipient: alloy_primitives::Address::random(),
                withdrawals: Some(vec![]),
                parent_beacon_block_root: Some(B256::ZERO),
            };

            env.active_node_state_mut()?
                .payload_attributes
                .insert(latest_block + 1, payload_attributes);

            Ok(())
        })
    }
}

impl<F> Action<OpEngineTypes> for ProduceBlocksWithFlashblocks<F>
where
    F: Fn(OpPayloadAttributes, FixedBytes<32>) -> Authorization + Clone + Send + Sync + 'static,
{
    fn execute<'a>(
        &'a mut self,
        env: &'a mut Environment<OpEngineTypes>,
    ) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            // Remember the active node to ensure all blocks are produced on the same node
            let producer_idx = 0;

            for _ in 0..self.num_blocks {
                // Ensure we always use the same producer
                env.last_producer_idx = Some(producer_idx);

                // create a sequence that produces blocks and sends only to active node
                let mut sequence = Sequence::new(vec![
                    // Skip PickNextBlockProducer to maintain the same producer
                    Box::new(GeneratePayloadAttributes::default()),
                    Box::new(GenerateNextFlashblocksPayload {
                        authorization_generator: self.authorization_generator.clone(),
                    }),
                    Box::new(MakeCanonical::default()),
                ]);

                sequence.execute(env).await?;
            }
            Ok(())
        })
    }
}

/// Action that makes the current latest block canonical by broadcasting a forkchoice update
#[derive(Debug, Default)]
pub struct MakeCanonical;

impl Action<OpEngineTypes> for MakeCanonical {
    fn execute<'a>(
        &'a mut self,
        env: &'a mut Environment<OpEngineTypes>,
    ) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            // Original broadcast behavior
            let mut actions: Vec<Box<dyn Action<OpEngineTypes>>> = vec![
                Box::new(BroadcastLatestForkchoice::default()),
                Box::new(UpdateBlockInfo::default()),
            ];

            // if we're on a fork, validate it now that it's canonical
            if let Ok(active_state) = env.active_node_state()
                && let Some(fork_base) = active_state.current_fork_base
            {
                debug!(
                    "MakeCanonical: Adding fork validation from base block {}",
                    fork_base
                );
                actions.push(Box::new(ValidateFork::new(fork_base)));
                // clear the fork base since we're now canonical
                env.active_node_state_mut()?.current_fork_base = None;
            }

            let mut sequence = Sequence::new(actions);
            sequence.execute(env).await
        })
    }
}

/// Action that broadcasts the latest fork choice state to all clients
#[derive(Debug, Default)]
pub struct BroadcastLatestForkchoice {}

impl Action<OpEngineTypes> for BroadcastLatestForkchoice {
    fn execute<'a>(
        &'a mut self,
        env: &'a mut Environment<OpEngineTypes>,
    ) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            if env.node_clients.is_empty() {
                return Err(eyre!("No node clients available"));
            }

            // use the hash of the newly executed payload if available
            let head_hash = env
                .current_block_info()
                .ok_or_else(|| eyre!("No current block info available"))?
                .hash;

            let fork_choice_state = ForkchoiceState {
                head_block_hash: head_hash,
                safe_block_hash: head_hash,
                finalized_block_hash: head_hash,
            };
            debug!(
                "Broadcasting forkchoice update to {} clients. Head: {:?}",
                env.node_clients.len(),
                fork_choice_state.head_block_hash
            );

            for (idx, client) in env.node_clients.iter().enumerate() {
                match EngineApiClient::<OpEngineTypes>::fork_choice_updated_v3(
                    &client.engine.http_client(),
                    fork_choice_state,
                    None,
                )
                .await
                {
                    Ok(resp) => {
                        debug!(
                            "Client {}: Forkchoice update status: {:?}",
                            idx, resp.payload_status.status
                        );
                        // validate that the forkchoice update was accepted
                        validate_fcu_response(&resp, &format!("Client {idx}"))?;
                    }
                    Err(err) => {
                        return Err(eyre!(
                            "Client {}: Failed to broadcast forkchoice: {:?}",
                            idx,
                            err
                        ));
                    }
                }
            }
            debug!("Forkchoice update broadcasted successfully");
            Ok(())
        })
    }
}

/// Action that updates environment state using the locally produced payload.
///
/// This uses the execution payload stored in the environment rather than querying RPC,
/// making it more efficient and reliable during block production. Preferred over
/// `UpdateBlockInfo` when we have just produced a block and have the payload available.
#[derive(Debug, Default)]
pub struct UpdateBlockInfoToLatestPayload {}

impl Action<OpEngineTypes> for UpdateBlockInfoToLatestPayload {
    fn execute<'a>(
        &'a mut self,
        env: &'a mut Environment<OpEngineTypes>,
    ) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            info!("Updating environment to latest produced payload...");
            let payload_envelope = env
                .active_node_state()?
                .latest_payload_envelope
                .as_ref()
                .ok_or_else(|| eyre!("No execution payload envelope available"))?;

            let execution_payload = &payload_envelope.execution_payload;

            let block_hash = execution_payload.payload_inner.payload_inner.block_hash;
            let block_number = execution_payload.payload_inner.payload_inner.block_number;
            let block_timestamp = execution_payload.payload_inner.payload_inner.timestamp;

            // update environment with the new block information from the payload
            env.set_current_block_info(BlockInfo {
                hash: block_hash,
                number: block_number,
                timestamp: block_timestamp,
            })?;

            env.active_node_state_mut()?.latest_header_time = block_timestamp;
            env.active_node_state_mut()?
                .latest_fork_choice_state
                .head_block_hash = block_hash;

            info!(
                "Updated environment to newly produced block {} (hash: {})",
                block_number, block_hash
            );

            Ok(())
        })
    }
}

// /// Action that broadcasts the next new payload
// #[derive(Debug, Default)]
// pub struct BroadcastNextNewPayload;

// impl Action<OpEngineTypes> for BroadcastNextNewPayload {
//     fn execute<'a>(
//         &'a mut self,
//         env: &'a mut Environment<OpEngineTypes>,
//     ) -> BoxFuture<'a, Result<()>> {
//         Box::pin(async move {
//             info!("Broadcasting next new payload...");
//             // Get the next new payload to broadcast
//             let next_new_payload = env
//                 .active_node_state()?
//                 .latest_payload_built
//                 .as_ref()
//                 .ok_or_else(|| eyre!("No next built payload found"))
//                 .inspect_err(|e| error!("{e}"))?;

//             let parent_beacon_block_root = next_new_payload
//                 .parent_beacon_block_root
//                 .ok_or_else(|| eyre!("No parent beacon block root for next new payload"))
//                 .inspect_err(|e| error!("{e}"))?;

//             let payload_envelope = env
//                 .active_node_state()?
//                 .latest_payload_envelope
//                 .as_ref()
//                 .ok_or_else(|| eyre!("No execution payload envelope available"))
//                 .inspect_err(|e| error!("{e}"))?;

//             let execution_payload_envelope: OpExecutionPayloadEnvelopeV3 = payload_envelope.clone();
//             let execution_payload = execution_payload_envelope.execution_payload;

//             // Loop through all clients and broadcast the next new payload
//             let mut broadcast_results = Vec::new();

//             for (idx, client) in env.node_clients.iter().enumerate() {
//                 let engine = client.engine.http_client();

//                 // Broadcast the execution payload
//                 let result = EngineApiClient::<OpEngineTypes>::new_payload_v3(
//                     &engine,
//                     execution_payload.clone(),
//                     vec![],
//                     parent_beacon_block_root,
//                 )
//                 .await?;

//                 broadcast_results.push((idx, result.status.clone()));

//                 info!(
//                     "Node {}: new_payload broadcast status: {:?}",
//                     idx, result.status
//                 );
//             }

//             // Update the executed payload state after broadcasting to all nodes
//             env.active_node_state_mut()?.latest_payload_executed = Some(next_new_payload.clone());

//             info!("Broadcast complete. Results: {:?}", broadcast_results);

//             Ok(())
//         })
//     }
// }

/// Action that generates the next payload
#[derive(Debug, Default)]
pub struct GenerateNextFlashblocksPayload<F>
where
    F: Fn(OpPayloadAttributes, FixedBytes<32>) -> Authorization + Clone + Send + Sync + 'static,
{
    /// Authorization generator function
    pub authorization_generator: F,
}

impl<F> Action<OpEngineTypes> for GenerateNextFlashblocksPayload<F>
where
    F: Fn(OpPayloadAttributes, FixedBytes<32>) -> Authorization + Clone + Send + Sync + 'static,
{
    fn execute<'a>(
        &'a mut self,
        env: &'a mut Environment<OpEngineTypes>,
    ) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            let latest_block = env
                .current_block_info()
                .ok_or_else(|| eyre!("No latest block information available"))?;

            let parent_hash = latest_block.hash;
            info!("Latest block hash: {parent_hash}");

            let fork_choice_state = ForkchoiceState {
                head_block_hash: parent_hash,
                safe_block_hash: parent_hash,
                finalized_block_hash: parent_hash,
            };

            let payload_attributes = env
                .active_node_state()?
                .payload_attributes
                .get(&(latest_block.number + 1))
                .cloned()
                .ok_or_else(|| eyre!("No payload attributes found for next block"))?;

            let eip1559 = encode_holocene_extra_data(
                Default::default(),
                CHAIN_SPEC.base_fee_params_at_timestamp(payload_attributes.timestamp),
            )?;

            info!(
                "Generating next flashblocks payload for block {} with attributes: {:?}",
                latest_block.number + 1,
                payload_attributes
            );
            // Compose a Mine Block action with an eth_getTransactionReceipt action
            let attributes = OpPayloadAttributes {
                payload_attributes,
                transactions: Some(vec![]),
                no_tx_pool: Some(false),
                eip_1559_params: Some(eip1559[1..=8].try_into()?),
                gas_limit: Some(30_000_000),
                min_base_fee: None,
            };

            let producer_idx = env
                .last_producer_idx
                .ok_or_else(|| eyre!("No producer index set in environment"))?;

            info!("Using producer index: {producer_idx}");
            let fcu_result =
                FlashblocksEngineApiExtClient::<OpEngineTypes>::flashblocks_fork_choice_updated_v3(
                    &env.node_clients[producer_idx].engine.http_client(),
                    fork_choice_state,
                    Some(attributes.clone()),
                    Some((self.authorization_generator)(
                        attributes.clone(),
                        parent_hash,
                    )),
                )
                .await?;

            info!("FCU result: {:?}", fcu_result);

            // validate the FCU status before proceeding
            // Note: In the context of GenerateNextPayload, Syncing usually means the engine
            // doesn't have the requested head block, which should be an error
            expect_fcu_not_syncing_or_accepted(&fcu_result, "GenerateNextPayload")
                .inspect_err(|e| error!("{e}"))?;

            let payload_id = fcu_result
                .payload_id
                .ok_or_else(|| eyre!("No payload ID returned from forkchoiceUpdated"))?;

            env.active_node_state_mut()?.next_payload_id = Some(payload_id);

            info!("Got payload ID: {payload_id}, Producer Index: {producer_idx}");

            let built_payload_envelope = EngineApiClient::<OpEngineTypes>::get_payload_v3(
                &env.node_clients[producer_idx].engine.http_client(),
                payload_id,
            )
            .await?;

            info!(
                "Built payload envelope for block {}: {:?}",
                latest_block.number + 1,
                built_payload_envelope
            );
            // Store the payload attributes that were used to generate this payload
            let built_payload = attributes.clone();

            info!(
                "Built payload for block {}: {:?}",
                latest_block.number + 1,
                built_payload_envelope
            );

            env.active_node_state_mut()?
                .payload_id_history
                .insert(latest_block.number + 1, payload_id);
            env.active_node_state_mut()?.latest_payload_built =
                Some(built_payload.payload_attributes);
            env.active_node_state_mut()?.latest_payload_envelope = Some(built_payload_envelope);

            info!(
                "Generated next flashblocks payload for block {}",
                latest_block.number + 1
            );
            Ok(())
        })
    }
}

// ------------------------------------------------------------------ EthApi Test Actions ------------------------------------------------------------------

/// A composite action that first mines a block, then executes an Ethereum API action.
///
/// This action is designed for end-to-end testing scenarios where you need to:
/// 1. Mine a block with specific transactions/payload attributes
/// 2. Execute an Ethereum RPC API call against the mined block
/// # Example
///
/// ```rust, ignore
/// let eth_call = EthCall::new(tx, vec![0, 1, 2], 200, result_tx);
///
/// // Combine into composite action
/// let mut action = EthApiAction::new(mine_block, eth_call);
///
/// // Execute: mines block first, then performs eth_call
/// action.execute(&mut env).await?;
/// ```
#[derive(Debug)]
pub struct EthApiAction<T, A> {
    pub mine_block: AssertMineBlock<A>,
    pub action: T,
}

impl<T, A> EthApiAction<T, A>
where
    T: Action<OpEngineTypes> + Send + 'static,
    A: Fn(OpPayloadAttributes) -> Authorization + Clone + Send + Sync + 'static,
{
    pub fn new(mine_block: AssertMineBlock<A>, action: T) -> Self {
        Self { mine_block, action }
    }
}

impl<T, A> Action<OpEngineTypes> for EthApiAction<T, A>
where
    T: Action<OpEngineTypes> + Send + 'static,
    A: Fn(OpPayloadAttributes) -> Authorization + Clone + Send + Sync + 'static,
{
    fn execute<'a>(
        &'a mut self,
        env: &'a mut Environment<OpEngineTypes>,
    ) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            self.mine_block.execute(env).await?;
            self.action.execute(env).await?;
            Ok(())
        })
    }
}

#[derive(Debug)]
pub struct EthGetTransactionReceipt {
    /// The transaction hash the receipt will be fetched for.
    pub hash: Vec<B256>,
    /// The node index's to query the receipt for.
    pub node_idxs: Vec<usize>,
    /// Duration in milliseconds of backoff before fetching the receipt
    pub backoff: u64,
    /// Tx sender for receipt results
    pub tx: tokio::sync::mpsc::Sender<Vec<Vec<Option<OpTransactionReceipt>>>>,
}

impl EthGetTransactionReceipt {
    /// Creates a new `EthGetTransactionReceipt` action.
    pub fn new(
        hash: Vec<B256>,
        node_idxs: Vec<usize>,
        backoff: u64,
        tx: tokio::sync::mpsc::Sender<Vec<Vec<Option<OpTransactionReceipt>>>>,
    ) -> Self {
        Self {
            hash,
            node_idxs,
            backoff,
            tx,
        }
    }
}

impl Action<OpEngineTypes> for EthGetTransactionReceipt {
    fn execute<'a>(
        &'a mut self,
        env: &'a mut Environment<OpEngineTypes>,
    ) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            tokio::time::sleep(Duration::from_millis(self.backoff)).await;

            let mut receipts = vec![];

            for node_idx in &self.node_idxs {
                let rpc_client = env.node_clients[*node_idx].rpc.clone();
                let mut receipts_inner = vec![];
                for hash in &self.hash {
                    let receipt: Option<OpTransactionReceipt> =
                        EthApiClient::<
                            TransactionRequest,
                            Transaction,
                            alloy_rpc_types_eth::Block,
                            OpTransactionReceipt,
                            Header,
                            TransactionSigned,
                        >::transaction_receipt(&rpc_client, *hash)
                        .await?;

                    receipts_inner.push(receipt);
                }

                receipts.push(receipts_inner);
            }

            self.tx
                .send(receipts)
                .await
                .map_err(|e| eyre!("Failed to send receipt results via channel: {}", e))?;

            Ok(())
        })
    }
}

#[derive(Debug)]
pub struct EthCall {
    /// The transaction to be simulated.
    pub transaction: TransactionRequest,
    /// The node index's to simulate the transaction through.
    pub node_idxs: Vec<usize>,
    /// Duration in milliseconds of backoff before performing the call
    pub backoff: u64,
    /// Sender for call results
    pub tx: tokio::sync::mpsc::Sender<Vec<Bytes>>,
}

impl EthCall {
    pub fn new(
        transaction: TransactionRequest,
        node_idxs: Vec<usize>,
        backoff: u64,
        tx: tokio::sync::mpsc::Sender<Vec<Bytes>>,
    ) -> Self {
        Self {
            transaction,
            node_idxs,
            backoff,
            tx,
        }
    }
}

impl Action<OpEngineTypes> for EthCall {
    fn execute<'a>(
        &'a mut self,
        env: &'a mut Environment<OpEngineTypes>,
    ) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            tokio::time::sleep(Duration::from_millis(self.backoff)).await;
            let mut futs = vec![];
            for idx in &self.node_idxs {
                let fut = async {
                    let res: Bytes = EthApiClient::<
                        TransactionRequest,
                        Transaction,
                        alloy_rpc_types_eth::Block,
                        alloy_consensus::Receipt,
                        Header,
                        TransactionSigned,
                    >::call(
                        &env.node_clients[*idx].rpc,
                        self.transaction.clone(),
                        Some(BlockId::pending()),
                        None,
                        None,
                    )
                    .await?;

                    Ok::<_, eyre::Report>(res)
                };

                futs.push(fut);
            }

            let results = futures::future::try_join_all(futs).await?;

            self.tx
                .send(results)
                .await
                .map_err(|e| eyre!("Failed to send call results via channel: {}", e))?;

            Ok(())
        })
    }
}

#[derive(Debug)]
pub struct SupportedCapabilitiesCall {
    pub tx: tokio::sync::mpsc::Sender<Vec<String>>,
}

impl SupportedCapabilitiesCall {
    pub fn new(tx: tokio::sync::mpsc::Sender<Vec<String>>) -> Self {
        Self { tx }
    }
}

impl Action<OpEngineTypes> for SupportedCapabilitiesCall {
    fn execute<'a>(
        &'a mut self,
        env: &'a mut Environment<OpEngineTypes>,
    ) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            let result = OpApiExtClient::supported_capabilities(&env.node_clients[0].rpc).await?;
            self.tx
                .send(result)
                .await
                .map_err(|e| eyre!("Failed to send call results via channel: {}", e))?;

            Ok(())
        })
    }
}

#[derive(Debug)]
pub struct EthGetBlockByHash {
    /// The block hash to fetch.
    pub hash: B256,
    /// The node index's to query the block for.
    pub node_idxs: Vec<usize>,
    /// Duration in milliseconds of backoff before fetching the block
    pub backoff: u64,
    /// Tx sender for block results
    pub tx: tokio::sync::mpsc::Sender<Vec<Option<alloy_rpc_types_eth::Block>>>,
}

impl EthGetBlockByHash {
    /// Creates a new `EthGetBlockByHash` action.
    pub fn new(
        hash: B256,
        node_idxs: Vec<usize>,
        backoff: u64,
        tx: tokio::sync::mpsc::Sender<Vec<Option<alloy_rpc_types_eth::Block>>>,
    ) -> Self {
        Self {
            hash,
            node_idxs,
            backoff,
            tx,
        }
    }
}

impl Action<OpEngineTypes> for EthGetBlockByHash {
    fn execute<'a>(
        &'a mut self,
        env: &'a mut Environment<OpEngineTypes>,
    ) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            tokio::time::sleep(Duration::from_millis(self.backoff)).await;

            let mut blocks = vec![];
            for node_idx in &self.node_idxs {
                let rpc_client = env.node_clients[*node_idx].rpc.clone();
                let block: Option<alloy_rpc_types_eth::Block> =
                    EthApiClient::<
                        TransactionRequest,
                        Transaction,
                        alloy_rpc_types_eth::Block,
                        alloy_consensus::Receipt,
                        Header,
                        TransactionSigned,
                    >::block_by_hash(&rpc_client, self.hash, true)
                    .await?;
                blocks.push(block);
            }

            self.tx
                .send(blocks)
                .await
                .map_err(|e| eyre!("Failed to send block results via channel: {}", e))?;

            Ok(())
        })
    }
}

#[derive(Debug)]
pub struct EthGetBalance {
    /// The address to fetch the balance for.
    pub address: Address,
    /// The node index's to query the balance for.
    pub node_idxs: Vec<usize>,
    /// Duration in milliseconds of backoff before fetching the balance
    pub backoff: u64,
    /// Tx sender for balance results
    pub tx: tokio::sync::mpsc::Sender<Vec<U256>>,
}

impl EthGetBalance {
    /// Creates a new `EthGetBalance` action.
    pub fn new(
        address: Address,
        node_idxs: Vec<usize>,
        backoff: u64,
        tx: tokio::sync::mpsc::Sender<Vec<U256>>,
    ) -> Self {
        Self {
            address,
            node_idxs,
            backoff,
            tx,
        }
    }
}

impl Action<OpEngineTypes> for EthGetBalance {
    fn execute<'a>(
        &'a mut self,
        env: &'a mut Environment<OpEngineTypes>,
    ) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            tokio::time::sleep(Duration::from_millis(self.backoff)).await;

            let mut balances = vec![];
            for node_idx in &self.node_idxs {
                let rpc_client = env.node_clients[*node_idx].rpc.clone();
                let balance: U256 = EthApiClient::<
                    TransactionRequest,
                    Transaction,
                    alloy_rpc_types_eth::Block,
                    alloy_consensus::Receipt,
                    Header,
                    TransactionSigned,
                >::balance(
                    &rpc_client, self.address, Some(BlockId::pending())
                )
                .await?;
                balances.push(balance);
            }

            self.tx
                .send(balances)
                .await
                .map_err(|e| eyre!("Failed to send balance results via channel: {}", e))?;

            Ok(())
        })
    }
}

#[derive(Debug)]
pub struct EthGetCode {
    /// The address to fetch the code for.
    pub address: Address,
    /// The node index's to query the code for.
    pub node_idxs: Vec<usize>,
    /// Duration in milliseconds of backoff before fetching the code
    pub backoff: u64,
    /// Tx sender for code results
    pub tx: tokio::sync::mpsc::Sender<Vec<Bytes>>,
}

impl EthGetCode {
    /// Creates a new `EthGetCode` action.
    pub fn new(
        address: Address,
        node_idxs: Vec<usize>,
        backoff: u64,
        tx: tokio::sync::mpsc::Sender<Vec<Bytes>>,
    ) -> Self {
        Self {
            address,
            node_idxs,
            backoff,
            tx,
        }
    }
}

impl Action<OpEngineTypes> for EthGetCode {
    fn execute<'a>(
        &'a mut self,
        env: &'a mut Environment<OpEngineTypes>,
    ) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            tokio::time::sleep(Duration::from_millis(self.backoff)).await;

            let mut codes = vec![];
            for node_idx in &self.node_idxs {
                let rpc_client = env.node_clients[*node_idx].rpc.clone();
                let code: Bytes = EthApiClient::<
                    TransactionRequest,
                    Transaction,
                    alloy_rpc_types_eth::Block,
                    alloy_consensus::Receipt,
                    Header,
                    TransactionSigned,
                >::get_code(
                    &rpc_client, self.address, Some(BlockId::pending())
                )
                .await?;
                codes.push(code);
            }

            self.tx
                .send(codes)
                .await
                .map_err(|e| eyre!("Failed to send code results via channel: {}", e))?;

            Ok(())
        })
    }
}

#[derive(Debug)]
pub struct EthGetTransactionCount {
    /// The address to fetch the transaction count for.
    pub address: Address,
    /// The node index's to query the transaction count for.
    pub node_idxs: Vec<usize>,
    /// Duration in milliseconds of backoff before fetching the transaction count
    pub backoff: u64,
    /// Tx sender for transaction count results
    pub tx: tokio::sync::mpsc::Sender<Vec<U256>>,
}

impl EthGetTransactionCount {
    /// Creates a new `EthGetTransactionCount` action.
    pub fn new(
        address: Address,
        node_idxs: Vec<usize>,
        backoff: u64,
        tx: tokio::sync::mpsc::Sender<Vec<U256>>,
    ) -> Self {
        Self {
            address,
            node_idxs,
            backoff,
            tx,
        }
    }
}

impl Action<OpEngineTypes> for EthGetTransactionCount {
    fn execute<'a>(
        &'a mut self,
        env: &'a mut Environment<OpEngineTypes>,
    ) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            tokio::time::sleep(Duration::from_millis(self.backoff)).await;

            let mut counts = vec![];
            for node_idx in &self.node_idxs {
                let rpc_client = env.node_clients[*node_idx].rpc.clone();
                let count: U256 = EthApiClient::<
                    TransactionRequest,
                    Transaction,
                    alloy_rpc_types_eth::Block,
                    alloy_consensus::Receipt,
                    Header,
                    TransactionSigned,
                >::transaction_count(
                    &rpc_client, self.address, Some(BlockId::pending())
                )
                .await?;
                counts.push(count);
            }

            self.tx.send(counts).await.map_err(|e| {
                eyre!(
                    "Failed to send transaction count results via channel: {}",
                    e
                )
            })?;

            Ok(())
        })
    }
}

#[derive(Debug)]
pub struct EthStorage {
    /// The address to fetch the storage for.
    pub address: Address,
    /// The storage slot to fetch.
    pub slot: U256,
    /// The node index's to query the storage for.
    pub node_idxs: Vec<usize>,
    /// Duration in milliseconds of backoff before fetching the storage
    pub backoff: u64,
    /// Tx sender for storage results
    pub tx: tokio::sync::mpsc::Sender<Vec<B256>>,
}

impl EthStorage {
    /// Creates a new `EthStorage` action.
    pub fn new(
        address: Address,
        slot: U256,
        node_idxs: Vec<usize>,
        backoff: u64,
        tx: tokio::sync::mpsc::Sender<Vec<B256>>,
    ) -> Self {
        Self {
            address,
            slot,
            node_idxs,
            backoff,
            tx,
        }
    }
}

impl Action<OpEngineTypes> for EthStorage {
    fn execute<'a>(
        &'a mut self,
        env: &'a mut Environment<OpEngineTypes>,
    ) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            tokio::time::sleep(Duration::from_millis(self.backoff)).await;

            let mut storages = vec![];
            for node_idx in &self.node_idxs {
                let rpc_client = env.node_clients[*node_idx].rpc.clone();
                let storage: B256 = EthApiClient::<
                    TransactionRequest,
                    Transaction,
                    alloy_rpc_types_eth::Block,
                    alloy_consensus::Receipt,
                    Header,
                    TransactionSigned,
                >::storage_at(
                    &rpc_client,
                    self.address,
                    self.slot.into(),
                    Some(BlockId::pending()),
                )
                .await?;
                storages.push(storage);
            }

            self.tx
                .send(storages)
                .await
                .map_err(|e| eyre!("Failed to send storage results via channel: {}", e))?;

            Ok(())
        })
    }
}

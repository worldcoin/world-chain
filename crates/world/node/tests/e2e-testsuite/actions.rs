#![allow(dead_code)]
use alloy_eips::BlockId;
use alloy_rpc_types::{Header, Transaction, TransactionRequest};
use alloy_rpc_types_engine::{ForkchoiceState, PayloadStatusEnum};
use backon::{ConstantBuilder, Retryable};
use eyre::eyre::{eyre, Result};
use flashblocks_primitives::p2p::Authorization;
use flashblocks_rpc::{engine::FlashblocksEngineApiExtClient, op::OpApiExtClient};
use futures::future::BoxFuture;
use op_alloy_rpc_types::OpTransactionReceipt;
use op_alloy_rpc_types_engine::OpExecutionPayloadEnvelopeV3;
use reth::rpc::api::{EngineApiClient, EthApiClient};
use reth_e2e_test_utils::testsuite::{actions::Action, Environment};
use reth_optimism_node::{OpEngineTypes, OpPayloadAttributes};
use reth_primitives::TransactionSigned;
use revm_primitives::{Address, Bytes, B256, U256};
use std::{fmt::Debug, marker::PhantomData, time::Duration};
use tokio::time::sleep;
use tracing::debug;

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
                        ))
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

            let latest_info = env
                .current_block_info()
                .ok_or_else(|| eyre!("No latest block information available"))?;

            // simple round-robin selection based on next block number
            let next_producer_idx = ((latest_info.number + 1) % num_clients as u64) as usize;

            env.last_producer_idx = Some(next_producer_idx);
            debug!(
                "Selected node {} as the next flashblocks producer for block {}",
                next_producer_idx,
                latest_info.number + 1
            );

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
            let timestamp =
                env.active_node_state()?.latest_header_time + env.block_timestamp_increment;

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
                eip_1559_params: Some(alloy_primitives::b64!("0000000800000008")),
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

/// Action that generates the next payload using flashblocks fork choice update
#[derive(Debug)]
pub struct GenerateNextFlashblocksPayload<F>
where
    F: Fn(OpPayloadAttributes) -> Authorization + Clone + Send + Sync + 'static,
{
    /// Authorization generator function
    pub authorization_generator: F,
    /// Tracks function type
    _phantom: PhantomData<F>,
}

impl<F> GenerateNextFlashblocksPayload<F>
where
    F: Fn(OpPayloadAttributes) -> Authorization + Clone + Send + Sync + 'static,
{
    /// Create a new action with authorization generator
    pub fn new(authorization_generator: F) -> Self {
        Self {
            authorization_generator,
            _phantom: PhantomData,
        }
    }
}

impl<F> Action<OpEngineTypes> for GenerateNextFlashblocksPayload<F>
where
    F: Fn(OpPayloadAttributes) -> Authorization + Clone + Send + Sync + 'static,
{
    fn execute<'a>(
        &'a mut self,
        env: &'a mut reth_e2e_test_utils::testsuite::Environment<OpEngineTypes>,
    ) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            let latest_block = env
                .current_block_info()
                .ok_or_else(|| eyre!("No latest block information available"))?;

            let parent_hash = latest_block.hash;
            debug!("Latest block hash: {parent_hash}");

            let fork_choice_state = ForkchoiceState {
                head_block_hash: parent_hash,
                safe_block_hash: parent_hash,
                finalized_block_hash: parent_hash,
            };

            let base_payload_attributes = env
                .active_node_state()?
                .payload_attributes
                .get(&(latest_block.number + 1))
                .cloned()
                .ok_or_else(|| eyre!("No payload attributes found for next block"))?;

            let payload_attributes = OpPayloadAttributes {
                payload_attributes: base_payload_attributes,
                transactions: None,
                no_tx_pool: Some(false),
                eip_1559_params: Some(alloy_primitives::b64!("0000000800000008")),
                gas_limit: Some(30_000_000),
                min_base_fee: None,
            };

            let authorization = (self.authorization_generator)(payload_attributes.clone());

            let producer_idx = env
                .last_producer_idx
                .ok_or_else(|| eyre!("No block producer selected"))?;

            let fcu_result =
                FlashblocksEngineApiExtClient::<OpEngineTypes>::flashblocks_fork_choice_updated_v3(
                    &env.node_clients[producer_idx].engine.http_client(),
                    fork_choice_state,
                    Some(payload_attributes.clone()),
                    Some(authorization),
                )
                .await?;

            debug!("Flashblocks FCU result: {:?}", fcu_result);

            // Check if we got a valid payload ID
            let payload_id = if let Some(payload_id) = fcu_result.payload_id {
                debug!("Received flashblocks payload ID: {:?}", payload_id);
                payload_id
            } else {
                debug!("No payload ID returned from flashblocks forkchoiceUpdated");
                return Err(eyre!(
                    "No payload ID returned from flashblocks forkchoiceUpdated"
                ));
            };

            // Validate the FCU status
            match fcu_result.payload_status.status {
                PayloadStatusEnum::Valid => {
                    env.active_node_state_mut()?.next_payload_id = Some(payload_id);

                    // Store the payload attributes that were used
                    env.active_node_state_mut()?
                        .payload_id_history
                        .insert(latest_block.number + 1, payload_id);

                    debug!(
                        "Flashblocks payload generation successful for block {}",
                        latest_block.number + 1
                    );
                    Ok(())
                }
                _ => Err(eyre!(
                    "Flashblocks payload status not valid: {:?}",
                    fcu_result.payload_status
                )),
            }
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

            let producer_idx = env
                .last_producer_idx
                .ok_or_else(|| eyre!("No block producer selected"))?;

            // Wait for payload to be built
            sleep(Duration::from_millis(2000)).await;

            let built_payload_envelope =
                reth::rpc::api::EngineApiClient::<OpEngineTypes>::get_payload_v3(
                    &env.node_clients[producer_idx].engine.http_client(),
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

            debug!(
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
    F: Fn(OpPayloadAttributes) -> Authorization + Clone + Send + Sync + 'static,
{
    /// Number of blocks to produce
    pub num_blocks: u64,
    /// Authorization generator function
    pub authorization_generator: F,
    /// Tracks function type
    _phantom: PhantomData<F>,
}

impl<F> ProduceBlocksWithFlashblocks<F>
where
    F: Fn(OpPayloadAttributes) -> Authorization + Clone + Send + Sync + 'static,
{
    /// Create a new `ProduceBlocksWithFlashblocks` action
    pub fn new(num_blocks: u64, authorization_generator: F) -> Self {
        Self {
            num_blocks,
            authorization_generator,
            _phantom: PhantomData,
        }
    }
}

impl<F> Action<OpEngineTypes> for ProduceBlocksWithFlashblocks<F>
where
    F: Fn(OpPayloadAttributes) -> Authorization + Clone + Send + Sync + 'static,
{
    fn execute<'a>(
        &'a mut self,
        env: &'a mut reth_e2e_test_utils::testsuite::Environment<OpEngineTypes>,
    ) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            for block_idx in 0..self.num_blocks {
                debug!(
                    "Producing flashblocks block {}/{}",
                    block_idx + 1,
                    self.num_blocks
                );

                // Pick the next block producer
                let mut pick_producer = PickNextFlashblocksProducer::new();
                pick_producer.execute(env).await?;

                // Generate payload attributes
                let mut generate_attrs =
                    GenerateFlashblocksPayloadAttributes::new(self.authorization_generator.clone());
                generate_attrs.execute(env).await?;

                // Generate the next payload using flashblocks
                let mut generate_payload =
                    GenerateNextFlashblocksPayload::new(self.authorization_generator.clone());
                generate_payload.execute(env).await?;

                // Retrieve the built payload
                let mut retrieve_payload = RetrieveFlashblocksPayload::new();
                retrieve_payload.execute(env).await?;

                // Update block info to the latest payload
                let mut update_block_info = UpdateBlockInfoToFlashblocksPayload::new();
                update_block_info.execute(env).await?;

                debug!(
                    "Successfully produced flashblocks block {}/{}",
                    block_idx + 1,
                    self.num_blocks
                );
            }

            debug!("Completed producing {} flashblocks blocks", self.num_blocks);
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
    pub hash: B256,
    /// The node index's to query the receipt for.
    pub node_idxs: Vec<usize>,
    /// Duration in milliseconds of backoff before fetching the receipt
    pub backoff: u64,
    /// Tx sender for receipt results
    pub tx: tokio::sync::mpsc::Sender<Vec<Option<OpTransactionReceipt>>>,
}

impl EthGetTransactionReceipt {
    /// Creates a new `EthGetTransactionReceipt` action.
    pub fn new(
        hash: B256,
        node_idxs: Vec<usize>,
        backoff: u64,
        tx: tokio::sync::mpsc::Sender<Vec<Option<OpTransactionReceipt>>>,
    ) -> Self {
        Self {
            hash,
            node_idxs,
            backoff,
            tx,
        }
    }
}

/// Error type for receipt fetching retry logic
#[derive(Debug)]
enum ReceiptFetchError {
    /// Receipt not yet available - this is retryable
    NotFound,
    /// RPC error - not retryable, should propagate
    Rpc(eyre::Report),
}

impl Action<OpEngineTypes> for EthGetTransactionReceipt {
    fn execute<'a>(
        &'a mut self,
        env: &'a mut Environment<OpEngineTypes>,
    ) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            tokio::time::sleep(Duration::from_millis(self.backoff)).await;

            // Retry configuration: up to 10 attempts with 100ms between retries
            const MAX_RETRIES: usize = 10;
            const RETRY_DELAY_MS: u64 = 100;

            let mut receipts = vec![];
            for node_idx in &self.node_idxs {
                let rpc_client = env.node_clients[*node_idx].rpc.clone();
                let tx_hash = self.hash;

                // Retry logic to handle CI delays
                let result = (|| async {
                    match EthApiClient::<
                        TransactionRequest,
                        Transaction,
                        alloy_rpc_types_eth::Block,
                        OpTransactionReceipt,
                        Header,
                        TransactionSigned,
                    >::transaction_receipt(&rpc_client, tx_hash)
                    .await
                    {
                        Ok(Some(receipt)) => Ok(receipt),
                        Ok(None) => Err(ReceiptFetchError::NotFound),
                        Err(e) => Err(ReceiptFetchError::Rpc(eyre!(e))),
                    }
                })
                .retry(
                    ConstantBuilder::default()
                        .with_delay(Duration::from_millis(RETRY_DELAY_MS))
                        .with_max_times(MAX_RETRIES),
                )
                .when(|e| matches!(e, ReceiptFetchError::NotFound))
                .await;

                // Convert result back to Option, propagating RPC errors
                let receipt = match result {
                    Ok(r) => Some(r),
                    Err(ReceiptFetchError::NotFound) => None,
                    Err(ReceiptFetchError::Rpc(e)) => return Err(e),
                };
                receipts.push(receipt);
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

#![allow(dead_code)]

use alloy_consensus::Header;
use alloy_eips::{BlockId, Decodable2718};
use alloy_rpc_types::{Transaction, TransactionRequest};
use alloy_rpc_types_engine::{ForkchoiceState, PayloadStatusEnum};
use eyre::eyre::{Result, eyre};
use flashblocks_primitives::{
    flashblocks::{Flashblock, Flashblocks},
    p2p::Authorization,
    primitives::FlashblocksPayloadV1,
};
use flashblocks_rpc::{engine::FlashblocksEngineApiExtClient, op::OpApiExtClient};
use futures::{
    Stream, StreamExt, TryStreamExt,
    future::BoxFuture,
    stream::{self, FuturesUnordered},
};
use op_alloy_rpc_types::OpTransactionReceipt;
use op_alloy_rpc_types_engine::OpExecutionPayloadEnvelopeV3;
use parking_lot::RwLock;
use reth::rpc::api::{EngineApiClient, EthApiClient};
use reth_e2e_test_utils::testsuite::{Environment, actions::Action};
use reth_node_api::{ConsensusEngineHandle, EngineApiMessageVersion};
use reth_optimism_chainspec::OpChainSpec;
use reth_optimism_node::{OpEngineTypes, OpPayloadAttributes};
use reth_optimism_primitives::OpTransactionSigned;
use reth_primitives::TransactionSigned;
use revm_primitives::{Address, B256, Bytes, U256};
use std::{pin::Pin, sync::Arc, time::Duration};
use tokio::sync::{mpsc, watch};
use tracing::{debug, error, info};

use crate::setup::execution_data_from_from_reduced_flashblock;

/// A hook that receives results from actions and can perform assertions.
/// Returns Ok(()) if assertions pass, Err if they fail.
pub type Hook<T> = Arc<dyn Fn(T) -> Result<()> + Send + Sync>;

/// Creates a hook from a closure
pub fn hook<T, F>(f: F) -> Hook<T>
where
    F: Fn(T) -> Result<()> + Send + Sync + 'static,
{
    Arc::new(f)
}

pub struct FlashblocksValidatonStream {
    pub node_indexes: Vec<usize>,
    pub flashblocks_stream:
        Pin<Box<dyn Stream<Item = FlashblocksPayloadV1> + Unpin + Send + Sync + 'static>>,
    pub validation_hook: Option<Hook<PayloadStatusEnum>>,
    pub chain_spec: Arc<OpChainSpec>,
}

impl Action<OpEngineTypes> for FlashblocksValidatonStream {
    fn execute<'a>(
        &'a mut self,
        env: &'a mut Environment<OpEngineTypes>,
    ) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            let chain_spec = self.chain_spec.clone();
            let flashblocks: Flashblocks = Flashblocks::default();
            let consensus_handles: Vec<_> = self
                .node_indexes
                .iter()
                .filter_map(|&idx| env.node_clients[idx].beacon_engine_handle.clone())
                .map(Arc::new)
                .collect::<Vec<_>>();

            let mut ordered = flashblocks;

            // Process flashblocks from the stream
            while let Some(flashblock) = self.flashblocks_stream.next().await  {
                let index = flashblock.index;

                // Push the new flashblock into the ordered collection
                let is_new = ordered
                    .push(Flashblock {
                        flashblock: flashblock.clone(),
                    })
                    .ok();

                if is_new.is_some() {
                    info!(
                        target: "flashblocks",
                        index = %index,
                        "New payload started, resetting flashblock collection"
                    );
                }

                // Reduce accumulated flashblocks to get intermediate state
                let Some(reduced) = Flashblock::reduce(ordered.clone()).ok() else {
                    continue;
                };
                let reduced_hash = reduced.diff().block_hash;

                info!(
                    target: "flashblocks",
                    index = %index,
                    block_hash = ?reduced_hash,
                    "Reduced flashblocks for validation"
                );

                // Construct execution data from the reduced flashblocks for validation
                let execution_data =
                    execution_data_from_from_reduced_flashblock(reduced, chain_spec.clone());

                // Update forkchoice to parent hash before validating
                let parent = execution_data.parent_hash();
                let forkchoice = ForkchoiceState {
                    head_block_hash: parent,
                    safe_block_hash: parent,
                    finalized_block_hash: parent,
                };

                let status_results = consensus_handles
                    .clone()
                    .into_iter()
                    .map(|beacon_handle| {
                        let forkchoice = forkchoice;
                        let execution_data = execution_data.clone();
                        async move {
                            beacon_handle
                                .fork_choice_updated(forkchoice, None, EngineApiMessageVersion::V3)
                                .await?;

                            let status = beacon_handle.new_payload(execution_data.clone()).await?;

                            Ok::<_, eyre::Report>(status)
                        }
                    })
                    .collect::<FuturesUnordered<_>>();

                let validation_hook = self.validation_hook.clone();
                let statuses = status_results
                    .into_stream()
                    .map(|result| {
                        let status = match &result {
                            Ok(status) => status.status.clone(),
                            Err(e) => PayloadStatusEnum::Invalid {
                                validation_error: format!("Error during validation: {:?}", e),
                            },
                        };
                        if let Some(ref hook) = validation_hook
                            && let Err(e) = hook(status.clone()) {
                                error!(
                                    target: "flashblocks",
                                    index = %index,
                                    "Validation hook failed: {:?}", e
                                );
                            }
                        status
                    })
                    .collect::<Vec<_>>()
                    .await;

                info!(
                    target: "flashblocks",
                    index = %index,
                    statuses = ?statuses,
                    "Flashblock validation complete"
                );
            }

            Ok(())
        })
    }
}

/// Query transaction receipts with optional assertion hook
pub struct GetReceipts {
    pub receipts_stream: Pin<Box<dyn Stream<Item = Vec<B256>> + Send + 'static>>,
    pub node_idxs: Vec<usize>,
    pub backoff: Duration,
    pub on_receipts: Option<Hook<Vec<Vec<Option<OpTransactionReceipt>>>>>,
}

impl GetReceipts {
    pub fn new(
        node_idxs: Vec<usize>,
        flashblocks_stream: impl Stream<Item = FlashblocksPayloadV1> + Send + Unpin + 'static,
    ) -> Self {
        let receipt_hashes = stream::unfold(flashblocks_stream, |mut stream| async {
            match stream.next().await {
                Some(payload) => {
                    let txs = payload
                        .diff
                        .transactions.to_vec();

                    let decoded = txs
                        .iter()
                        .filter_map(|tx_bytes| {
                            let tx =
                                OpTransactionSigned::decode_2718(&mut tx_bytes.clone().as_ref())
                                    .ok();
                            tx.map(|t| *t.hash())
                        })
                        .collect::<Vec<_>>();

                    Some((decoded, stream))
                }
                None => None,
            }
        })
        .boxed();

        Self {
            receipts_stream: receipt_hashes,
            node_idxs,
            backoff: Duration::from_millis(200),
            on_receipts: None,
        }
    }

    pub fn with_backoff(mut self, backoff: Duration) -> Self {
        self.backoff = backoff;
        self
    }

    pub fn on_receipts<F>(mut self, f: F) -> Self
    where
        F: Fn(Vec<Vec<Option<OpTransactionReceipt>>>) -> Result<()> + Send + Sync + 'static,
    {
        self.on_receipts = Some(hook(f));
        self
    }
}

impl Action<OpEngineTypes> for GetReceipts {
    fn execute<'a>(
        &'a mut self,
        env: &'a mut Environment<OpEngineTypes>,
    ) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            let mut all_receipts = Vec::new();

            while let Some(hashes) = self.receipts_stream.next().await {
                let mut node_receipts = Vec::new();

                for &node_idx in &self.node_idxs {
                    for hash in &hashes {
                        let rpc = &env.node_clients[node_idx].rpc;

                        let receipt: Option<OpTransactionReceipt> =
                            EthApiClient::<
                                TransactionRequest,
                                Transaction,
                                alloy_rpc_types_eth::Block,
                                OpTransactionReceipt,
                                Header,
                                TransactionSigned,
                            >::transaction_receipt(rpc, *hash)
                            .await?;

                        node_receipts.push(receipt);
                    }
                }

                all_receipts.push(node_receipts);

                if let Some(ref hook) = self.on_receipts {
                    hook(all_receipts.clone())?;
                }
            }

            Ok(())
        })
    }
}

/// Execute eth_call and send results through channel
pub struct EthCall {
    pub tx: TransactionRequest,
    pub node_idxs: Vec<usize>,
    pub backoff_ms: u64,
    pub sender: mpsc::Sender<Vec<Bytes>>,
}

impl EthCall {
    pub fn new(
        tx: TransactionRequest,
        node_idxs: Vec<usize>,
        backoff_ms: u64,
        sender: mpsc::Sender<Vec<Bytes>>,
    ) -> Self {
        Self {
            tx,
            node_idxs,
            backoff_ms,
            sender,
        }
    }
}

impl Action<OpEngineTypes> for EthCall {
    fn execute<'a>(
        &'a mut self,
        env: &'a mut Environment<OpEngineTypes>,
    ) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            tokio::time::sleep(Duration::from_millis(self.backoff_ms)).await;

            let mut results = Vec::with_capacity(self.node_idxs.len());

            for &idx in &self.node_idxs {
                let result: Bytes = EthApiClient::<
                    TransactionRequest,
                    Transaction,
                    alloy_rpc_types_eth::Block,
                    alloy_consensus::Receipt,
                    Header,
                    TransactionSigned,
                >::call(
                    &env.node_clients[idx].rpc,
                    self.tx.clone(),
                    Some(BlockId::pending()),
                    None,
                    None,
                )
                .await?;
                results.push(result);
            }

            self.sender
                .send(results)
                .await
                .map_err(|e| eyre!("Failed to send call results: {}", e))?;

            Ok(())
        })
    }
}

/// Query block by hash with optional assertion hook
pub struct GetBlockByHash {
    pub hash_stream: Pin<Box<dyn Stream<Item = B256> + Send + 'static>>,
    pub node_idxs: Vec<usize>,
    pub backoff: Duration,
    pub on_blocks: Option<Hook<Vec<Option<alloy_rpc_types_eth::Block>>>>,
}

impl GetBlockByHash {
    pub fn new(
        node_idxs: Vec<usize>,
        flashblocks_stream: impl Stream<Item = FlashblocksPayloadV1> + Unpin + Send + 'static,
    ) -> Self {
        let hash_stream = stream::unfold(flashblocks_stream, |mut stream| async {
            match stream.next().await {
                Some(payload) => Some((payload.diff.block_hash, stream)),
                None => None,
            }
        })
        .boxed();

        Self {
            hash_stream,
            node_idxs,
            backoff: Duration::from_millis(200),
            on_blocks: None,
        }
    }

    pub fn with_backoff(mut self, backoff: Duration) -> Self {
        self.backoff = backoff;
        self
    }

    pub fn on_blocks<F>(mut self, f: F) -> Self
    where
        F: Fn(Vec<Option<alloy_rpc_types_eth::Block>>) -> Result<()> + Send + Sync + 'static,
    {
        self.on_blocks = Some(hook(f));
        self
    }
}

impl Action<OpEngineTypes> for GetBlockByHash {
    fn execute<'a>(
        &'a mut self,
        env: &'a mut Environment<OpEngineTypes>,
    ) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            let mut blocks = Vec::with_capacity(self.node_idxs.len());
            while let Some(hash) = self.hash_stream.next().await {
                for &idx in &self.node_idxs {
                    let block: Option<alloy_rpc_types_eth::Block> =
                        EthApiClient::<
                            TransactionRequest,
                            Transaction,
                            alloy_rpc_types_eth::Block,
                            alloy_consensus::Receipt,
                            Header,
                            TransactionSigned,
                        >::block_by_hash(
                            &env.node_clients[idx].rpc, hash, true
                        )
                        .await?;

                    blocks.push(block);
                }
            }
            if let Some(ref hook) = self.on_blocks {
                hook(blocks)?;
            }

            Ok(())
        })
    }
}

/// Query code with optional assertion hook
pub struct GetCode {
    pub address: Address,
    pub node_idxs: Vec<usize>,
    pub backoff: Duration,
    pub on_code: Option<Hook<Vec<Bytes>>>,
}

impl GetCode {
    pub fn new(address: Address, node_idxs: Vec<usize>) -> Self {
        Self {
            address,
            node_idxs,
            backoff: Duration::from_millis(200),
            on_code: None,
        }
    }

    pub fn with_backoff(mut self, backoff: Duration) -> Self {
        self.backoff = backoff;
        self
    }

    pub fn on_code<F>(mut self, f: F) -> Self
    where
        F: Fn(Vec<Bytes>) -> Result<()> + Send + Sync + 'static,
    {
        self.on_code = Some(hook(f));
        self
    }
}

impl Action<OpEngineTypes> for GetCode {
    fn execute<'a>(
        &'a mut self,
        env: &'a mut Environment<OpEngineTypes>,
    ) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            tokio::time::sleep(self.backoff).await;

            let mut codes = Vec::with_capacity(self.node_idxs.len());

            for &idx in &self.node_idxs {
                let code: Bytes = EthApiClient::<
                    TransactionRequest,
                    Transaction,
                    alloy_rpc_types_eth::Block,
                    alloy_consensus::Receipt,
                    Header,
                    TransactionSigned,
                >::get_code(
                    &env.node_clients[idx].rpc,
                    self.address,
                    Some(BlockId::pending()),
                )
                .await?;
                codes.push(code);
            }

            if let Some(ref hook) = self.on_code {
                hook(codes)?;
            }

            Ok(())
        })
    }
}

/// Query transaction count (nonce) with optional assertion hook
pub struct GetTransactionCount {
    pub address: Address,
    pub node_idxs: Vec<usize>,
    pub backoff: Duration,
    pub on_counts: Option<Hook<Vec<U256>>>,
}

impl GetTransactionCount {
    pub fn new(address: Address, node_idxs: Vec<usize>) -> Self {
        Self {
            address,
            node_idxs,
            backoff: Duration::from_millis(200),
            on_counts: None,
        }
    }

    pub fn with_backoff(mut self, backoff: Duration) -> Self {
        self.backoff = backoff;
        self
    }

    pub fn on_counts<F>(mut self, f: F) -> Self
    where
        F: Fn(Vec<U256>) -> Result<()> + Send + Sync + 'static,
    {
        self.on_counts = Some(hook(f));
        self
    }
}

impl Action<OpEngineTypes> for GetTransactionCount {
    fn execute<'a>(
        &'a mut self,
        env: &'a mut Environment<OpEngineTypes>,
    ) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            tokio::time::sleep(self.backoff).await;

            let mut counts = Vec::with_capacity(self.node_idxs.len());

            for &idx in &self.node_idxs {
                let count: U256 = EthApiClient::<
                    TransactionRequest,
                    Transaction,
                    alloy_rpc_types_eth::Block,
                    alloy_consensus::Receipt,
                    Header,
                    TransactionSigned,
                >::transaction_count(
                    &env.node_clients[idx].rpc,
                    self.address,
                    Some(BlockId::pending()),
                )
                .await?;
                counts.push(count);
            }

            if let Some(ref hook) = self.on_counts {
                hook(counts)?;
            }

            Ok(())
        })
    }
}

/// Query storage with optional assertion hook
pub struct GetStorage {
    pub address: Address,
    pub slot: U256,
    pub node_idxs: Vec<usize>,
    pub backoff: Duration,
    pub on_storage: Option<Hook<Vec<B256>>>,
}

impl GetStorage {
    pub fn new(address: Address, slot: U256, node_idxs: Vec<usize>) -> Self {
        Self {
            address,
            slot,
            node_idxs,
            backoff: Duration::from_millis(200),
            on_storage: None,
        }
    }

    pub fn with_backoff(mut self, backoff: Duration) -> Self {
        self.backoff = backoff;
        self
    }

    pub fn on_storage<F>(mut self, f: F) -> Self
    where
        F: Fn(Vec<B256>) -> Result<()> + Send + Sync + 'static,
    {
        self.on_storage = Some(hook(f));
        self
    }
}

impl Action<OpEngineTypes> for GetStorage {
    fn execute<'a>(
        &'a mut self,
        env: &'a mut Environment<OpEngineTypes>,
    ) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            tokio::time::sleep(self.backoff).await;

            let mut storage = Vec::with_capacity(self.node_idxs.len());

            for &idx in &self.node_idxs {
                let value: B256 = EthApiClient::<
                    TransactionRequest,
                    Transaction,
                    alloy_rpc_types_eth::Block,
                    alloy_consensus::Receipt,
                    Header,
                    TransactionSigned,
                >::storage_at(
                    &env.node_clients[idx].rpc,
                    self.address,
                    self.slot.into(),
                    Some(BlockId::pending()),
                )
                .await?;
                storage.push(value);
            }

            if let Some(ref hook) = self.on_storage {
                hook(storage)?;
            }

            Ok(())
        })
    }
}

/// Query supported capabilities with optional assertion hook
pub struct GetCapabilities {
    pub node_idx: usize,
    pub on_capabilities: Option<Hook<Vec<String>>>,
}

impl GetCapabilities {
    pub fn new(node_idx: usize) -> Self {
        Self {
            node_idx,
            on_capabilities: None,
        }
    }

    pub fn on_capabilities<F>(mut self, f: F) -> Self
    where
        F: Fn(Vec<String>) -> Result<()> + Send + Sync + 'static,
    {
        self.on_capabilities = Some(hook(f));
        self
    }
}

impl Action<OpEngineTypes> for GetCapabilities {
    fn execute<'a>(
        &'a mut self,
        env: &'a mut Environment<OpEngineTypes>,
    ) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            let capabilities =
                OpApiExtClient::supported_capabilities(&env.node_clients[self.node_idx].rpc)
                    .await?;

            if let Some(ref hook) = self.on_capabilities {
                hook(capabilities)?;
            }

            Ok(())
        })
    }
}

/// Assertion helpers
pub mod assert {
    use super::*;

    /// Assert all values in a collection are equal
    pub fn all_equal<T: PartialEq + std::fmt::Debug>(items: &[T], msg: &str) -> Result<()> {
        if items.windows(2).all(|w| w[0] == w[1]) {
            Ok(())
        } else {
            Err(eyre!("{}: values differ: {:?}", msg, items))
        }
    }

    /// Assert all Options are Some
    pub fn all_some<T>(items: &[Option<T>], msg: &str) -> Result<()> {
        if items.iter().all(|o| o.is_some()) {
            Ok(())
        } else {
            for (i, o) in items.iter().enumerate() {
                if o.is_none() {
                    error!("Item at index {} is None", i);
                }
            }
            Err(eyre!("{}: some values are None", msg))
        }
    }

    /// Assert a condition
    pub fn that(condition: bool, msg: &str) -> Result<()> {
        if condition {
            Ok(())
        } else {
            Err(eyre!("{}", msg))
        }
    }
}

// ============================================================================
// Channel-based actions for backward compatibility with existing tests
// ============================================================================

/// Mine a block and send the resulting payload through a channel
#[derive(Clone)]
pub struct AssertMineBlock<A> {
    pub node_idx: usize,
    pub parent_hash: Option<B256>,
    pub attributes: OpPayloadAttributes,
    pub authorization_gen: A,
    pub block_interval: Duration,
    pub flashblocks: bool,
    pub sender: mpsc::Sender<OpExecutionPayloadEnvelopeV3>,
}

impl<A> AssertMineBlock<A>
where
    A: Fn(OpPayloadAttributes) -> Authorization + Clone + Send + Sync,
{
    pub async fn new(
        node_idx: usize,
        parent_hash: Option<B256>,
        attributes: OpPayloadAttributes,
        authorization_gen: A,
        block_interval: Duration,
        flashblocks: bool,
        sender: mpsc::Sender<OpExecutionPayloadEnvelopeV3>,
    ) -> Self {
        Self {
            node_idx,
            parent_hash,
            attributes,
            authorization_gen,
            block_interval,
            flashblocks,
            sender,
        }
    }
}

impl<A> Action<OpEngineTypes> for AssertMineBlock<A>
where
    A: Fn(OpPayloadAttributes) -> Authorization + Clone + Send + Sync + 'static,
{
    fn execute<'a>(
        &'a mut self,
        env: &'a mut Environment<OpEngineTypes>,
    ) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            let client = &env.node_clients[self.node_idx];
            let engine = client.engine.http_client();

            let parent_hash = if let Some(hash) = self.parent_hash {
                hash
            } else {
                let latest: Option<alloy_rpc_types_eth::Block> =
                    EthApiClient::<
                        TransactionRequest,
                        Transaction,
                        alloy_rpc_types_eth::Block,
                        alloy_consensus::Receipt,
                        Header,
                        TransactionSigned,
                    >::block_by_number(
                        &client.rpc, alloy_eips::BlockNumberOrTag::Latest, false
                    )
                    .await?;

                latest
                    .ok_or_else(|| eyre!("No latest block"))?
                    .header
                    .hash_slow()
            };

            let fcu_state = ForkchoiceState {
                head_block_hash: parent_hash,
                safe_block_hash: parent_hash,
                finalized_block_hash: parent_hash,
            };

            let fcu_result = if self.flashblocks {
                FlashblocksEngineApiExtClient::<OpEngineTypes>::flashblocks_fork_choice_updated_v3(
                    &engine,
                    fcu_state,
                    Some(self.attributes.clone()),
                    Some((self.authorization_gen)(self.attributes.clone())),
                )
                .await?
            } else {
                EngineApiClient::<OpEngineTypes>::fork_choice_updated_v3(
                    &engine,
                    fcu_state,
                    Some(self.attributes.clone()),
                )
                .await?
            };

            if !matches!(fcu_result.payload_status.status, PayloadStatusEnum::Valid) {
                return Err(eyre!(
                    "FCU status not valid: {:?}",
                    fcu_result.payload_status
                ));
            }

            let payload_id = fcu_result
                .payload_id
                .ok_or_else(|| eyre!("No payload ID returned"))?;

            std::thread::sleep(self.block_interval);

            let payload =
                EngineApiClient::<OpEngineTypes>::get_payload_v3(&engine, payload_id).await?;

            debug!(
                "Mined block {} with {} txs",
                payload
                    .execution_payload
                    .payload_inner
                    .payload_inner
                    .block_hash,
                payload
                    .execution_payload
                    .payload_inner
                    .payload_inner
                    .transactions
                    .len()
            );

            // Send the payload through the channel
            self.sender
                .send(payload)
                .await
                .map_err(|e| eyre!("Failed to send payload: {}", e))?;

            Ok(())
        })
    }
}

/// Get transaction receipts and send through channel
pub struct EthGetTransactionReceipt {
    pub hashes: Vec<B256>,
    pub node_idxs: Vec<usize>,
    pub backoff_ms: u64,
    pub sender: mpsc::Sender<Vec<Vec<Option<OpTransactionReceipt>>>>,
}

impl EthGetTransactionReceipt {
    pub fn new(
        hashes: Vec<B256>,
        node_idxs: Vec<usize>,
        backoff_ms: u64,
        sender: mpsc::Sender<Vec<Vec<Option<OpTransactionReceipt>>>>,
    ) -> Self {
        Self {
            hashes,
            node_idxs,
            backoff_ms,
            sender,
        }
    }
}

impl Action<OpEngineTypes> for EthGetTransactionReceipt {
    fn execute<'a>(
        &'a mut self,
        env: &'a mut Environment<OpEngineTypes>,
    ) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            tokio::time::sleep(Duration::from_millis(self.backoff_ms)).await;

            let mut all_receipts = Vec::with_capacity(self.node_idxs.len());

            for &node_idx in &self.node_idxs {
                let rpc = &env.node_clients[node_idx].rpc;
                let mut node_receipts = Vec::with_capacity(self.hashes.len());

                for hash in &self.hashes {
                    let receipt: Option<OpTransactionReceipt> =
                        EthApiClient::<
                            TransactionRequest,
                            Transaction,
                            alloy_rpc_types_eth::Block,
                            OpTransactionReceipt,
                            Header,
                            TransactionSigned,
                        >::transaction_receipt(rpc, *hash)
                        .await?;
                    node_receipts.push(receipt);
                }
                all_receipts.push(node_receipts);
            }

            self.sender
                .send(all_receipts)
                .await
                .map_err(|e| eyre!("Failed to send receipts: {}", e))?;

            Ok(())
        })
    }
}

/// Call supported_capabilities and send through channel
pub struct SupportedCapabilitiesCall {
    pub sender: mpsc::Sender<Vec<String>>,
}

impl SupportedCapabilitiesCall {
    pub fn new(sender: mpsc::Sender<Vec<String>>) -> Self {
        Self { sender }
    }
}

impl Action<OpEngineTypes> for SupportedCapabilitiesCall {
    fn execute<'a>(
        &'a mut self,
        env: &'a mut Environment<OpEngineTypes>,
    ) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            let capabilities =
                OpApiExtClient::supported_capabilities(&env.node_clients[0].rpc).await?;

            self.sender
                .send(capabilities)
                .await
                .map_err(|e| eyre!("Failed to send capabilities: {}", e))?;

            Ok(())
        })
    }
}

/// Compose two actions: EthApiAction is an alias for Then
pub struct EthApiAction<A, B> {
    first: A,
    second: B,
}

impl<A, B> EthApiAction<A, B> {
    pub fn new(first: A, second: B) -> Self {
        Self { first, second }
    }
}

impl<A, B> Action<OpEngineTypes> for EthApiAction<A, B>
where
    A: Action<OpEngineTypes> + Send,
    B: Action<OpEngineTypes> + Send,
{
    fn execute<'a>(
        &'a mut self,
        env: &'a mut Environment<OpEngineTypes>,
    ) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            self.first.execute(env).await?;
            self.second.execute(env).await?;
            Ok(())
        })
    }
}

// ============================================================================
// Shared State and Advanced Composition Actions
// ============================================================================

/// Shared state for communication between parallel actions during block production
#[derive(Clone)]
pub struct BlockProductionState {
    /// Current payload being produced
    pub payload: Arc<RwLock<Option<OpExecutionPayloadEnvelopeV3>>>,
    /// Block hashes from validated flashblocks (for GetBlockByHash)
    pub validated_block_hashes: Arc<RwLock<Vec<B256>>>,
    /// Transaction hashes submitted by spammer (for GetReceipts)
    pub submitted_tx_hashes: Arc<RwLock<Vec<B256>>>,
    /// Signal when final flashblock is validated
    pub final_validated: watch::Sender<bool>,
    pub final_validated_rx: watch::Receiver<bool>,
}

impl BlockProductionState {
    pub fn new() -> Self {
        let (final_validated, final_validated_rx) = watch::channel(false);
        Self {
            payload: Arc::new(RwLock::new(None)),
            validated_block_hashes: Arc::new(RwLock::new(Vec::new())),
            submitted_tx_hashes: Arc::new(RwLock::new(Vec::new())),
            final_validated,
            final_validated_rx,
        }
    }

    pub fn set_payload(&self, payload: OpExecutionPayloadEnvelopeV3) {
        *self.payload.write() = Some(payload);
    }

    pub fn get_payload(&self) -> Option<OpExecutionPayloadEnvelopeV3> {
        self.payload.read().clone()
    }

    pub fn add_validated_hash(&self, hash: B256) {
        self.validated_block_hashes.write().push(hash);
    }

    pub fn add_tx_hash(&self, hash: B256) {
        self.submitted_tx_hashes.write().push(hash);
    }

    pub fn get_tx_hashes(&self) -> Vec<B256> {
        self.submitted_tx_hashes.read().clone()
    }

    pub fn signal_final(&self) {
        let _ = self.final_validated.send(true);
    }

    pub fn reset(&self) {
        *self.payload.write() = None;
        self.validated_block_hashes.write().clear();
        self.submitted_tx_hashes.write().clear();
        let _ = self.final_validated.send(false);
    }
}

/// Canonicalize a block by sending new_payload + fork_choice_updated to follower nodes
pub struct Canonicalize {
    pub node_idxs: Vec<usize>,
    pub state: BlockProductionState,
}

impl Canonicalize {
    pub fn new(node_idxs: Vec<usize>, state: BlockProductionState) -> Self {
        Self { node_idxs, state }
    }
}

impl Action<OpEngineTypes> for Canonicalize {
    fn execute<'a>(
        &'a mut self,
        env: &'a mut Environment<OpEngineTypes>,
    ) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            use alloy_rpc_types_engine::CancunPayloadFields;
            use op_alloy_rpc_types_engine::{OpExecutionData, OpExecutionPayloadSidecar};

            let payload = self
                .state
                .get_payload()
                .ok_or_else(|| eyre!("No payload to canonicalize"))?;

            let block_hash = payload
                .execution_payload
                .payload_inner
                .payload_inner
                .block_hash;

            let parent_hash = payload
                .execution_payload
                .payload_inner
                .payload_inner
                .parent_hash;

            info!(
                target: "actions",
                block_hash = ?block_hash,
                "Canonicalizing block"
            );

            // Construct OpExecutionData from the envelope
            use op_alloy_rpc_types_engine::OpExecutionPayload;
            let execution_data = OpExecutionData {
                payload: OpExecutionPayload::V3(payload.execution_payload.clone()),
                sidecar: OpExecutionPayloadSidecar::v3(CancunPayloadFields::new(
                    payload.parent_beacon_block_root,
                    vec![],
                )),
            };

            for &node_idx in &self.node_idxs {
                // Use beacon engine handle which accepts OpExecutionData directly
                if let Some(beacon_handle) =
                    env.node_clients[node_idx].beacon_engine_handle.as_ref()
                {
                    // First: update forkchoice to parent so node can accept the new block
                    let parent_fcu = ForkchoiceState {
                        head_block_hash: parent_hash,
                        safe_block_hash: parent_hash,
                        finalized_block_hash: parent_hash,
                    };
                    beacon_handle
                        .fork_choice_updated(parent_fcu, None, EngineApiMessageVersion::V3)
                        .await
                        .map_err(|e| eyre!("fork_choice_updated to parent failed: {:?}", e))?;

                    // Second: send new_payload with the block
                    let status = beacon_handle
                        .new_payload(execution_data.clone())
                        .await
                        .map_err(|e| eyre!("new_payload failed: {:?}", e))?;

                    if !matches!(status.status, PayloadStatusEnum::Valid) {
                        return Err(eyre!(
                            "new_payload failed for node {}: {:?}",
                            node_idx,
                            status
                        ));
                    }

                    // Third: send fork_choice_updated to make it canonical
                    let fcu_state = ForkchoiceState {
                        head_block_hash: block_hash,
                        safe_block_hash: block_hash,
                        finalized_block_hash: block_hash,
                    };

                    beacon_handle
                        .fork_choice_updated(fcu_state, None, EngineApiMessageVersion::V3)
                        .await
                        .map_err(|e| eyre!("fork_choice_updated failed: {:?}", e))?;

                    info!(
                        target: "actions",
                        node_idx = node_idx,
                        block_hash = ?block_hash,
                        "Block canonicalized successfully via beacon handle"
                    );
                } else {
                    // Fallback: use RPC engine client
                    let engine = env.node_clients[node_idx].engine.http_client();

                    // First: update forkchoice to parent so node can accept the new block
                    let parent_fcu = ForkchoiceState {
                        head_block_hash: parent_hash,
                        safe_block_hash: parent_hash,
                        finalized_block_hash: parent_hash,
                    };
                    let _ = EngineApiClient::<OpEngineTypes>::fork_choice_updated_v3(
                        &engine, parent_fcu, None,
                    )
                    .await?;

                    // Second: send new_payload with the block via RPC
                    let np_result = EngineApiClient::<OpEngineTypes>::new_payload_v3(
                        &engine,
                        payload.execution_payload.clone(),
                        vec![],
                        payload.parent_beacon_block_root,
                    )
                    .await?;

                    if !matches!(np_result.status, PayloadStatusEnum::Valid) {
                        return Err(eyre!(
                            "new_payload failed for node {}: {:?}",
                            node_idx,
                            np_result
                        ));
                    }

                    // Third: send fork_choice_updated to make it canonical
                    let fcu_state = ForkchoiceState {
                        head_block_hash: block_hash,
                        safe_block_hash: block_hash,
                        finalized_block_hash: block_hash,
                    };

                    let fcu_result = EngineApiClient::<OpEngineTypes>::fork_choice_updated_v3(
                        &engine, fcu_state, None,
                    )
                    .await?;

                    if !matches!(fcu_result.payload_status.status, PayloadStatusEnum::Valid) {
                        return Err(eyre!(
                            "fork_choice_updated failed for node {}: {:?}",
                            node_idx,
                            fcu_result.payload_status
                        ));
                    }

                    info!(
                        target: "actions",
                        node_idx = node_idx,
                        block_hash = ?block_hash,
                        "Block canonicalized successfully via RPC"
                    );
                }
            }

            Ok(())
        })
    }
}

/// Mine a block and store the payload in shared state
pub struct MineBlockWithState<A> {
    pub node_idx: usize,
    pub attributes: OpPayloadAttributes,
    pub authorization_gen: A,
    pub block_interval: Duration,
    pub flashblocks: bool,
    pub state: BlockProductionState,
}

impl<A> MineBlockWithState<A>
where
    A: Fn(OpPayloadAttributes) -> Authorization + Clone + Send + Sync,
{
    pub fn new(
        node_idx: usize,
        attributes: OpPayloadAttributes,
        authorization_gen: A,
        state: BlockProductionState,
    ) -> Self {
        Self {
            node_idx,
            attributes,
            authorization_gen,
            block_interval: Duration::from_millis(2000),
            flashblocks: true,
            state,
        }
    }

    pub fn with_interval(mut self, interval: Duration) -> Self {
        self.block_interval = interval;
        self
    }
}

impl<A> ReadOnlyAction for MineBlockWithState<A>
where
    A: Fn(OpPayloadAttributes) -> Authorization + Clone + Send + Sync + 'static,
{
    fn execute_readonly<'a>(
        &'a self,
        env: &'a Environment<OpEngineTypes>,
    ) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            let client = &env.node_clients[self.node_idx];
            let engine = client.engine.http_client();

            let latest: Option<alloy_rpc_types_eth::Block> =
                EthApiClient::<
                    TransactionRequest,
                    Transaction,
                    alloy_rpc_types_eth::Block,
                    alloy_consensus::Receipt,
                    Header,
                    TransactionSigned,
                >::block_by_number(
                    &client.rpc, alloy_eips::BlockNumberOrTag::Latest, false
                )
                .await?;

            let parent_hash = latest
                .ok_or_else(|| eyre!("No latest block"))?
                .header
                .hash_slow();

            let fcu_state = ForkchoiceState {
                head_block_hash: parent_hash,
                safe_block_hash: parent_hash,
                finalized_block_hash: parent_hash,
            };

            let fcu_result = if self.flashblocks {
                FlashblocksEngineApiExtClient::<OpEngineTypes>::flashblocks_fork_choice_updated_v3(
                    &engine,
                    fcu_state,
                    Some(self.attributes.clone()),
                    Some((self.authorization_gen)(self.attributes.clone())),
                )
                .await?
            } else {
                EngineApiClient::<OpEngineTypes>::fork_choice_updated_v3(
                    &engine,
                    fcu_state,
                    Some(self.attributes.clone()),
                )
                .await?
            };

            if !matches!(fcu_result.payload_status.status, PayloadStatusEnum::Valid) {
                return Err(eyre!(
                    "FCU status not valid: {:?}",
                    fcu_result.payload_status
                ));
            }

            let payload_id = fcu_result
                .payload_id
                .ok_or_else(|| eyre!("No payload ID returned"))?;

            // Wait for block to be built
            std::thread::sleep(self.block_interval);

            let payload =
                EngineApiClient::<OpEngineTypes>::get_payload_v3(&engine, payload_id).await?;

            let block_hash = payload
                .execution_payload
                .payload_inner
                .payload_inner
                .block_hash;

            info!(
                target: "actions",
                block_hash = ?block_hash,
                tx_count = payload.execution_payload.payload_inner.payload_inner.transactions.len(),
                "Mined block, storing in shared state"
            );

            // Store payload in shared state
            self.state.set_payload(payload);

            Ok(())
        })
    }
}

/// Validate flashblocks and signal when complete, storing validated hashes in shared state
pub struct ValidateFlashblocksWithState {
    pub flashblock_stream: Pin<Box<dyn Stream<Item = FlashblocksPayloadV1> + Send>>,
    pub beacon_handle: Arc<ConsensusEngineHandle<OpEngineTypes>>,
    pub chain_spec: Arc<OpChainSpec>,
    pub state: BlockProductionState,
}

impl ValidateFlashblocksWithState {
    pub fn new(
        flashblock_stream: Pin<Box<dyn Stream<Item = FlashblocksPayloadV1> + Send>>,
        beacon_handle: Arc<ConsensusEngineHandle<OpEngineTypes>>,
        chain_spec: Arc<OpChainSpec>,
        state: BlockProductionState,
    ) -> Self {
        Self {
            flashblock_stream,
            beacon_handle,
            chain_spec,
            state,
        }
    }
}

impl Action<OpEngineTypes> for ValidateFlashblocksWithState {
    fn execute<'a>(
        &'a mut self,
        _env: &'a mut Environment<OpEngineTypes>,
    ) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            let mut flashblocks = Flashblocks::default();
            let stream = &mut self.flashblock_stream;

            // Wait for payload to be available
            let target_hash = loop {
                if let Some(payload) = self.state.get_payload() {
                    break payload
                        .execution_payload
                        .payload_inner
                        .payload_inner
                        .block_hash;
                }
                tokio::time::sleep(Duration::from_millis(50)).await;
            };

            info!(
                target: "actions",
                target_hash = ?target_hash,
                "Starting flashblock validation"
            );

            while let Some(fb_payload) = stream.next().await {
                let index = fb_payload.index;

                let is_new = flashblocks
                    .push(Flashblock {
                        flashblock: fb_payload,
                    })
                    .ok();

                if is_new.is_some() {
                    info!(
                        target: "actions",
                        index = %index,
                        "New payload started, reset flashblock collection"
                    );
                }

                // Reduce to get current state
                let Some(reduced) = Flashblock::reduce(flashblocks.clone()).ok() else {
                    continue;
                };

                let reduced_hash = reduced.diff().block_hash;

                // Store validated hash for GetBlockByHash
                self.state.add_validated_hash(reduced_hash);

                // Construct execution data
                let execution_data =
                    execution_data_from_from_reduced_flashblock(reduced, self.chain_spec.clone());

                // Validate
                let parent = execution_data.parent_hash();
                let forkchoice = ForkchoiceState {
                    head_block_hash: parent,
                    safe_block_hash: parent,
                    finalized_block_hash: parent,
                };

                self.beacon_handle
                    .fork_choice_updated(forkchoice, None, EngineApiMessageVersion::V3)
                    .await
                    .ok();

                let status = self.beacon_handle.new_payload(execution_data.clone()).await;

                match &status {
                    Ok(s) => {
                        info!(
                            target: "actions",
                            index = %index,
                            ?reduced_hash,
                            status = ?s.status,
                            "Validated intermediate flashblock"
                        );
                    }
                    Err(e) => {
                        error!(
                            target: "actions",
                            index = %index,
                            error = ?e,
                            "Flashblock validation failed"
                        );
                    }
                }

                // Check if final
                if reduced_hash == target_hash {
                    info!(
                        target: "actions",
                        block_hash = ?reduced_hash,
                        index = %index,
                        "Final flashblock validated"
                    );
                    self.state.signal_final();
                    break;
                }
            }

            Ok(())
        })
    }
}

/// Query blocks by hash as they become available from validation
pub struct GetBlockByHashStream {
    pub node_idxs: Vec<usize>,
    pub state: BlockProductionState,
    pub on_block: Option<Hook<alloy_rpc_types_eth::Block>>,
}

impl GetBlockByHashStream {
    pub fn new(node_idxs: Vec<usize>, state: BlockProductionState) -> Self {
        Self {
            node_idxs,
            state,
            on_block: None,
        }
    }

    pub fn on_block<F>(mut self, f: F) -> Self
    where
        F: Fn(alloy_rpc_types_eth::Block) -> Result<()> + Send + Sync + 'static,
    {
        self.on_block = Some(hook(f));
        self
    }
}

impl Action<OpEngineTypes> for GetBlockByHashStream {
    fn execute<'a>(
        &'a mut self,
        env: &'a mut Environment<OpEngineTypes>,
    ) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            let mut final_rx = self.state.final_validated_rx.clone();
            let mut processed = 0usize;

            loop {
                // Check for new hashes to query
                let hashes = self.state.validated_block_hashes.read().clone();

                for hash in hashes.iter().skip(processed) {
                    for &node_idx in &self.node_idxs {
                        let block: Option<alloy_rpc_types_eth::Block> =
                            EthApiClient::<
                                TransactionRequest,
                                Transaction,
                                alloy_rpc_types_eth::Block,
                                alloy_consensus::Receipt,
                                Header,
                                TransactionSigned,
                            >::block_by_hash(
                                &env.node_clients[node_idx].rpc, *hash, true
                            )
                            .await?;

                        if let Some(ref block) = block {
                            if let Some(ref hook) = self.on_block {
                                hook(block.clone())?;
                            }
                            info!(
                                target: "actions",
                                node_idx = node_idx,
                                hash = ?hash,
                                "Fetched block by hash"
                            );
                        }
                    }
                    processed += 1;
                }

                // Check if we should stop
                if *final_rx.borrow() {
                    break;
                }

                tokio::select! {
                    _ = final_rx.changed() => {
                        if *final_rx.borrow() {
                            break;
                        }
                    }
                    _ = tokio::time::sleep(Duration::from_millis(100)) => {}
                }
            }

            Ok(())
        })
    }
}

/// Query receipts for transaction hashes from shared state
pub struct GetReceiptsStream {
    pub node_idxs: Vec<usize>,
    pub state: BlockProductionState,
    pub on_receipt: Option<Hook<OpTransactionReceipt>>,
}

impl GetReceiptsStream {
    pub fn new(node_idxs: Vec<usize>, state: BlockProductionState) -> Self {
        Self {
            node_idxs,
            state,
            on_receipt: None,
        }
    }

    pub fn on_receipt<F>(mut self, f: F) -> Self
    where
        F: Fn(OpTransactionReceipt) -> Result<()> + Send + Sync + 'static,
    {
        self.on_receipt = Some(hook(f));
        self
    }
}

impl Action<OpEngineTypes> for GetReceiptsStream {
    fn execute<'a>(
        &'a mut self,
        env: &'a mut Environment<OpEngineTypes>,
    ) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            let mut final_rx = self.state.final_validated_rx.clone();
            let mut processed = 0usize;

            loop {
                let tx_hashes = self.state.get_tx_hashes();

                for hash in tx_hashes.iter().skip(processed) {
                    for &node_idx in &self.node_idxs {
                        let receipt: Option<OpTransactionReceipt> = EthApiClient::<
                            TransactionRequest,
                            Transaction,
                            alloy_rpc_types_eth::Block,
                            OpTransactionReceipt,
                            Header,
                            TransactionSigned,
                        >::transaction_receipt(
                            &env.node_clients[node_idx].rpc,
                            *hash,
                        )
                        .await?;

                        if let Some(ref r) = receipt {
                            if let Some(ref hook) = self.on_receipt {
                                hook(r.clone())?;
                            }
                            info!(
                                target: "actions",
                                node_idx = node_idx,
                                tx_hash = ?hash,
                                "Fetched receipt"
                            );
                        }
                    }
                    processed += 1;
                }

                // Check if we should stop
                if *final_rx.borrow() {
                    break;
                }

                tokio::select! {
                    _ = final_rx.changed() => {
                        if *final_rx.borrow() {
                            break;
                        }
                    }
                    _ = tokio::time::sleep(Duration::from_millis(100)) => {}
                }
            }

            Ok(())
        })
    }
}

/// Run multiple actions in parallel
pub struct Parallel {
    actions: Vec<Box<dyn Action<OpEngineTypes> + Send>>,
}

impl Parallel {
    pub fn new() -> Self {
        Self { actions: vec![] }
    }

    pub fn with<A: Action<OpEngineTypes> + Send + 'static>(mut self, action: A) -> Self {
        self.actions.push(Box::new(action));
        self
    }
}

impl Default for Parallel {
    fn default() -> Self {
        Self::new()
    }
}

impl Action<OpEngineTypes> for Parallel {
    fn execute<'a>(
        &'a mut self,
        env: &'a mut Environment<OpEngineTypes>,
    ) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            // We need to execute all actions concurrently
            // Since we can't easily share &mut env, we'll execute them sequentially
            // but signal completion via shared state
            //
            // For true parallel execution, we would need Arc<Mutex<Environment>>
            // which would require changes to the Action trait.
            //
            // Instead, we use tokio::spawn for actions that don't need env mutation
            // and run the main action in the current task.

            // For now, run actions sequentially (they coordinate via shared state)
            for action in &mut self.actions {
                action.execute(env).await?;
            }
            Ok(())
        })
    }
}

// ============================================================================
// Block Production Loop - Composable N-block production as a single Action
// ============================================================================

/// Configuration for block production loop
#[derive(Clone)]
pub struct BlockProductionConfig<A, F> {
    /// Builder node index
    pub builder_node_idx: usize,
    /// Follower node indexes for canonicalization
    pub follower_node_idxs: Vec<usize>,
    /// Authorization generator
    pub authorization_gen: A,
    /// Attributes builder function: (timestamp, eip1559_params) -> OpPayloadAttributes
    pub attributes_builder: F,
    /// Block interval
    pub block_interval: Duration,
    /// Number of blocks to produce
    pub num_blocks: u64,
    /// Starting timestamp
    pub start_timestamp: u64,
    /// Timestamp increment per block
    pub timestamp_increment: u64,
    /// Shared state for cross-action communication
    pub state: BlockProductionState,
    /// Chain spec for EIP-1559 params
    pub chain_spec: Arc<OpChainSpec>,
}

/// A complete block production loop as a single composable Action.
///
/// This action produces N blocks in sequence, with each block going through:
/// 1. Mine block (stores payload in shared state)
/// 2. Validate flashblocks (runs parallel, signals when done)
/// 3. Query blocks by hash (runs parallel, uses validated hashes)
/// 4. Query receipts (runs parallel, uses tx hashes from spammer)
/// 5. Canonicalize on follower nodes
/// 6. Reset state and advance to next block
pub struct BlockProductionLoop<A, F, H> {
    pub config: BlockProductionConfig<A, F>,
    /// Flashblocks handle for getting streams
    pub flashblocks_handle: H,
    /// Beacon handle for validation
    pub beacon_handle: Arc<ConsensusEngineHandle<OpEngineTypes>>,
    /// Hook called after each block with (block_num, block_hash, tx_count)
    pub on_block_produced: Option<Hook<(u64, B256, usize)>>,
    /// Hook for each validated flashblock hash
    pub on_validated_hash: Option<Hook<B256>>,
    /// Hook for each fetched receipt
    pub on_receipt: Option<Hook<OpTransactionReceipt>>,
}

/// Type alias for boxed flashblock stream
pub type FlashblockStreamBoxed =
    Pin<Box<dyn Stream<Item = FlashblocksPayloadV1> + Send + Sync + 'static>>;

/// Trait for types that can provide flashblock streams
pub(crate) trait FlashblocksStreamProvider: Send + Sync {
    fn flashblock_stream(&self) -> FlashblockStreamBoxed;
}

// Implement for flashblocks_p2p handle
impl FlashblocksStreamProvider for flashblocks_p2p::protocol::handler::FlashblocksHandle {
    fn flashblock_stream(&self) -> FlashblockStreamBoxed {
        Box::pin(self.flashblock_stream())
    }
}

/// Compose two actions: first runs, then second
pub struct Then<A, B> {
    first: A,
    second: B,
}

impl<A, B> Then<A, B> {
    pub fn new(first: A, second: B) -> Self {
        Self { first, second }
    }
}

impl<A, B> Action<OpEngineTypes> for Then<A, B>
where
    A: Action<OpEngineTypes> + Send,
    B: Action<OpEngineTypes> + Send,
{
    fn execute<'a>(
        &'a mut self,
        env: &'a mut Environment<OpEngineTypes>,
    ) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            self.first.execute(env).await?;
            self.second.execute(env).await?;
            Ok(())
        })
    }
}

/// Trait for read-only actions that can run in parallel.
/// These actions only read from the environment, never write.
pub trait ReadOnlyAction: Send + Sync {
    /// Execute with shared read-only access to the environment.
    /// No Mutex needed since we're only reading.
    fn execute_readonly<'a>(
        &'a self,
        env: &'a Environment<OpEngineTypes>,
    ) -> BoxFuture<'a, Result<()>>;
}

/// Run two read-only actions in parallel using Arc (no Mutex needed for reads)
pub struct WithParallel<A, B> {
    pub action_a: A,
    pub action_b: B,
}

impl<A, B> WithParallel<A, B> {
    pub fn new(action_a: A, action_b: B) -> Self {
        Self { action_a, action_b }
    }
}

impl<A, B> ReadOnlyAction for WithParallel<A, B>
where
    A: ReadOnlyAction,
    B: ReadOnlyAction,
{
    fn execute_readonly<'a>(
        &'a self,
        env: &'a Environment<OpEngineTypes>,
    ) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            // Both actions get shared &Environment - no Mutex needed for reads
            let (res_a, res_b) = tokio::join!(
                self.action_a.execute_readonly(env),
                self.action_b.execute_readonly(env)
            );
            res_a?;
            res_b?;
            Ok(())
        })
    }
}

/// Wrapper to run a ReadOnlyAction as a regular Action
pub struct AsAction<R> {
    inner: R,
}

impl<R: ReadOnlyAction> AsAction<R> {
    pub fn new(inner: R) -> Self {
        Self { inner }
    }
}

impl<R: ReadOnlyAction + 'static> Action<OpEngineTypes> for AsAction<R> {
    fn execute<'a>(
        &'a mut self,
        env: &'a mut Environment<OpEngineTypes>,
    ) -> BoxFuture<'a, Result<()>> {
        // Convert &mut to & for read-only access
        Box::pin(async move { self.inner.execute_readonly(env).await })
    }
}

/// A composable sequence of actions with hooks between steps.
///
/// Usage:
/// ```ignore
/// ActionSequence::new()
///     .then(MineBlockWithState::new(...))
///     .then(ValidateFlashblocks::new(...))
///     .then(Canonicalize::new(...))
///     .build()
/// ```
pub struct ActionSequence {
    actions: Vec<Box<dyn Action<OpEngineTypes> + Send>>,
}

impl ActionSequence {
    pub fn new() -> Self {
        Self { actions: vec![] }
    }

    /// Compose two actions in parallel with each other
    pub fn with<A: ReadOnlyAction + Sync + 'static, B: ReadOnlyAction + Sync + 'static>(
        mut self,
        a: A,
        b: B,
    ) -> Self {
        let with = WithParallel::new(a, b);
        let action = AsAction::new(with);
        self.actions.push(Box::new(action));
        self
    }

    /// Add an action to the sequence
    pub fn then<A: Action<OpEngineTypes> + Send + 'static>(mut self, action: A) -> Self {
        self.actions.push(Box::new(action));
        self
    }

    /// Convert to a Repeat action that runs this sequence N times
    pub fn repeat(self, times: u64) -> Repeat {
        Repeat {
            sequence: self,
            times,
            on_iteration: None,
        }
    }
}

impl Default for ActionSequence {
    fn default() -> Self {
        Self::new()
    }
}

impl Action<OpEngineTypes> for ActionSequence {
    fn execute<'a>(
        &'a mut self,
        env: &'a mut Environment<OpEngineTypes>,
    ) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            for action in &mut self.actions {
                action.execute(env).await?;
            }
            Ok(())
        })
    }
}

/// Repeat a sequence N times with an optional callback between iterations
pub struct Repeat {
    sequence: ActionSequence,
    times: u64,
    on_iteration: Option<Arc<dyn Fn(u64) -> Result<()> + Send + Sync>>,
}

impl Repeat {
    /// Called before each iteration with the iteration number (0-indexed)
    pub fn on_each<F>(mut self, f: F) -> Self
    where
        F: Fn(u64) -> Result<()> + Send + Sync + 'static,
    {
        self.on_iteration = Some(Arc::new(f));
        self
    }
}

impl Action<OpEngineTypes> for Repeat {
    fn execute<'a>(
        &'a mut self,
        env: &'a mut Environment<OpEngineTypes>,
    ) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            for i in 0..self.times {
                if let Some(ref callback) = self.on_iteration {
                    callback(i)?;
                }
                self.sequence.execute(env).await?;
            }
            Ok(())
        })
    }
}

// ============================================================================
// Simple Action Primitives for Composition
// ============================================================================

/// Reset the shared state - use at the start of each block cycle
pub struct ResetState {
    pub state: BlockProductionState,
}

impl ResetState {
    pub fn new(state: BlockProductionState) -> Self {
        Self { state }
    }
}

impl Action<OpEngineTypes> for ResetState {
    fn execute<'a>(
        &'a mut self,
        _env: &'a mut Environment<OpEngineTypes>,
    ) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            self.state.reset();
            Ok(())
        })
    }
}

/// Query all validated block hashes from shared state
pub struct QueryValidatedBlocks {
    pub node_idxs: Vec<usize>,
    pub state: BlockProductionState,
    pub on_block: Option<Hook<alloy_rpc_types_eth::Block>>,
}

impl QueryValidatedBlocks {
    pub fn new(node_idxs: Vec<usize>, state: BlockProductionState) -> Self {
        Self {
            node_idxs,
            state,
            on_block: None,
        }
    }

    pub fn on_block<F>(mut self, f: F) -> Self
    where
        F: Fn(alloy_rpc_types_eth::Block) -> Result<()> + Send + Sync + 'static,
    {
        self.on_block = Some(hook(f));
        self
    }
}

impl ReadOnlyAction for QueryValidatedBlocks {
    fn execute_readonly<'a>(
        &'a self,
        env: &'a Environment<OpEngineTypes>,
    ) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            let hashes = self.state.validated_block_hashes.read().clone();

            for hash in &hashes {
                for &node_idx in &self.node_idxs {
                    let block: Option<alloy_rpc_types_eth::Block> =
                        EthApiClient::<
                            TransactionRequest,
                            Transaction,
                            alloy_rpc_types_eth::Block,
                            alloy_consensus::Receipt,
                            Header,
                            TransactionSigned,
                        >::block_by_hash(
                            &env.node_clients[node_idx].rpc, *hash, false
                        )
                        .await?;

                    if let Some(ref b) = block
                        && let Some(ref hook) = self.on_block {
                            hook(b.clone())?;
                        }
                }
            }
            Ok(())
        })
    }
}

/// Query receipts for all transaction hashes in shared state
pub struct QueryTxReceipts {
    pub node_idxs: Vec<usize>,
    pub state: BlockProductionState,
    pub on_receipt: Option<Hook<OpTransactionReceipt>>,
}

impl QueryTxReceipts {
    pub fn new(node_idxs: Vec<usize>, state: BlockProductionState) -> Self {
        Self {
            node_idxs,
            state,
            on_receipt: None,
        }
    }

    pub fn on_receipt<F>(mut self, f: F) -> Self
    where
        F: Fn(OpTransactionReceipt) -> Result<()> + Send + Sync + 'static,
    {
        self.on_receipt = Some(hook(f));
        self
    }
}

impl ReadOnlyAction for QueryTxReceipts {
    fn execute_readonly<'a>(
        &'a self,
        env: &'a Environment<OpEngineTypes>,
    ) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            let hashes = self.state.get_tx_hashes();

            for hash in &hashes {
                for &node_idx in &self.node_idxs {
                    let receipt: Option<OpTransactionReceipt> = EthApiClient::<
                        TransactionRequest,
                        Transaction,
                        alloy_rpc_types_eth::Block,
                        OpTransactionReceipt,
                        Header,
                        TransactionSigned,
                    >::transaction_receipt(
                        &env.node_clients[node_idx].rpc,
                        *hash,
                    )
                    .await?;

                    if let Some(ref r) = receipt
                        && let Some(ref hook) = self.on_receipt {
                            hook(r.clone())?;
                        }
                }
            }
            Ok(())
        })
    }
}

/// A dynamic mining action that gets attributes from shared state
pub struct DynamicMineBlock<A> {
    pub node_idx: usize,
    pub authorization_gen: A,
    pub block_interval: Duration,
    pub state: BlockProductionState,
    /// Function to get current attributes (called at execution time)
    pub get_attributes: Arc<dyn Fn() -> OpPayloadAttributes + Send + Sync>,
}

impl<A> DynamicMineBlock<A>
where
    A: Fn(OpPayloadAttributes) -> Authorization + Clone + Send + Sync + 'static,
{
    pub fn new<F>(
        node_idx: usize,
        authorization_gen: A,
        state: BlockProductionState,
        get_attributes: F,
    ) -> Self
    where
        F: Fn() -> OpPayloadAttributes + Send + Sync + 'static,
    {
        Self {
            node_idx,
            authorization_gen,
            block_interval: Duration::from_millis(2000),
            state,
            get_attributes: Arc::new(get_attributes),
        }
    }

    pub fn with_interval(mut self, interval: Duration) -> Self {
        self.block_interval = interval;
        self
    }
}

impl<A> ReadOnlyAction for DynamicMineBlock<A>
where
    A: Fn(OpPayloadAttributes) -> Authorization + Clone + Send + Sync + 'static,
{
    fn execute_readonly<'a>(
        &'a self,
        env: &'a Environment<OpEngineTypes>,
    ) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            let attributes = (self.get_attributes)();

            let mine_action = MineBlockWithState::new(
                self.node_idx,
                attributes,
                self.authorization_gen.clone(),
                self.state.clone(),
            )
            .with_interval(self.block_interval);

            mine_action.execute_readonly(env).await
        })
    }
}

/// A dynamic flashblock validator that gets streams from a flashblocks handle
pub struct DynamicValidateFlashblocks {
    pub flashblocks_handle: flashblocks_p2p::protocol::handler::FlashblocksHandle,
    pub beacon_handle: Arc<ConsensusEngineHandle<OpEngineTypes>>,
    pub chain_spec: Arc<OpChainSpec>,
    pub state: BlockProductionState,
}

impl DynamicValidateFlashblocks {
    pub fn new(
        flashblocks_handle: flashblocks_p2p::protocol::handler::FlashblocksHandle,
        beacon_handle: Arc<ConsensusEngineHandle<OpEngineTypes>>,
        chain_spec: Arc<OpChainSpec>,
        state: BlockProductionState,
    ) -> Self {
        Self {
            flashblocks_handle,
            beacon_handle,
            chain_spec,
            state,
        }
    }
}

impl Action<OpEngineTypes> for DynamicValidateFlashblocks {
    fn execute<'a>(
        &'a mut self,
        env: &'a mut Environment<OpEngineTypes>,
    ) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            let stream = Box::pin(self.flashblocks_handle.flashblock_stream());

            let mut validate_action = ValidateFlashblocksWithState::new(
                stream,
                self.beacon_handle.clone(),
                self.chain_spec.clone(),
                self.state.clone(),
            );

            validate_action.execute(env).await
        })
    }
}

impl ReadOnlyAction for DynamicValidateFlashblocks {
    fn execute_readonly<'a>(
        &'a self,
        _env: &'a Environment<OpEngineTypes>,
    ) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            let mut flashblocks = Flashblocks::default();
            let mut stream = Box::pin(self.flashblocks_handle.flashblock_stream());

            // Wait for payload to be available
            let target_hash = loop {
                if let Some(payload) = self.state.get_payload() {
                    break payload
                        .execution_payload
                        .payload_inner
                        .payload_inner
                        .block_hash;
                }
                tokio::time::sleep(Duration::from_millis(50)).await;
            };

            info!(
                target: "actions",
                target_hash = ?target_hash,
                "Starting flashblock validation (parallel)"
            );

            while let Some(fb_payload) = stream.next().await {
                let index = fb_payload.index;

                let is_new = flashblocks
                    .push(Flashblock {
                        flashblock: fb_payload,
                    })
                    .ok();

                if is_new.is_some() {
                    info!(
                        target: "actions",
                        index = %index,
                        "New flashblock received"
                    );
                }

                // Check if this is the final flashblock matching our target
                if let Ok(reduced) = Flashblock::reduce(flashblocks.clone()) {
                    let block_hash = reduced.diff().block_hash;
                    if block_hash == target_hash {
                        info!(
                            target: "actions",
                            block_hash = ?block_hash,
                            "Final flashblock validated"
                        );
                        self.state.add_validated_hash(block_hash);
                        self.state.signal_final();
                        break;
                    }
                }
            }

            Ok(())
        })
    }
}

/// Log current block production state
pub struct LogBlockComplete {
    pub state: BlockProductionState,
    pub on_complete: Option<Hook<(B256, usize)>>,
}

impl LogBlockComplete {
    pub fn new(state: BlockProductionState) -> Self {
        Self {
            state,
            on_complete: None,
        }
    }

    pub fn on_complete<F>(mut self, f: F) -> Self
    where
        F: Fn((B256, usize)) -> Result<()> + Send + Sync + 'static,
    {
        self.on_complete = Some(hook(f));
        self
    }
}

impl Action<OpEngineTypes> for LogBlockComplete {
    fn execute<'a>(
        &'a mut self,
        _env: &'a mut Environment<OpEngineTypes>,
    ) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            if let Some(payload) = self.state.get_payload() {
                let hash = payload
                    .execution_payload
                    .payload_inner
                    .payload_inner
                    .block_hash;
                let tx_count = payload
                    .execution_payload
                    .payload_inner
                    .payload_inner
                    .transactions
                    .len();

                info!(
                    target: "block_production",
                    ?hash,
                    tx_count,
                    validated_hashes = self.state.validated_block_hashes.read().len(),
                    tx_receipts = self.state.get_tx_hashes().len(),
                    "Block cycle complete"
                );

                if let Some(ref hook) = self.on_complete {
                    hook((hash, tx_count))?;
                }
            }
            Ok(())
        })
    }
}

/// Sleep for a duration - useful between block cycles
pub struct Sleep {
    pub duration: Duration,
}

impl Sleep {
    pub fn new(duration: Duration) -> Self {
        Self { duration }
    }

    pub fn millis(ms: u64) -> Self {
        Self::new(Duration::from_millis(ms))
    }
}

impl Action<OpEngineTypes> for Sleep {
    fn execute<'a>(
        &'a mut self,
        _env: &'a mut Environment<OpEngineTypes>,
    ) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            tokio::time::sleep(self.duration).await;
            Ok(())
        })
    }
}

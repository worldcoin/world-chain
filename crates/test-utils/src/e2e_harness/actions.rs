#![allow(dead_code)]

use alloy_consensus::Header;
use alloy_eips::{BlockId, Decodable2718};
use alloy_rpc_types::{Transaction, TransactionRequest};
use alloy_rpc_types_engine::{ForkchoiceState, PayloadStatusEnum};
use eyre::eyre::{Result, eyre};
use futures::{
    Stream, StreamExt, TryStreamExt,
    future::BoxFuture,
    stream::{self, FuturesUnordered},
};
use op_alloy_rpc_types::OpTransactionReceipt;
use op_alloy_rpc_types_engine::OpExecutionPayloadEnvelopeV4;
use reth_e2e_test_utils::testsuite::{Environment, actions::Action};
use reth_node_api::ConsensusEngineHandle;
use world_chain_primitives::OpChainSpec;
use world_chain_primitives::OpEngineTypes;
use world_chain_primitives::OpPayloadAttrs;
use world_chain_primitives::OpTransactionSigned;
use reth_rpc_api::{EngineApiClient, EthApiClient};
use revm_primitives::{Address, B256, Bytes, U256};
use std::{pin::Pin, sync::Arc, time::Duration};
use tokio::sync::mpsc;
use tracing::{error, info};
use world_chain_primitives::{
    flashblocks::{Flashblock, Flashblocks},
    p2p::Authorization,
    primitives::FlashblocksPayloadV1,
};
use world_chain_rpc::{engine::FlashblocksEngineApiExtClient, op::OpApiExtClient};

use super::setup::execution_data_from_from_reduced_flashblock;

// ---------------------------------------------------------------------------
// Test helper macros for Eth API queries
// ---------------------------------------------------------------------------

/// Create an `alloy_provider::RootProvider` from a node's RPC URL.
///
/// ```ignore
/// let provider = provider!(nodes[0]);
/// ```
#[macro_export]
macro_rules! provider {
    ($node:expr) => {{
        let url = $node.node.rpc_url();
        alloy_provider::ProviderBuilder::new().connect_http(url)
    }};
}

/// Fetch a block by tag (`Pending`, `Latest`, etc.) from a node.
///
/// ```ignore
/// let block = fetch_block!(nodes[0], Pending);
/// let block = fetch_block!(nodes[0], Latest, true); // full txs
/// ```
#[macro_export]
macro_rules! fetch_block {
    ($node:expr, $tag:ident) => {{
        let provider = $crate::provider!($node);
        alloy_provider::Provider::get_block_by_number(
            &provider,
            alloy_eips::BlockNumberOrTag::$tag,
            false,
        )
        .await
    }};
    ($node:expr, $tag:ident, $full_txs:expr) => {{
        let provider = $crate::provider!($node);
        alloy_provider::Provider::get_block_by_number(
            &provider,
            alloy_eips::BlockNumberOrTag::$tag,
            $full_txs,
        )
        .await
    }};
}

/// Fetch a transaction receipt by hash.
///
/// ```ignore
/// let receipt = fetch_receipt!(nodes[0], tx_hash);
/// ```
#[macro_export]
macro_rules! fetch_receipt {
    ($node:expr, $tx_hash:expr) => {{
        let provider = $crate::provider!($node);
        alloy_provider::Provider::get_transaction_receipt(&provider, $tx_hash).await
    }};
}

/// Fetch a transaction by hash.
///
/// ```ignore
/// let tx = fetch_tx!(nodes[0], tx_hash);
/// ```
#[macro_export]
macro_rules! fetch_tx {
    ($node:expr, $tx_hash:expr) => {{
        let provider = $crate::provider!($node);
        alloy_provider::Provider::get_transaction_by_hash(&provider, $tx_hash).await
    }};
}

/// Perform an `eth_call` against a node.
///
/// ```ignore
/// let result = eth_call!(nodes[0], tx_request);
/// let result = eth_call!(nodes[0], tx_request, Pending);
/// ```
#[macro_export]
macro_rules! eth_call {
    ($node:expr, $tx:expr) => {{
        let provider = $crate::provider!($node);
        alloy_provider::Provider::call(&provider, &$tx).await
    }};
    ($node:expr, $tx:expr, $tag:ident) => {{
        let provider = $crate::provider!($node);
        alloy_provider::Provider::call(&provider, &$tx)
            .block(alloy_eips::BlockId::Number(
                alloy_eips::BlockNumberOrTag::$tag,
            ))
            .await
    }};
}

/// Fetch logs matching a filter.
///
/// ```ignore
/// let logs = fetch_logs!(nodes[0], filter);
/// ```
#[macro_export]
macro_rules! fetch_logs {
    ($node:expr, $filter:expr) => {{
        let provider = $crate::provider!($node);
        alloy_provider::Provider::get_logs(&provider, &$filter).await
    }};
}

/// Subscribe to new block headers (uses polling via `watch_blocks`).
///
/// ```ignore
/// let poller = stream_blocks!(nodes[0]);
/// ```
#[macro_export]
macro_rules! stream_blocks {
    ($node:expr) => {{
        let provider = $crate::provider!($node);
        alloy_provider::Provider::watch_blocks(&provider).await
    }};
}

pub type Hook<T> = Arc<dyn Fn(T) -> Result<()> + Send + Sync>;

pub fn hook<T, F>(f: F) -> Hook<T>
where
    F: Fn(T) -> Result<()> + Send + Sync + 'static,
{
    Arc::new(f)
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

pub struct FlashblocksValidatonStream {
    pub beacon_engine_handles: Vec<Arc<ConsensusEngineHandle<OpEngineTypes>>>,
    pub flashblocks_stream:
        Pin<Box<dyn Stream<Item = FlashblocksPayloadV1> + Unpin + Send + Sync + 'static>>,
    pub validation_hook: Option<Hook<PayloadStatusEnum>>,
    pub chain_spec: Arc<OpChainSpec>,
}

impl Action<OpEngineTypes> for FlashblocksValidatonStream {
    fn execute<'a>(
        &'a mut self,
        _env: &'a mut Environment<OpEngineTypes>,
    ) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            let chain_spec = self.chain_spec.clone();
            let flashblocks: Flashblocks = Flashblocks::default();

            let mut ordered = flashblocks;

            // Process flashblocks from the stream
            while let Some(flashblock) = self.flashblocks_stream.next().await {
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

                let status_results = self
                    .beacon_engine_handles
                    .clone()
                    .into_iter()
                    .map(|beacon_handle| {
                        let execution_data = execution_data.clone();
                        async move {
                            beacon_handle.fork_choice_updated(forkchoice, None).await?;

                            let status = beacon_handle
                                .new_payload(execution_data.clone().into())
                                .await?;

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
                            && let Err(e) = hook(status.clone())
                        {
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
                    let txs = payload.diff.transactions.to_vec();

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
                                OpTransactionSigned,
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
                    OpTransactionSigned,
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
                            OpTransactionSigned,
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
                    OpTransactionSigned,
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
                    OpTransactionSigned,
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
                    OpTransactionSigned,
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
    pub attributes: OpPayloadAttrs,
    pub authorization_gen: A,
    pub block_interval: Duration,
    pub flashblocks: bool,
    pub sender: mpsc::Sender<OpExecutionPayloadEnvelopeV4>,
}

impl<A> AssertMineBlock<A>
where
    A: Fn(OpPayloadAttrs) -> Authorization + Clone + Send + Sync,
{
    pub async fn new(
        node_idx: usize,
        parent_hash: Option<B256>,
        attributes: OpPayloadAttrs,
        authorization_gen: A,
        block_interval: Duration,
        flashblocks: bool,
        sender: mpsc::Sender<OpExecutionPayloadEnvelopeV4>,
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
    A: Fn(OpPayloadAttrs) -> Authorization + Clone + Send + Sync + 'static,
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
                        OpTransactionSigned,
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

            tokio::time::sleep(self.block_interval).await;
            let payload =
                EngineApiClient::<OpEngineTypes>::get_payload_v4(&engine, payload_id).await?;

            info!(
                "Mined block {} with {} txs",
                payload
                    .execution_payload
                    .payload_inner
                    .payload_inner
                    .payload_inner
                    .block_hash,
                payload
                    .execution_payload
                    .payload_inner
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

// ---------------------------------------------------------------------------
// EngineDriver — drives the consensus engine through N block-building cycles
// ---------------------------------------------------------------------------

/// Callback invoked after each block is built and canonicalized.
pub type BlockCallback = Box<
    dyn Fn(
            usize,
            &OpExecutionPayloadEnvelopeV4,
        ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<()>> + Send>>
        + Send
        + Sync,
>;

/// Callback invoked during the build interval (no payload available yet).
pub type MidBuildCallback = Box<
    dyn Fn(usize) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<()>> + Send>>
        + Send
        + Sync,
>;

/// Drives the consensus engine through `num_blocks` block-building cycles.
///
/// Each cycle:
/// 1. Generates payload attributes for the next block
/// 2. Sends `forkchoiceUpdatedV3` with attributes to start building
/// 3. Waits for `block_interval` (the build deadline)
/// 4. Calls `getPayloadV4` to retrieve the built payload
/// 5. Sends `newPayloadV4` + `forkchoiceUpdated` on all follower nodes
/// 6. Invokes the optional `on_block` callback
/// 7. Advances to the next cycle with the new block as head
pub struct EngineDriver<A> {
    /// Index of the builder node in the environment's node_clients.
    pub builder_idx: usize,
    /// Indices of follower nodes that receive `newPayload` + FCU.
    pub follower_idxs: Vec<usize>,
    /// Initial parent hash (genesis). If None, fetched from latest block.
    pub initial_parent_hash: Option<B256>,
    /// Number of blocks to build.
    pub num_blocks: usize,
    /// Time to wait between FCU (start building) and getPayload (retrieve).
    pub block_interval: Duration,
    /// Whether to use flashblocks FCU with authorization.
    pub flashblocks: bool,
    /// Generates `Authorization` from `(parent_hash, OpPayloadAttributes)`.
    pub authorization_gen: A,
    /// Generates attributes for the next block given (block_number, parent_timestamp).
    pub attributes_gen: Box<dyn Fn(u64, u64) -> Result<OpPayloadAttrs> + Send + Sync>,
    /// Optional callback during the build interval (between FCU and getPayload).
    /// Called while the payload builder is actively working.
    pub during_build: Option<MidBuildCallback>,
    /// Optional callback after each block is built and canonicalized.
    pub on_block: Option<BlockCallback>,
}

impl<A> EngineDriver<A>
where
    A: Fn(B256, OpPayloadAttrs) -> Authorization + Clone + Send + Sync + 'static,
{
    pub fn execute<'a>(
        &'a mut self,
        env: &'a mut Environment<OpEngineTypes>,
    ) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            let builder = &env.node_clients[self.builder_idx];
            let engine = builder.engine.http_client();

            // Get the initial head
            let mut parent_hash = if let Some(hash) = self.initial_parent_hash {
                hash
            } else {
                let latest: Option<alloy_rpc_types_eth::Block> =
                    EthApiClient::<
                        TransactionRequest,
                        Transaction,
                        alloy_rpc_types_eth::Block,
                        alloy_consensus::Receipt,
                        Header,
                        OpTransactionSigned,
                    >::block_by_number(
                        &builder.rpc, alloy_eips::BlockNumberOrTag::Latest, false
                    )
                    .await?;
                latest
                    .ok_or_else(|| eyre!("No latest block"))?
                    .header
                    .hash_slow()
            };

            let mut parent_timestamp = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_secs();

            for block_num in 0..self.num_blocks {
                // 1. Generate attributes
                let block_number = block_num as u64 + 1;
                parent_timestamp += self.block_interval.as_secs().max(1);
                let attributes = (self.attributes_gen)(block_number, parent_timestamp)?;

                // 2. FCU with attributes → start building
                let fcu_state = ForkchoiceState {
                    head_block_hash: parent_hash,
                    safe_block_hash: parent_hash,
                    finalized_block_hash: parent_hash,
                };

                let fcu_result = if self.flashblocks {
                    FlashblocksEngineApiExtClient::<OpEngineTypes>::flashblocks_fork_choice_updated_v3(
                        &engine,
                        fcu_state,
                        Some(attributes.clone()),
                        Some((self.authorization_gen)(parent_hash, attributes.clone())),
                    )
                    .await?
                } else {
                    EngineApiClient::<OpEngineTypes>::fork_choice_updated_v3(
                        &engine,
                        fcu_state,
                        Some(attributes.clone()),
                    )
                    .await?
                };

                if !matches!(fcu_result.payload_status.status, PayloadStatusEnum::Valid) {
                    return Err(eyre!(
                        "block {block_num}: FCU status not valid: {:?}",
                        fcu_result.payload_status
                    ));
                }

                let payload_id = fcu_result
                    .payload_id
                    .ok_or_else(|| eyre!("block {block_num}: No payload ID returned"))?;

                info!(
                    target: "engine_driver",
                    block = block_num,
                    %payload_id,
                    "building block"
                );

                // 3. Wait for build deadline
                tokio::time::sleep(self.block_interval).await;

                // 3.5. Mid-build callback (payload builder is still working)
                if let Some(ref during_build) = self.during_build {
                    during_build(block_num).await?;
                }

                // 4. getPayloadV4
                let payload =
                    EngineApiClient::<OpEngineTypes>::get_payload_v4(&engine, payload_id).await?;

                let block_hash = payload
                    .execution_payload
                    .payload_inner
                    .payload_inner
                    .payload_inner
                    .block_hash;

                let tx_count = payload
                    .execution_payload
                    .payload_inner
                    .payload_inner
                    .payload_inner
                    .transactions
                    .len();

                info!(
                    target: "engine_driver",
                    block = block_num,
                    %block_hash,
                    tx_count,
                    "payload retrieved"
                );

                // 5. Canonicalize: FCU(parent) → newPayload → FCU(head)
                //    on builder AND all follower nodes
                use alloy_rpc_types_engine::CancunPayloadFields;
                use op_alloy_rpc_types_engine::{
                    OpExecutionData, OpExecutionPayload, OpExecutionPayloadSidecar,
                };

                // Canonicalize on builder via FCU only (it already has the payload)
                {
                    let builder_engine = env.node_clients[self.builder_idx].engine.http_client();
                    let head_fcu = ForkchoiceState {
                        head_block_hash: block_hash,
                        safe_block_hash: block_hash,
                        finalized_block_hash: block_hash,
                    };
                    let fcu_result = EngineApiClient::<OpEngineTypes>::fork_choice_updated_v3(
                        &builder_engine,
                        head_fcu,
                        None,
                    )
                    .await?;

                    if !matches!(fcu_result.payload_status.status, PayloadStatusEnum::Valid) {
                        return Err(eyre!(
                            "block {block_num}: builder FCU to head failed: {:?}",
                            fcu_result.payload_status
                        ));
                    }
                }

                // Canonicalize on follower nodes: FCU(parent) → newPayload → FCU(head)
                for follower_idx in self.follower_idxs.iter().copied() {
                    if let Some(beacon_handle) =
                        env.node_clients[follower_idx].beacon_engine_handle.as_ref()
                    {
                        // FCU to parent
                        let parent_fcu = ForkchoiceState {
                            head_block_hash: parent_hash,
                            safe_block_hash: parent_hash,
                            finalized_block_hash: parent_hash,
                        };
                        beacon_handle
                            .fork_choice_updated(
                                parent_fcu,
                                None,
                            )
                            .await
                            .map_err(|e| {
                                eyre!("block {block_num}: FCU to parent failed on follower {follower_idx}: {e:?}")
                            })?;

                        // newPayload
                        let execution_data = OpExecutionData {
                            payload: OpExecutionPayload::V4(payload.execution_payload.clone()),
                            sidecar: OpExecutionPayloadSidecar::v4(
                                CancunPayloadFields::new(payload.parent_beacon_block_root, vec![]),
                                alloy_rpc_types_engine::PraguePayloadFields {
                                    requests: alloy_eips::eip7685::RequestsOrHash::Hash(
                                        alloy_eips::eip7685::EMPTY_REQUESTS_HASH,
                                    ),
                                },
                            ),
                        };

                        let status = beacon_handle
                            .new_payload(execution_data.into())
                            .await
                            .map_err(|e| {
                                eyre!("block {block_num}: newPayload failed on follower {follower_idx}: {e:?}")
                            })?;

                        if !matches!(status.status, PayloadStatusEnum::Valid) {
                            return Err(eyre!(
                                "block {block_num}: newPayload invalid on follower {follower_idx}: {:?}",
                                status
                            ));
                        }

                        // FCU to head
                        let head_fcu = ForkchoiceState {
                            head_block_hash: block_hash,
                            safe_block_hash: block_hash,
                            finalized_block_hash: block_hash,
                        };
                        beacon_handle
                            .fork_choice_updated(
                                head_fcu,
                                None,
                            )
                            .await
                            .map_err(|e| {
                                eyre!("block {block_num}: FCU to head failed on follower {follower_idx}: {e:?}")
                            })?;

                        info!(
                            target: "engine_driver",
                            block = block_num,
                            follower = follower_idx,
                            %block_hash,
                            "canonicalized on follower via beacon handle"
                        );
                    } else {
                        return Err(eyre!(
                            "block {block_num}: follower {follower_idx} has no beacon_engine_handle"
                        ));
                    }
                }

                // 6. Invoke callback
                if let Some(ref on_block) = self.on_block {
                    on_block(block_num, &payload).await?;
                }

                // 7. Advance head
                parent_hash = block_hash;

                info!(
                    target: "engine_driver",
                    block = block_num,
                    %block_hash,
                    "block complete"
                );
            }

            Ok(())
        })
    }
}

impl<A> Action<OpEngineTypes> for EngineDriver<A>
where
    A: Fn(B256, OpPayloadAttrs) -> Authorization + Clone + Send + Sync + 'static,
{
    fn execute<'a>(
        &'a mut self,
        env: &'a mut Environment<OpEngineTypes>,
    ) -> BoxFuture<'a, Result<()>> {
        EngineDriver::execute(self, env)
    }
}

// ---------------------------------------------------------------------------
// StreamAssertion — assertion-driven event stream validation
// ---------------------------------------------------------------------------

use world_chain_p2p::protocol::event::{ChainEvent, WorldChainEvent};

/// Pre-computed assertion on a [`WorldChainEvent`] from the event stream.
#[derive(Debug, Clone)]
pub enum StreamAssertion {
    /// Expect a Canon event. Optionally assert the block number.
    Canon { number: Option<u64> },
    /// Expect a Pending flashblock with the given index.
    /// `is_base` asserts whether it should have a `base` field.
    Pending { index: u64, is_base: bool },
    /// Expect the pending block watch channel to contain a block at this number.
    PendingBlockAt { number: u64 },
    /// Expect the pending block watch channel to be None.
    PendingBlockCleared,
}

/// Result of running assertions against the event stream.
#[derive(Debug)]
pub struct StreamAssertionResult {
    /// Number of assertions that passed.
    pub passed: usize,
    /// Number of assertions that were expected but not seen (stream ended or timed out).
    pub missed: usize,
    /// Failures with details.
    pub failures: Vec<String>,
}

impl StreamAssertionResult {
    pub fn assert_all_passed(&self) {
        assert!(
            self.failures.is_empty() && self.missed == 0,
            "Stream assertion failures: {:?}, missed: {}",
            self.failures,
            self.missed,
        );
    }
}

/// Consume events from the stream, checking each against the next expected
/// assertion. Returns when all assertions are satisfied or the timeout fires.
pub async fn assert_stream<S>(
    mut stream: S,
    assertions: Vec<StreamAssertion>,
    timeout: Duration,
) -> StreamAssertionResult
where
    S: futures::Stream<Item = WorldChainEvent<()>> + Unpin + Send,
{
    let mut passed = 0usize;
    let mut failures = Vec::new();
    let mut assertion_iter = assertions.iter().enumerate().peekable();
    let deadline = tokio::time::Instant::now() + timeout;

    loop {
        // All assertions consumed — done.
        if assertion_iter.peek().is_none() {
            break;
        }

        let event = tokio::select! {
            event = futures::StreamExt::next(&mut stream) => {
                match event {
                    Some(e) => e,
                    None => break, // stream ended
                }
            }
            _ = tokio::time::sleep_until(deadline) => {
                break; // timeout
            }
        };

        let Some(&(idx, assertion)) = assertion_iter.peek() else {
            break;
        };

        match (&event, assertion) {
            // Canon matches Canon assertion
            (WorldChainEvent::Chain(ChainEvent::Canon(tip)), StreamAssertion::Canon { number }) => {
                if let Some(expected) = number
                    && tip.number != *expected
                {
                    failures.push(format!(
                        "assertion[{idx}]: expected Canon(number={}), got Canon(number={})",
                        expected, tip.number,
                    ));
                }
                passed += 1;
                assertion_iter.next();
            }
            // Pending matches Pending assertion with correct index
            (
                WorldChainEvent::Chain(ChainEvent::Pending(fb)),
                StreamAssertion::Pending { index, is_base },
            ) if fb.index == *index => {
                if fb.base.is_some() != *is_base {
                    failures.push(format!(
                        "assertion[{idx}]: expected is_base={}, got is_base={}",
                        is_base,
                        fb.base.is_some(),
                    ));
                }
                passed += 1;
                assertion_iter.next();
            }
            // Any non-matching event is skipped — Canon events, delta
            // flashblocks from the current epoch, etc. The assertion
            // checker only advances when an event matches the next
            // expected assertion.
            _ => continue,
        }
    }

    let missed = assertion_iter.count();

    StreamAssertionResult {
        passed,
        missed,
        failures,
    }
}

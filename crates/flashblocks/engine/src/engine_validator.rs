//! Metrics-instrumented wrapper around reth's [`EngineValidator`].
//!
//! Tracks pre-executed block insertions from the flashblocks builder and detects
//! whether reth's engine tree served them as cache hits (skipping full validation
//! on `newPayload`) or whether they were evicted and required re-validation.

use std::{sync::Arc, time::Instant};

use alloy_primitives::B256;
use dashmap::DashSet;
use metrics::{Counter, Histogram};
use metrics_derive::Metrics;
use reth_chain_state::ExecutedBlock;
use reth_engine_primitives::ExecutionPayload;
use reth_engine_tree::tree::payload_validator::{EngineValidator, TreeCtx, ValidationOutcome};
use reth_node_api::{AddOnsContext, FullNodeComponents, NodePrimitives};
use reth_node_builder::rpc::EngineValidatorBuilder;
use reth_payload_primitives::{InvalidPayloadAttributesError, NewPayloadError, PayloadTypes};
use reth_primitives_traits::SealedBlock;
use reth_trie_db::ChangesetCache;
use tracing::trace;

/// Maximum number of pre-executed block hashes to track before pruning.
const MAX_TRACKED_BLOCKS: usize = 64;

/// Metrics for the flashblocks engine validator wrapper.
#[derive(Clone, Metrics)]
#[metrics(scope = "flashblocks.engine_validator")]
pub struct WorldChainEngineValidatorMetrics {
    /// Blocks pre-executed by the flashblocks builder and inserted into the engine tree.
    pub pre_executed_inserts: Counter,
    /// `validate_payload` was called for a block that was previously pre-executed.
    /// This indicates a cache miss — the tree evicted the block before the CL sent `newPayload`.
    pub pre_executed_cache_miss: Counter,
    /// Total number of `validate_payload` calls received.
    pub payload_validations: Counter,
    /// Total number of `validate_block` calls received.
    pub block_validations: Counter,
    /// Wall-clock duration of `validate_payload` calls (seconds).
    pub validate_payload_duration: Histogram,
    /// Wall-clock duration of `validate_block` calls (seconds).
    pub validate_block_duration: Histogram,
}

/// Wrapper around an [`EngineValidator`] that instruments every method with
/// flashblocks-specific metrics.
///
/// The key insight: when the flashblocks builder pre-executes a block and broadcasts
/// it via `Events::BuiltPayload`, reth's engine tree inserts it via
/// `InsertExecutedBlock` → `on_inserted_executed_block`. If the CL later sends
/// `newPayload` for that same block hash, the tree returns `AlreadySeen(Valid)`
/// **without** calling `validate_payload`. So:
///
/// - `pre_executed_inserts` counts how many blocks were pre-inserted.
/// - If `validate_payload` is called for a pre-inserted hash, it's a **cache miss**
///   (the tree evicted the block). This increments `pre_executed_cache_miss`.
/// - Cache hits = `pre_executed_inserts` − `pre_executed_cache_miss`.
pub struct WorldChainEngineValidator<V> {
    inner: V,
    /// Block hashes inserted via `on_inserted_executed_block`.
    pre_executed_blocks: Arc<DashSet<B256>>,
    metrics: WorldChainEngineValidatorMetrics,
}

impl<V> WorldChainEngineValidator<V> {
    /// Wraps an existing engine validator with flashblocks metrics.
    pub fn new(inner: V) -> Self {
        Self {
            inner,
            pre_executed_blocks: Arc::new(DashSet::new()),
            metrics: WorldChainEngineValidatorMetrics::default(),
        }
    }
}

impl<V, Types, N> EngineValidator<Types, N> for WorldChainEngineValidator<V>
where
    V: EngineValidator<Types, N>,
    Types: PayloadTypes<ExecutionData: ExecutionPayload>,
    N: NodePrimitives,
{
    fn validate_payload_attributes_against_header(
        &self,
        attr: &Types::PayloadAttributes,
        header: &N::BlockHeader,
    ) -> Result<(), InvalidPayloadAttributesError> {
        self.inner
            .validate_payload_attributes_against_header(attr, header)
    }

    fn convert_payload_to_block(
        &self,
        payload: Types::ExecutionData,
    ) -> Result<SealedBlock<N::Block>, NewPayloadError> {
        self.inner.convert_payload_to_block(payload)
    }

    fn validate_payload(
        &mut self,
        payload: Types::ExecutionData,
        ctx: TreeCtx<'_, N>,
    ) -> ValidationOutcome<N> {
        let block_hash = payload.block_hash();

        // If this hash was previously pre-inserted by the flashblocks builder,
        // the tree should have returned AlreadySeen before reaching us.
        // If we're here, it means the tree evicted the block — a cache miss.
        if self.pre_executed_blocks.remove(&block_hash).is_some() {
            self.metrics.pre_executed_cache_miss.increment(1);
            trace!(
                target: "flashblocks::engine_validator",
                %block_hash,
                "cache miss: pre-executed block evicted from tree, re-validating"
            );
        }

        self.metrics.payload_validations.increment(1);
        let start = Instant::now();
        let result = self.inner.validate_payload(payload, ctx);
        self.metrics
            .validate_payload_duration
            .record(start.elapsed().as_secs_f64());
        result
    }

    fn validate_block(
        &mut self,
        block: SealedBlock<N::Block>,
        ctx: TreeCtx<'_, N>,
    ) -> ValidationOutcome<N> {
        self.metrics.block_validations.increment(1);
        let start = Instant::now();
        let result = self.inner.validate_block(block, ctx);
        self.metrics
            .validate_block_duration
            .record(start.elapsed().as_secs_f64());
        result
    }

    fn on_inserted_executed_block(&self, block: ExecutedBlock<N>) {
        let hash = block.recovered_block().hash();

        // Bound the tracking set to avoid unbounded growth.
        if self.pre_executed_blocks.len() >= MAX_TRACKED_BLOCKS {
            self.pre_executed_blocks.clear();
        }
        self.pre_executed_blocks.insert(hash);
        self.metrics.pre_executed_inserts.increment(1);

        trace!(
            target: "flashblocks::engine_validator",
            %hash,
            tracked = self.pre_executed_blocks.len(),
            "pre-executed block inserted into engine tree"
        );

        self.inner.on_inserted_executed_block(block);
    }
}

/// Builder that wraps an inner [`EngineValidatorBuilder`] and produces a
/// [`WorldChainEngineValidator`] around whatever the inner builder creates.
#[derive(Debug, Clone)]
pub struct WorldChainEngineValidatorBuilder<EVB> {
    inner: EVB,
}

impl<EVB> WorldChainEngineValidatorBuilder<EVB> {
    /// Create a new builder wrapping `inner`.
    pub const fn new(inner: EVB) -> Self {
        Self { inner }
    }
}

impl<Node, EVB> EngineValidatorBuilder<Node> for WorldChainEngineValidatorBuilder<EVB>
where
    Node: FullNodeComponents,
    EVB: EngineValidatorBuilder<Node>,
{
    type EngineValidator = WorldChainEngineValidator<EVB::EngineValidator>;

    async fn build_tree_validator(
        self,
        ctx: &AddOnsContext<'_, Node>,
        tree_config: reth_engine_primitives::TreeConfig,
        changeset_cache: ChangesetCache,
    ) -> eyre::Result<Self::EngineValidator> {
        let inner = self
            .inner
            .build_tree_validator(ctx, tree_config, changeset_cache)
            .await?;
        Ok(WorldChainEngineValidator::new(inner))
    }
}

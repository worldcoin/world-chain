//! Converts validated execution and full canonical notifications into the optional feed.

use std::time::{Duration, Instant};

use ::eyre::eyre::{WrapErr, bail, eyre};
use alloy_consensus::{Header, TxReceipt, transaction::TxHashRef};
use alloy_eips::BlockNumHash;
use alloy_primitives::{B256, Log as PrimitiveLog, keccak256};
use alloy_rpc_types::Log;
use alloy_rpc_types_engine::PayloadId;
use reth_chain_state::{CanonStateNotification, CanonStateNotifications};
use reth_chainspec::{ChainSpecProvider, EthChainSpec};
use reth_optimism_primitives::{OpPrimitives, OpReceipt};
use reth_primitives_traits::{Block, BlockBody, NodePrimitives, RecoveredBlock};
use reth_provider::{BlockNumReader, CanonStateSubscriptions, HeaderProvider};
use reth_tasks::TaskExecutor;
use tokio::{sync::broadcast, time::MissedTickBehavior};
use tracing::warn;
use world_chain_chainspec::WorldChainSpec;
use world_chain_validator::{
    coordinator::FlashblocksExecutionCoordinator,
    preconfirmed::{ValidatedEvent, ValidatedFlashblock, ValidatedSnapshot},
};

use super::{
    CanonicalCheckpoint, FeedError, FeedLimits, PreconfirmedLog, PreconfirmedLogStore,
    PreconfirmedView, PreparedLogBlock, ResetReason,
};

const MAX_CANONICAL_BLOCKS: usize = 64;
const LOG_ID_DOMAIN: &[u8] = b"worldchain.preconfirmed.log.v1";

/// Start a noncritical worker only when the optional API was explicitly enabled.
pub fn spawn_preconfirmed_log_feed<P>(
    provider: P,
    coordinator: FlashblocksExecutionCoordinator,
    task_executor: &TaskExecutor,
) -> eyre::Result<PreconfirmedLogStore>
where
    P: BlockNumReader
        + HeaderProvider<Header = Header>
        + CanonStateSubscriptions
        + ChainSpecProvider<ChainSpec = WorldChainSpec>
        + Send
        + Sync
        + 'static,
    P::Primitives: NodePrimitives<BlockHeader = Header, Receipt = OpReceipt>,
{
    let canonical = provider.subscribe_to_canonical_state();
    let checkpoint = provider_checkpoint(&provider)?;
    let chain_id = provider.chain_spec().chain_id();
    let store = PreconfirmedLogStore::new(chain_id, checkpoint, FeedLimits::default())?;
    coordinator.observe_canonical_tip(BlockNumHash::new(checkpoint.number, checkpoint.hash));
    let feed = store.clone();
    task_executor.spawn_task(async move {
        let _guard = FeedGuard(feed.clone());
        if let Err(error) = run(provider, coordinator, feed, canonical, chain_id).await {
            metrics::counter!("worldchain.preconfirmed.source_failures").increment(1);
            warn!(target: "rpc::preconfirmed", %error, "Preconfirmed log feed stopped; canonical RPC remains available");
        }
    });
    Ok(store)
}

/// Cancellation or a worker panic also makes the optional API fail closed.
struct FeedGuard(PreconfirmedLogStore);

impl Drop for FeedGuard {
    fn drop(&mut self) {
        self.0.mark_unavailable(ResetReason::SourceUnavailable);
    }
}

async fn run<P>(
    provider: P,
    coordinator: FlashblocksExecutionCoordinator,
    store: PreconfirmedLogStore,
    mut canonical: CanonStateNotifications<P::Primitives>,
    chain_id: u64,
) -> eyre::Result<()>
where
    P: BlockNumReader + HeaderProvider<Header = Header> + CanonStateSubscriptions,
    P::Primitives: NodePrimitives<BlockHeader = Header, Receipt = OpReceipt>,
{
    // Baseline notifications queued before the provider head was read, before taking a payload snapshot.
    let mut drained = false;
    for _ in 0..MAX_CANONICAL_BLOCKS {
        match canonical.try_recv() {
            Ok(notification) => {
                if let Err(error) =
                    apply_canonical(&provider, &store, &notification, chain_id, true)
                {
                    warn!(target: "rpc::preconfirmed", %error, "Resetting preconfirmed feed during canonical initialization");
                    resync(&provider, &coordinator, &store, ResetReason::SourceGap)?;
                }
            }
            Err(broadcast::error::TryRecvError::Empty) => {
                drained = true;
                break;
            }
            Err(broadcast::error::TryRecvError::Lagged(skipped)) => {
                canonical_lag(&provider, &coordinator, &store, skipped)?;
            }
            Err(broadcast::error::TryRecvError::Closed) => {
                bail!("canonical notification source closed");
            }
        }
    }
    if !drained {
        canonical = provider.subscribe_to_canonical_state();
        resync(&provider, &coordinator, &store, ResetReason::SourceGap)?;
    }

    let (snapshot, mut validated) = coordinator.subscribe_validated();
    let mut source = SourceCursor::default();
    if drained || canonical.is_empty() {
        apply_snapshot(&coordinator, &store, snapshot, chain_id, &mut source);
    }
    let mut heartbeat = tokio::time::interval(Duration::from_secs(1));
    heartbeat.set_missed_tick_behavior(MissedTickBehavior::Skip);

    loop {
        tokio::select! {
            biased;
            notification = canonical.recv() => match notification {
                Ok(notification) => {
                    if let Err(error) = apply_canonical(&provider, &store, &notification, chain_id, false) {
                        metrics::counter!("worldchain.preconfirmed.canonical_resyncs").increment(1);
                        warn!(target: "rpc::preconfirmed", %error, "Resetting preconfirmed feed after an inconsistent canonical update");
                        resync(&provider, &coordinator, &store, ResetReason::SourceGap)?;
                    }
                    if provider_checkpoint(&provider)? == store.checkpoint() {
                        let checkpoint = store.checkpoint();
                        coordinator.observe_canonical_tip(BlockNumHash::new(checkpoint.number, checkpoint.hash));
                    }
                    apply_snapshot(&coordinator, &store, coordinator.snapshot_validated(), chain_id, &mut source);
                }
                Err(broadcast::error::RecvError::Lagged(skipped)) => {
                    canonical_lag(&provider, &coordinator, &store, skipped)?;
                    let (snapshot, receiver) = coordinator.subscribe_validated();
                    validated = receiver;
                    apply_snapshot(&coordinator, &store, snapshot, chain_id, &mut source);
                }
                Err(broadcast::error::RecvError::Closed) => { bail!("canonical notification source closed"); },
            },
            event = validated.recv() => match event {
                Ok(ValidatedEvent::Payload(payload)) => {
                    apply_payload(&coordinator, &store, payload, chain_id, &mut source, false);
                }
                Ok(ValidatedEvent::Reset { canonical_generation, .. }) => {
                    let snapshot = coordinator.snapshot_validated();
                    if snapshot.canonical_generation == canonical_generation {
                        // Reduced notifications never assign canonical inclusion or change its checkpoint.
                        let checkpoint = store.checkpoint();
                        if source.canonical_generation != Some(canonical_generation)
                            || snapshot.canonical_tip != Some(BlockNumHash::new(checkpoint.number, checkpoint.hash))
                        {
                            store.mark_unavailable(ResetReason::SourceGap);
                        }
                        apply_snapshot(&coordinator, &store, snapshot, chain_id, &mut source);
                    }
                }
                Err(broadcast::error::RecvError::Lagged(skipped)) => {
                    metrics::counter!("worldchain.preconfirmed.source_lag").increment(skipped);
                    warn!(target: "rpc::preconfirmed", skipped, "Validated Flashblock source lagged; resynchronizing optional feed");
                    store.mark_unavailable(ResetReason::SourceGap);
                    let (snapshot, receiver) = coordinator.subscribe_validated();
                    validated = receiver;
                    apply_snapshot(&coordinator, &store, snapshot, chain_id, &mut source);
                }
                Err(broadcast::error::RecvError::Closed) => { bail!("validated Flashblock source closed"); },
            },
            _ = heartbeat.tick() => store.tick(),
        }
    }
}

fn provider_checkpoint<P>(provider: &P) -> eyre::Result<CanonicalCheckpoint>
where
    P: BlockNumReader + HeaderProvider<Header = Header>,
{
    let number = provider
        .best_block_number()
        .wrap_err("reading preconfirmed feed canonical height")?;
    let header = provider
        .sealed_header(number)
        .wrap_err("reading preconfirmed feed canonical header")?
        .ok_or_else(|| eyre!("canonical header missing at height {number}"))?;
    Ok(CanonicalCheckpoint {
        number,
        hash: header.hash(),
    })
}

fn resync<P>(
    provider: &P,
    coordinator: &FlashblocksExecutionCoordinator,
    store: &PreconfirmedLogStore,
    reason: ResetReason,
) -> eyre::Result<()>
where
    P: BlockNumReader + HeaderProvider<Header = Header>,
{
    let checkpoint = provider_checkpoint(provider)?;
    store.reset_to(checkpoint, reason);
    coordinator.observe_canonical_tip(BlockNumHash::new(checkpoint.number, checkpoint.hash));
    Ok(())
}

fn canonical_lag<P>(
    provider: &P,
    coordinator: &FlashblocksExecutionCoordinator,
    store: &PreconfirmedLogStore,
    skipped: u64,
) -> eyre::Result<()>
where
    P: BlockNumReader + HeaderProvider<Header = Header>,
{
    metrics::counter!("worldchain.preconfirmed.canonical_lag").increment(skipped);
    warn!(target: "rpc::preconfirmed", skipped, "Canonical source lagged; resetting optional feed to provider head");
    resync(provider, coordinator, store, ResetReason::SourceGap)
}

#[derive(Default)]
struct SourceCursor {
    sequence: u64,
    canonical_generation: Option<u64>,
}

impl SourceCursor {
    fn accepts(&self, sequence: u64, snapshot: bool) -> bool {
        sequence > self.sequence || (snapshot && sequence == self.sequence)
    }
}

fn matches_checkpoint(
    snapshot: &ValidatedSnapshot,
    checkpoint: CanonicalCheckpoint,
    generation: u64,
) -> bool {
    snapshot.canonical_tip == Some(BlockNumHash::new(checkpoint.number, checkpoint.hash))
        && snapshot.canonical_generation == generation
}

fn apply_snapshot(
    coordinator: &FlashblocksExecutionCoordinator,
    store: &PreconfirmedLogStore,
    snapshot: ValidatedSnapshot,
    chain_id: u64,
    source: &mut SourceCursor,
) {
    let checkpoint = store.checkpoint();
    if snapshot.canonical_tip == Some(BlockNumHash::new(checkpoint.number, checkpoint.hash)) {
        source.canonical_generation = Some(snapshot.canonical_generation);
    }
    if let Some(payload) = snapshot.current {
        apply_payload(coordinator, store, payload, chain_id, source, true);
    }
}

fn apply_payload(
    coordinator: &FlashblocksExecutionCoordinator,
    store: &PreconfirmedLogStore,
    payload: ValidatedFlashblock,
    chain_id: u64,
    source: &mut SourceCursor,
    snapshot: bool,
) {
    if !source.accepts(payload.sequence, snapshot) {
        return;
    }
    let current = coordinator.snapshot_validated();
    let checkpoint = store.checkpoint();
    if !matches_checkpoint(&current, checkpoint, payload.canonical_generation) {
        return;
    }
    if source.sequence != 0 && payload.sequence > source.sequence.saturating_add(1) {
        store.mark_unavailable(ResetReason::SourceGap);
    }
    let is_new = payload.sequence > source.sequence;
    source.sequence = source.sequence.max(payload.sequence);
    let started = Instant::now();
    let mut budget = ConversionBudget::default();
    let prepared = prepare_block::<OpPrimitives>(
        chain_id,
        payload.executed.recovered_block(),
        &payload.executed.execution_outcome().receipts,
        Some((payload.payload_id, payload.index)),
        &mut budget,
        store.limits(),
    );
    metrics::histogram!("worldchain.preconfirmed.conversion_seconds")
        .record(started.elapsed().as_secs_f64());
    let current = coordinator.snapshot_validated();
    if !matches_checkpoint(&current, checkpoint, payload.canonical_generation) {
        return;
    }
    let result =
        prepared.and_then(|prepared| store.apply_validated(prepared, payload.validated_at));
    if result.is_ok() && is_new {
        metrics::histogram!("worldchain.preconfirmed.publication_latency_seconds")
            .record(payload.validated_at.elapsed().as_secs_f64());
    }
    if let Err(error) = result {
        metrics::counter!("worldchain.preconfirmed.source_rejections").increment(1);
        warn!(target: "rpc::preconfirmed", %error, sequence = payload.sequence,
            index = payload.index, payload_id = ?payload.payload_id,
            block_hash = ?payload.executed.recovered_block().hash(),
            "Validated payload could not be retained in optional feed");
        store.mark_unavailable(match error {
            FeedError::LimitExceeded(_) => ResetReason::ResourceLimit,
            _ => ResetReason::InvalidSource,
        });
    }
}

fn apply_canonical<P, N>(
    provider: &P,
    store: &PreconfirmedLogStore,
    notification: &CanonStateNotification<N>,
    chain_id: u64,
    initializing: bool,
) -> eyre::Result<()>
where
    P: BlockNumReader + HeaderProvider<Header = Header>,
    N: NodePrimitives<BlockHeader = Header, Receipt = OpReceipt>,
{
    let new = notification.committed();
    let old = notification.reverted();
    let checkpoint = store.checkpoint();
    let tip = if let Some(tip) = notification.tip_checked() {
        CanonicalCheckpoint {
            number: tip.header().number,
            hash: tip.hash(),
        }
    } else {
        let first = old
            .as_ref()
            .and_then(|old| old.blocks().first_key_value())
            .ok_or_else(|| eyre!("canonical notification has no old or new blocks"))?
            .1;
        parent_checkpoint(first)?
    };
    if tip == checkpoint {
        return Ok(());
    }
    if initializing
        && tip.number < checkpoint.number
        && provider_checkpoint(provider)? == checkpoint
        && provider
            .sealed_header(tip.number)?
            .is_some_and(|header| header.hash() == tip.hash)
    {
        return Ok(());
    }
    if new
        .blocks()
        .len()
        .saturating_add(old.as_ref().map_or(0, |old| old.blocks().len()))
        > MAX_CANONICAL_BLOCKS
    {
        bail!("canonical segment exceeds optional feed block limit");
    }
    let predecessor = if let Some(old) = &old {
        let tip = old
            .blocks()
            .last_key_value()
            .ok_or_else(|| eyre!("reorg has no reverted blocks"))?
            .1;
        CanonicalCheckpoint {
            number: tip.header().number,
            hash: tip.hash(),
        }
    } else {
        parent_checkpoint(
            new.blocks()
                .first_key_value()
                .ok_or_else(|| eyre!("commit has no blocks"))?
                .1,
        )?
    };
    if predecessor != checkpoint {
        bail!("canonical notification does not follow the feed checkpoint");
    }
    if let (Some(old), Some((_, first_new))) = (&old, new.blocks().first_key_value()) {
        let first_old = old
            .blocks()
            .first_key_value()
            .ok_or_else(|| eyre!("reorg has no reverted blocks"))?
            .1;
        if parent_checkpoint(first_old)? != parent_checkpoint(first_new)? {
            bail!("reorg segments do not share a fork parent");
        }
    }
    let mut reverted = Vec::new();
    let mut committed = Vec::new();
    let mut budget = ConversionBudget::default();
    for (removed, chain) in old
        .iter()
        .map(|chain| (true, chain))
        .chain(std::iter::once((false, &new)))
    {
        if chain.blocks().len() != chain.execution_outcome().receipts().len() {
            bail!("canonical notification has incomplete block receipts");
        }
        let mut previous = None;
        for (block, receipts) in chain.blocks_and_receipts() {
            if let Some(previous) = previous
                && parent_checkpoint(block)? != previous
            {
                bail!("canonical notification contains a discontinuous chain");
            }
            previous = Some(CanonicalCheckpoint {
                number: block.header().number,
                hash: block.hash(),
            });
            let prepared =
                prepare_block::<N>(chain_id, block, receipts, None, &mut budget, store.limits())?;
            if removed {
                reverted.push(prepared);
            } else {
                committed.push(prepared);
            }
        }
    }
    store.apply_canonical(reverted, committed, tip)?;
    Ok(())
}

fn parent_checkpoint<B: Block<Header = Header>>(
    block: &RecoveredBlock<B>,
) -> eyre::Result<CanonicalCheckpoint> {
    Ok(CanonicalCheckpoint {
        number: block
            .header()
            .number
            .checked_sub(1)
            .ok_or_else(|| eyre!("cannot revert genesis"))?,
        hash: block.header().parent_hash,
    })
}

#[derive(Default)]
struct ConversionBudget {
    logs: usize,
    bytes: usize,
}

fn prepare_block<N>(
    chain_id: u64,
    block: &RecoveredBlock<N::Block>,
    receipts: &[OpReceipt],
    flashblock: Option<(PayloadId, u64)>,
    budget: &mut ConversionBudget,
    limits: &FeedLimits,
) -> Result<PreparedLogBlock, FeedError>
where
    N: NodePrimitives<BlockHeader = Header, Receipt = OpReceipt>,
{
    let transactions = block.body().transactions();
    if transactions.len() != receipts.len() {
        return Err(FeedError::InvalidFilter(
            "execution receipt count does not match transactions",
        ));
    }
    let mut logs = Vec::new();
    for (transaction_index, (transaction, receipt)) in transactions.iter().zip(receipts).enumerate()
    {
        for (transaction_log_index, inner) in receipt.logs().iter().enumerate() {
            if budget.logs >= limits.max_view_logs
                || inner.data.data.len() > limits.max_view_bytes / 2
            {
                return Err(FeedError::LimitExceeded("converted logs exceed view limit"));
            }
            let log = PreconfirmedLog {
                event_id: event_id(
                    chain_id,
                    transaction.tx_hash(),
                    transaction_log_index as u64,
                    inner,
                ),
                log: Log {
                    inner: inner.clone(),
                    block_hash: Some(block.hash()),
                    block_number: Some(block.header().number),
                    transaction_hash: Some(*transaction.tx_hash()),
                    transaction_index: Some(transaction_index as u64),
                    log_index: Some(logs.len() as u64),
                    removed: false,
                    block_timestamp: Some(block.header().timestamp),
                },
            };
            let bytes = serde_json::to_vec(&log)
                .map_err(|_| FeedError::LimitExceeded("could not serialize converted log"))?
                .len();
            budget.bytes = budget.bytes.saturating_add(bytes);
            if budget.bytes > limits.max_view_bytes {
                return Err(FeedError::LimitExceeded("converted logs exceed byte limit"));
            }
            budget.logs += 1;
            logs.push(log);
        }
    }
    Ok(PreparedLogBlock {
        view: PreconfirmedView {
            chain_id,
            parent_hash: block.header().parent_hash,
            block_number: block.header().number,
            block_hash: block.hash(),
            payload_id: flashblock.map(|(id, _)| id),
            flashblock_index: flashblock.map(|(_, index)| index),
            timestamp: block.header().timestamp,
        },
        logs,
    })
}

fn event_id(
    chain_id: u64,
    transaction_hash: &B256,
    transaction_log_index: u64,
    log: &PrimitiveLog,
) -> B256 {
    let mut identity = Vec::with_capacity(256);
    identity.extend_from_slice(LOG_ID_DOMAIN);
    identity.extend_from_slice(&chain_id.to_be_bytes());
    identity.extend_from_slice(transaction_hash.as_slice());
    identity.extend_from_slice(&transaction_log_index.to_be_bytes());
    identity.extend_from_slice(log.address.as_slice());
    identity.extend_from_slice(&(log.topics().len() as u64).to_be_bytes());
    for topic in log.topics() {
        identity.extend_from_slice(topic.as_slice());
    }
    identity.extend_from_slice(keccak256(&log.data.data).as_slice());
    keccak256(identity)
}

#[cfg(test)]
mod tests {
    use std::{collections::BTreeMap, sync::Arc};

    use alloy_consensus::{
        Block as AlloyBlock, BlockBody as AlloyBlockBody, Receipt, SignableTransaction, TxLegacy,
    };
    use alloy_primitives::{Address, Bytes, Signature, U256};
    use reth_optimism_primitives::{OpBlock, OpTransactionSigned};
    use reth_provider::{Chain, ExecutionOutcome};
    use reth_storage_api::noop::NoopProvider;

    use super::*;

    fn block(number: u64, parent_hash: B256) -> RecoveredBlock<OpBlock> {
        RecoveredBlock::new_unhashed(
            AlloyBlock {
                header: Header {
                    number,
                    parent_hash,
                    ..Default::default()
                },
                body: AlloyBlockBody::<OpTransactionSigned>::default(),
            },
            Vec::new(),
        )
    }

    #[test]
    fn pure_revert_moves_to_fork_parent_without_a_new_tip() {
        let old_block = block(10, B256::repeat_byte(1));
        let checkpoint = CanonicalCheckpoint {
            number: 10,
            hash: old_block.hash(),
        };
        let store = PreconfirmedLogStore::new(480, checkpoint, FeedLimits::default()).unwrap();
        let old = Chain::<OpPrimitives>::new(
            [old_block],
            ExecutionOutcome {
                receipts: vec![Vec::new()],
                ..Default::default()
            },
            BTreeMap::new(),
        );
        let notification = CanonStateNotification::Reorg {
            old: Arc::new(old),
            new: Arc::new(Chain::default()),
        };
        assert!(notification.tip_checked().is_none());
        apply_canonical(&NoopProvider::default(), &store, &notification, 480, false).unwrap();
        assert_eq!(
            store.checkpoint(),
            CanonicalCheckpoint {
                number: 9,
                hash: B256::repeat_byte(1)
            }
        );
    }

    #[test]
    fn canonical_predecessor_mismatch_does_not_change_checkpoint() {
        let checkpoint = CanonicalCheckpoint {
            number: 10,
            hash: B256::repeat_byte(1),
        };
        let store = PreconfirmedLogStore::new(480, checkpoint, FeedLimits::default()).unwrap();
        let new = Chain::<OpPrimitives>::new(
            [block(11, B256::repeat_byte(2))],
            ExecutionOutcome {
                receipts: vec![Vec::new()],
                ..Default::default()
            },
            BTreeMap::new(),
        );
        let notification = CanonStateNotification::Commit { new: Arc::new(new) };
        assert!(
            apply_canonical(&NoopProvider::default(), &store, &notification, 480, false).is_err()
        );
        assert_eq!(store.checkpoint(), checkpoint);
    }

    #[test]
    fn source_ordering_allows_explicit_snapshot_recovery_without_regression() {
        let source = SourceCursor {
            sequence: 42,
            canonical_generation: Some(2),
        };
        assert!(!source.accepts(41, false));
        assert!(!source.accepts(41, true));
        assert!(!source.accepts(42, false));
        assert!(source.accepts(42, true));
        assert!(source.accepts(43, false));
        let checkpoint = CanonicalCheckpoint {
            number: 10,
            hash: B256::repeat_byte(1),
        };
        let snapshot = ValidatedSnapshot {
            canonical_tip: Some(BlockNumHash::new(checkpoint.number, checkpoint.hash)),
            canonical_generation: 3,
            current: None,
        };
        assert!(!matches_checkpoint(&snapshot, checkpoint, 2));
        assert!(matches_checkpoint(&snapshot, checkpoint, 3));
        assert!(!matches_checkpoint(
            &snapshot,
            CanonicalCheckpoint::default(),
            3
        ));
    }

    #[test]
    fn log_identity_survives_partial_block_metadata_changes() {
        let transaction = OpTransactionSigned::Legacy(
            TxLegacy::default().into_signed(Signature::new(U256::ONE, U256::ONE, false)),
        );
        let mut original = block(10, B256::repeat_byte(1)).into_block();
        original.body.transactions.push(transaction);
        let mut replacement = original.clone();
        replacement.header.timestamp = 1;
        let original = RecoveredBlock::new_unhashed(original, vec![Address::ZERO]);
        let replacement = RecoveredBlock::new_unhashed(replacement, vec![Address::ZERO]);
        let inner = PrimitiveLog::new_unchecked(
            Address::repeat_byte(1),
            vec![B256::repeat_byte(2)],
            Bytes::from_static(b"registered"),
        );
        let receipts = [OpReceipt::Legacy(Receipt {
            logs: vec![inner],
            ..Default::default()
        })];
        let limits = FeedLimits::default();
        let first = prepare_block::<OpPrimitives>(
            480,
            &original,
            &receipts,
            Some((PayloadId::new([1; 8]), 0)),
            &mut ConversionBudget::default(),
            &limits,
        )
        .unwrap();
        let second = prepare_block::<OpPrimitives>(
            480,
            &replacement,
            &receipts,
            Some((PayloadId::new([1; 8]), 1)),
            &mut ConversionBudget::default(),
            &limits,
        )
        .unwrap();
        assert_eq!(first.logs[0].event_id, second.logs[0].event_id);
        assert_ne!(first.logs[0].log.block_hash, second.logs[0].log.block_hash);
        assert_eq!(first.logs[0].log.transaction_index, Some(0));
        assert_eq!(first.logs[0].log.log_index, Some(0));
        let mut limited = limits;
        limited.max_view_bytes = 8;
        assert!(matches!(
            prepare_block::<OpPrimitives>(
                480,
                &original,
                &receipts,
                None,
                &mut ConversionBudget::default(),
                &limited
            ),
            Err(FeedError::LimitExceeded(_))
        ));
    }
}

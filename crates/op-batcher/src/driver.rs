//! OP Batcher driver (`BatchSubmitter`).
//!
//! Mirrors `op-batcher/batcher/driver.go`, in the single-loop shape (the v1
//! choice documented in `BATCHER_SPEC.md` §4). Each poll cycle:
//!
//! 1. read local heads (`source.heads`) + L1 tip;
//! 2. [`compute_sync_actions`] → prune safe blocks / load the unsafe range;
//! 3. load blocks from local node state into the channel manager;
//! 4. drain ready frames to L1 as calldata batcher txs.
//!
//! Transaction nonce / gas / fee management is delegated to the alloy wallet
//! provider's filler stack (no `op-service/txmgr` port — see spec §1/§11).

use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use alloy_network::TransactionBuilder;
use alloy_primitives::{Address, Bytes};
use alloy_provider::{DynProvider, Provider};
use alloy_rpc_types_eth::TransactionRequest;
use parking_lot::Mutex;
use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, info, warn};

use crate::{
    Result,
    channel_manager::{ChannelManager, ChannelParams, PendingFrame},
    config::BatcherConfig,
    db::{BatcherStore, StoredBatch},
    error::OpBatcherError,
    metrics::BatcherMetrics,
    rpc::BatcherAdmin,
    source::LocalBlockSource,
    sync::compute_sync_actions,
};

/// Receipt-wait timeout for a submitted batch tx.
const RECEIPT_TIMEOUT: Duration = Duration::from_secs(120);

struct DriverState {
    running: bool,
    cancel: Option<CancellationToken>,
    stopped: Arc<Notify>,
}

/// The batch submitter.
pub struct BatchSubmitter {
    cfg: BatcherConfig,
    source: Arc<dyn LocalBlockSource>,
    /// Wallet-equipped L1 provider (reads + writes).
    l1: DynProvider,
    /// Batcher EOA (must match `SystemConfig.batcherHash`).
    from: Address,
    store: Arc<BatcherStore>,
    metrics: Arc<BatcherMetrics>,
    manager: Mutex<ChannelManager>,
    state: Mutex<DriverState>,
    /// Set by `flush` to force-close the open channel on the next cycle.
    flush: AtomicBool,
}

impl BatchSubmitter {
    pub fn new(
        cfg: BatcherConfig,
        source: Arc<dyn LocalBlockSource>,
        l1: DynProvider,
        from: Address,
        store: Arc<BatcherStore>,
        metrics: Arc<BatcherMetrics>,
    ) -> Self {
        let params = ChannelParams {
            max_rlp_bytes: crate::channel_out::MAX_RLP_BYTES_PER_CHANNEL_FJORD,
            max_frame_size: cfg.max_frame_size() as usize,
            // TargetNumFrames = 1 (calldata): one frame's worth of data per channel.
            target_output: (cfg.max_frame_size() as usize)
                .saturating_sub(crate::channel_out::FRAME_V0_OVERHEAD_SIZE),
            approx_compr_ratio: cfg.approx_compr_ratio,
            max_channel_duration: cfg.max_channel_duration,
        };
        let manager = Mutex::new(ChannelManager::new(params, metrics.clone()));
        Self {
            cfg,
            source,
            l1,
            from,
            store,
            metrics,
            manager,
            state: Mutex::new(DriverState {
                running: false,
                cancel: None,
                stopped: Arc::new(Notify::new()),
            }),
            flush: AtomicBool::new(false),
        }
    }

    /// Start the polling loop. Mirrors `(*BatchSubmitter).StartBatchSubmitting`.
    pub fn start(self: &Arc<Self>) -> Result<()> {
        let mut state = self.state.lock();
        if state.running {
            return Err(OpBatcherError::AlreadyRunning);
        }
        let cancel = CancellationToken::new();
        let stopped = Arc::new(Notify::new());
        state.cancel = Some(cancel.clone());
        state.stopped = stopped.clone();
        state.running = true;
        drop(state);

        info!(target: "exex::batcher", from = ?self.from, "starting batcher");
        let this = self.clone();
        tokio::spawn(async move {
            this.run_loop(cancel).await;
            stopped.notify_one();
        });
        Ok(())
    }

    /// Stop the polling loop, waiting for it to exit. Mirrors
    /// `(*BatchSubmitter).StopBatchSubmitting`.
    pub async fn stop(self: &Arc<Self>) -> Result<()> {
        let (cancel, stopped) = {
            let mut state = self.state.lock();
            if !state.running {
                return Err(OpBatcherError::NotRunning);
            }
            state.running = false;
            (state.cancel.take(), state.stopped.clone())
        };
        info!(target: "exex::batcher", "stopping batcher");
        if let Some(cancel) = cancel {
            cancel.cancel();
        }
        stopped.notified().await;
        info!(target: "exex::batcher", "batcher stopped");
        Ok(())
    }

    pub async fn stop_if_running(self: &Arc<Self>) {
        match self.stop().await {
            Ok(()) | Err(OpBatcherError::NotRunning) => {}
            Err(e) => warn!(target: "exex::batcher", error = %e, "stop returned non-fatal error"),
        }
    }

    /// Mirrors `(*BatchSubmitter).loop`.
    async fn run_loop(self: Arc<Self>, cancel: CancellationToken) {
        let mut ticker = tokio::time::interval(self.cfg.poll_interval);
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            tokio::select! {
                biased;
                _ = cancel.cancelled() => {
                    debug!(target: "exex::batcher", "loop cancelled");
                    break;
                }
                _ = ticker.tick() => {
                    if cancel.is_cancelled() { break; }
                    if let Err(e) = self.tick().await {
                        warn!(target: "exex::batcher", error = ?e, "batcher cycle error");
                    }
                }
            }
        }
    }

    /// One poll cycle.
    async fn tick(&self) -> Result<()> {
        let heads = self.source.heads().await?;
        let current_l1 = self.l1.get_block_number().await?;

        // 1. Sync actions from local heads.
        let newest_in_state = self.manager.lock().newest_block();
        let actions = compute_sync_actions(&heads, newest_in_state);
        if actions.out_of_sync {
            debug!(target: "exex::batcher", ?heads, "local heads out of sync; skipping cycle");
            return Ok(());
        }
        if let Some(safe) = actions.prune_safe {
            let mut mgr = self.manager.lock();
            mgr.prune_safe(safe);
            self.metrics.set_pending_blocks(mgr.pending_block_count());
        }

        // 2. Load the unsafe range from local node state.
        if let Some(range) = actions.blocks_to_load.clone() {
            for n in range {
                let block = self.source.l2_block(n).await?;
                let mut mgr = self.manager.lock();
                match mgr.add_l2_block(block) {
                    Ok(()) => {}
                    Err(e) => {
                        // Reorg / discontinuity: clear and reload next cycle.
                        warn!(target: "exex::batcher", error = %e, "clearing channel state");
                        mgr.clear();
                        return Ok(());
                    }
                }
            }
        }

        // 3. Publish ready frames.
        // Force-close the open channel when caught up to the unsafe head (or on
        // flush) so the safe head can advance. See spec §4 (liveness note).
        let caught_up = {
            let mgr = self.manager.lock();
            mgr.newest_block() == Some(heads.unsafe_l2) && heads.unsafe_l2 > heads.safe_l2
        };
        let force = caught_up || self.flush.swap(false, Ordering::SeqCst);

        self.publish(current_l1, force).await;
        Ok(())
    }

    /// Drain ready frames to L1. Mirrors `publishStateToL1` / `publishTxToL1`.
    async fn publish(&self, current_l1: u64, force: bool) {
        loop {
            let pending = {
                let mut mgr = self.manager.lock();
                match mgr.next_tx(current_l1, force) {
                    Ok(p) => p,
                    Err(e) => {
                        error!(target: "exex::batcher", error = %e, "channel error");
                        self.metrics.record_failure();
                        return;
                    }
                }
            };
            let Some(pending) = pending else { return };

            match self.send_frame(&pending).await {
                Ok(l1_block) => {
                    let mut mgr = self.manager.lock();
                    mgr.frame_confirmed(pending.channel_id, pending.frame.frame_number);
                    self.record_confirmed(&pending, l1_block);
                }
                Err(e) => {
                    error!(
                        target: "exex::batcher",
                        error = ?e,
                        channel_id = ?pending.channel_id,
                        frame = pending.frame.frame_number,
                        "failed to submit batch frame",
                    );
                    self.metrics.record_failure();
                    self.manager
                        .lock()
                        .frame_failed(pending.channel_id, pending.frame.frame_number);
                    return;
                }
            }
        }
    }

    /// Build and send one calldata batcher tx, awaiting its receipt. Returns the
    /// L1 inclusion block on success.
    async fn send_frame(&self, pending: &PendingFrame) -> Result<u64> {
        let payload = Bytes::from(pending.frame.payload.clone());
        let tx = TransactionRequest::default()
            .with_to(self.cfg.batch_inbox_address)
            .with_input(payload);

        let pending_tx = self.l1.send_transaction(tx).await.map_err(|e| {
            OpBatcherError::msg(format!("send_transaction to batch inbox failed: {e}"))
        })?;
        let tx_hash = *pending_tx.tx_hash();

        let receipt = pending_tx
            .with_required_confirmations(1)
            .with_timeout(Some(RECEIPT_TIMEOUT))
            .get_receipt()
            .await
            .map_err(|e| OpBatcherError::msg(format!("get_receipt({tx_hash:?}): {e}")))?;

        if !receipt.status() {
            return Err(OpBatcherError::msg(format!(
                "batch tx {:?} reverted on-chain",
                receipt.transaction_hash
            )));
        }

        let l1_block = receipt.block_number.unwrap_or(0);
        info!(
            target: "exex::batcher",
            tx_hash = ?receipt.transaction_hash,
            l1_block,
            channel_id = ?pending.channel_id,
            frame = pending.frame.frame_number,
            is_last = pending.frame.is_last,
            calldata_len = pending.frame.payload.len(),
            "batch frame published",
        );
        Ok(l1_block)
    }

    fn record_confirmed(&self, pending: &PendingFrame, l1_block: u64) {
        let (l2_start, l2_end) = pending.l2_range.unwrap_or((0, 0));
        self.metrics
            .record_batch_submission(l2_end, pending.frame.payload.len());
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        if let Err(e) = self.store.put_last_batch(&StoredBatch {
            l2_start,
            l2_end,
            channel_id: pending.channel_id,
            l1_tx_hash: Default::default(),
            l1_block_number: l1_block,
            at_unix: now,
        }) {
            warn!(target: "exex::batcher", error = %e, "failed to persist last batch");
        }
    }
}

/// Admin RPC handle over an `Arc<BatchSubmitter>`. The driver's `start`/`stop`
/// take `Arc<Self>`, so the admin surface lives on this wrapper.
pub struct AdminHandle(pub Arc<BatchSubmitter>);

#[async_trait::async_trait]
impl BatcherAdmin for AdminHandle {
    fn start(&self) -> std::result::Result<(), String> {
        self.0.start().map_err(|e| e.to_string())
    }

    async fn stop(&self) -> std::result::Result<(), String> {
        self.0.stop().await.map_err(|e| e.to_string())
    }

    async fn flush(&self) -> std::result::Result<(), String> {
        self.0.flush.store(true, Ordering::SeqCst);
        Ok(())
    }
}

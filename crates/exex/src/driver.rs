//! OP Proposer driver (port of `op-proposer/proposer/driver.go`).
//!
//! The driver owns the polling loop: every `poll_interval` it walks the DGF
//! to determine whether a proposal is due, fetches the proposal from the
//! configured [`ProposalSource`], and submits it on L1 by calling
//! `DisputeGameFactory.create(...).send()` on the wallet-equipped provider.
//!
//! All transaction concerns — gas estimation (with 3/2 fallback), nonce
//! management, signing, fee bumps — are handled by the provider's filler
//! stack (see [`crate::provider`]). This module has no custom transaction
//! manager and no custom receipt types.

use std::{
    sync::Arc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use alloy_primitives::Address;
use alloy_provider::{DynProvider, Provider};
use parking_lot::Mutex;
use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, info, warn};

use crate::{
    config::ProposerConfig,
    contracts::DisputeGameFactory,
    db::{ProposerStore, StoredProposal},
    error::{OpProposerError, Result},
    metrics::ProposerMetrics,
    source::{Proposal, ProposalSource, ProposalSourceError},
};

/// The L2 output submitter / proposer driver.
pub struct L2OutputSubmitter {
    cfg: ProposerConfig,
    source: Arc<dyn ProposalSource>,
    factory: DisputeGameFactory,
    /// Wallet-equipped L1 provider used for both reads and writes.
    l1: DynProvider,
    /// The proposer EOA — recorded from the wallet at startup.
    from: Address,
    metrics: Arc<ProposerMetrics>,
    store: Arc<ProposerStore>,

    state: Mutex<DriverState>,
}

struct DriverState {
    running: bool,
    cancel: Option<CancellationToken>,
    stopped: Arc<Notify>,
}

impl L2OutputSubmitter {
    pub fn new(
        cfg: ProposerConfig,
        source: Arc<dyn ProposalSource>,
        factory: DisputeGameFactory,
        l1: DynProvider,
        from: Address,
        metrics: Arc<ProposerMetrics>,
        store: Arc<ProposerStore>,
    ) -> Self {
        Self {
            cfg,
            source,
            factory,
            l1,
            from,
            metrics,
            store,
            state: Mutex::new(DriverState {
                running: false,
                cancel: None,
                stopped: Arc::new(Notify::new()),
            }),
        }
    }

    /// Whether the driver loop is currently running.
    pub fn is_running(&self) -> bool {
        self.state.lock().running
    }

    /// Address that signs and pays for proposer transactions.
    pub fn from_address(&self) -> Address {
        self.from
    }

    /// Start the polling loop.
    pub fn start(self: &Arc<Self>) -> Result<()> {
        let mut state = self.state.lock();
        if state.running {
            return Err(OpProposerError::AlreadyRunning);
        }
        let cancel = CancellationToken::new();
        let stopped = Arc::new(Notify::new());
        state.cancel = Some(cancel.clone());
        state.stopped = stopped.clone();
        state.running = true;
        drop(state);

        info!(target: "exex::proposer", "starting proposer");
        let this = self.clone();
        tokio::spawn(async move {
            this.run_loop(cancel).await;
            stopped.notify_waiters();
        });
        Ok(())
    }

    /// Stop the polling loop, waiting for it to exit.
    pub async fn stop(self: &Arc<Self>) -> Result<()> {
        let (cancel, stopped) = {
            let mut state = self.state.lock();
            if !state.running {
                return Err(OpProposerError::NotRunning);
            }
            state.running = false;
            (state.cancel.take(), state.stopped.clone())
        };
        info!(target: "exex::proposer", "stopping proposer");
        if let Some(cancel) = cancel {
            cancel.cancel();
        }
        stopped.notified().await;
        info!(target: "exex::proposer", "proposer stopped");
        Ok(())
    }

    /// Convenience: stops only if running, swallows `NotRunning`.
    pub async fn stop_if_running(self: &Arc<Self>) {
        match self.stop().await {
            Ok(()) | Err(OpProposerError::NotRunning) => {}
            Err(e) => warn!(target: "exex::proposer", error = %e, "stop returned non-fatal error"),
        }
    }

    async fn run_loop(self: Arc<Self>, cancel: CancellationToken) {
        if self.cfg.wait_node_sync
            && let Err(e) = self.wait_node_sync(&cancel).await
        {
            error!(target: "exex::proposer", error = ?e, "wait_node_sync failed");
        }

        let mut ticker = tokio::time::interval(self.cfg.poll_interval);
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            tokio::select! {
                biased;
                _ = cancel.cancelled() => {
                    debug!(target: "exex::proposer", "loop cancelled");
                    break;
                }
                _ = ticker.tick() => {
                    if cancel.is_cancelled() { break; }
                    match self.fetch_dgf_output().await {
                        Ok(Some(proposal)) => self.propose(&proposal).await,
                        Ok(None) => {}
                        Err(e) => {
                            warn!(target: "exex::proposer", error = ?e, "error getting proposal");
                        }
                    }
                }
            }
        }
    }

    /// Port of `FetchDGFOutput`.
    pub async fn fetch_dgf_output(&self) -> Result<Option<Proposal>> {
        let now_unix = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let cutoff = now_unix.saturating_sub(self.cfg.proposal_interval.as_secs());

        let recent = self
            .factory
            .has_proposed_since(self.from, cutoff, self.cfg.game_type)
            .await?;
        if let Some((ts, claim)) = recent {
            debug!(
                target: "exex::proposer",
                duration_since_secs = now_unix.saturating_sub(ts),
                ?claim,
                "duration since last game not past proposal interval",
            );
            self.metrics.record_skipped();
            return Ok(None);
        }

        let current_block = self.fetch_current_block_number().await?;
        if current_block == 0 {
            info!(target: "exex::proposer", "skipping proposal for genesis block");
            self.metrics.record_skipped();
            return Ok(None);
        }

        let proposal = self.fetch_output(current_block).await?;

        if let Some(last) = self.store.last_proposal()?
            && last.root == proposal.root
            && last.block_number == proposal.block_number
        {
            debug!(
                target: "exex::proposer",
                last_root = ?last.root,
                "skipping proposal: output root unchanged since last persisted submission",
            );
            self.metrics.record_skipped();
            return Ok(None);
        }

        info!(
            target: "exex::proposer",
            proposal_interval_secs = self.cfg.proposal_interval.as_secs(),
            block_number = proposal.block_number,
            "no proposals found for at least proposal interval, submitting proposal now",
        );
        Ok(Some(proposal))
    }

    /// Port of `FetchCurrentBlockNumber`.
    pub async fn fetch_current_block_number(&self) -> Result<u64> {
        let status = self.source.sync_status().await?;
        Ok(if self.cfg.allow_non_finalized {
            status.safe_l2
        } else {
            status.finalized_l2
        })
    }

    /// Port of `FetchOutput`.
    pub async fn fetch_output(&self, block: u64) -> Result<Proposal> {
        let proposal = self.source.proposal_at_block(block).await?;
        if proposal.block_number != block {
            return Err(OpProposerError::Source(
                ProposalSourceError::BlockNumberMismatch {
                    got: proposal.block_number,
                    expected: block,
                },
            ));
        }
        Ok(proposal)
    }

    async fn propose(&self, output: &Proposal) {
        match self.send_proposal(output).await {
            Ok(()) => {
                self.metrics.record_l2_proposal(output.block_number);
            }
            Err(e) => {
                self.metrics.record_failure();
                error!(
                    target: "exex::proposer",
                    error = ?e,
                    l1_blocknum = output.current_l1.number,
                    l1_blockhash = ?output.current_l1.hash,
                    "failed to send proposal transaction",
                );
            }
        }
    }

    /// Build and submit the proposal transaction directly via the contract
    /// instance. Gas estimation, nonce management, signing, and fee
    /// computation are all handled by the wallet-equipped provider's
    /// filler stack.
    async fn send_proposal(&self, output: &Proposal) -> Result<()> {
        info!(
            target: "exex::proposer",
            block_number = output.block_number,
            output_root = ?output.root,
            "proposing output root",
        );

        let bond = self.factory.init_bond(self.cfg.game_type).await?;
        let extra = alloy_primitives::Bytes::from(output.extra_data().to_vec());

        let pending = self
            .factory
            .instance()
            .create(self.cfg.game_type, output.root, extra)
            .value(bond)
            .send()
            .await?;

        let receipt = pending.get_receipt().await?;

        if !receipt.status() {
            error!(
                target: "exex::proposer",
                tx_hash = ?receipt.transaction_hash,
                "proposer tx successfully published but reverted",
            );
            return Ok(());
        }
        info!(
            target: "exex::proposer",
            tx_hash = ?receipt.transaction_hash,
            l1_blocknum = output.current_l1.number,
            l1_blockhash = ?output.current_l1.hash,
            "proposer tx successfully published",
        );

        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        self.store.put_last_proposal(&StoredProposal {
            game_type: self.cfg.game_type,
            block_number: output.block_number,
            root: output.root,
            tx_hash: receipt.transaction_hash,
            l1_block_number: receipt.block_number.unwrap_or(0),
            l1_block_hash: receipt.block_hash.unwrap_or_default(),
            proposer: self.from,
            at_unix: now,
        })?;
        Ok(())
    }

    /// Port of `waitNodeSync`. Polls the source sync status until it catches
    /// up with the current L1 head.
    async fn wait_node_sync(&self, cancel: &CancellationToken) -> Result<()> {
        let target = self.l1.get_block_number().await?;
        let deadline_interval = Duration::from_secs(12);
        loop {
            if cancel.is_cancelled() {
                return Ok(());
            }
            match self.source.sync_status().await {
                Ok(status) if status.current_l1.number >= target => return Ok(()),
                Ok(status) => {
                    debug!(
                        target: "exex::proposer",
                        target,
                        current = status.current_l1.number,
                        "waiting for rollup node to sync",
                    );
                }
                Err(e) => {
                    warn!(target: "exex::proposer", error = %e, "sync status error during wait");
                }
            }
            tokio::select! {
                _ = tokio::time::sleep(deadline_interval) => {}
                _ = cancel.cancelled() => return Ok(()),
            }
        }
    }
}

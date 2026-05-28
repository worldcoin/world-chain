//! OP Proposer driver.
//!
//! Mirrors: [`op-proposer/proposer/driver.go`][driver-go] @ tag
//! `op-proposer/v1.16.3-rc.1`.
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
//!
//! [driver-go]:
//!     https://github.com/ethereum-optimism/optimism/blob/op-proposer/v1.16.3-rc.1/op-proposer/proposer/driver.go

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
    DisputeGameFactory, Result,
    config::ProposerConfig,
    db::{ProposerStore, StoredProposal},
    error::OpProposerError,
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
    #[allow(clippy::wrong_self_convention)]
    pub fn from_address(&self) -> Address {
        self.from
    }

    /// Start the polling loop.
    ///
    /// Mirrors: `(L2OutputSubmitter).StartL2OutputSubmitting` in
    /// [driver.go L115–L134][src].
    ///
    /// [src]:
    ///     https://github.com/ethereum-optimism/optimism/blob/op-proposer/v1.16.3-rc.1/op-proposer/proposer/driver.go#L115-L134
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
            // `notify_one` (not `notify_waiters`) — stores a permit if no
            // waiter is registered yet, so `stop()` can't deadlock when the
            // loop finishes before the waiter parks itself.
            stopped.notify_one();
        });
        Ok(())
    }

    /// Stop the polling loop, waiting for it to exit.
    ///
    /// Mirrors: `(L2OutputSubmitter).StopL2OutputSubmitting` in
    /// [driver.go L144–L157][src].
    ///
    /// [src]:
    ///     https://github.com/ethereum-optimism/optimism/blob/op-proposer/v1.16.3-rc.1/op-proposer/proposer/driver.go#L144-L157
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
    ///
    /// Mirrors: `(L2OutputSubmitter).StopL2OutputSubmittingIfRunning` in
    /// [driver.go L136–L142][src].
    ///
    /// [src]:
    ///     https://github.com/ethereum-optimism/optimism/blob/op-proposer/v1.16.3-rc.1/op-proposer/proposer/driver.go#L136-L142
    pub async fn stop_if_running(self: &Arc<Self>) {
        match self.stop().await {
            Ok(()) | Err(OpProposerError::NotRunning) => {}
            Err(e) => warn!(target: "exex::proposer", error = %e, "stop returned non-fatal error"),
        }
    }

    /// Mirrors: `(L2OutputSubmitter).loop` in [driver.go L261–L294][src].
    ///
    /// Differences from the Go original:
    /// - The Go loop uses a manual `select { ticker.C / l.done }` chain; here
    ///   we use `tokio::select!` over the cancellation token + ticker tick.
    /// - Cancellation is signalled via [`tokio_util::CancellationToken`]
    ///   instead of a `done chan struct{}`. Semantics are identical: a
    ///   cancellation wins over a fired tick (`biased;` matches Go's
    ///   `case <-l.done: return` short-circuit at the top of each cycle).
    ///
    /// [src]:
    ///     https://github.com/ethereum-optimism/optimism/blob/op-proposer/v1.16.3-rc.1/op-proposer/proposer/driver.go#L261-L294
    async fn run_loop(self: Arc<Self>, cancel: CancellationToken) {
        if self.cfg.wait_node_sync
            && let Err(e) = self.wait_node_sync(&cancel).await
        {
            error!(target: "exex::proposer", error = %e, "wait_node_sync failed");
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

    /// Mirrors: `(L2OutputSubmitter).FetchDGFOutput` in
    /// [driver.go L164–L200][src].
    ///
    /// Differences from the Go original:
    /// - `claim` equality skip (Go L192–L195) is rephrased as an MDBX
    ///   persistence check (we don't always have the previous claim from
    ///   `HasProposedSince`, so we compare against the last persisted
    ///   submission instead — strictly stronger: identical
    ///   (root, block_number) is skipped even across restarts).
    ///
    /// [src]:
    ///     https://github.com/ethereum-optimism/optimism/blob/op-proposer/v1.16.3-rc.1/op-proposer/proposer/driver.go#L164-L200
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

    /// Mirrors: `(L2OutputSubmitter).FetchCurrentBlockNumber` in
    /// [driver.go L204–L215][src].
    ///
    /// [src]:
    ///     https://github.com/ethereum-optimism/optimism/blob/op-proposer/v1.16.3-rc.1/op-proposer/proposer/driver.go#L204-L215
    pub async fn fetch_current_block_number(&self) -> Result<u64> {
        let status = self.source.sync_status().await?;
        Ok(if self.cfg.allow_non_finalized {
            status.safe_l2
        } else {
            status.finalized_l2
        })
    }

    /// Mirrors: `(L2OutputSubmitter).FetchOutput` in
    /// [driver.go L217–L226][src].
    ///
    /// The super-root branch (`!proposal.IsSuperRootProposal()`) is omitted —
    /// interop is intentionally not supported here.
    ///
    /// [src]:
    ///     https://github.com/ethereum-optimism/optimism/blob/op-proposer/v1.16.3-rc.1/op-proposer/proposer/driver.go#L217-L226
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

    /// Mirrors: `(L2OutputSubmitter).proposeOutput` in
    /// [driver.go L314–L336][src].
    ///
    /// [src]:
    ///     https://github.com/ethereum-optimism/optimism/blob/op-proposer/v1.16.3-rc.1/op-proposer/proposer/driver.go#L314-L336
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
    ///
    /// Mirrors: `(L2OutputSubmitter).sendTransaction` in
    /// [driver.go L235–L257][src] **and** the contract-side bond/calldata
    /// assembly from `(L2OutputSubmitter).ProposeL2OutputDGFTxCandidate`
    /// in [driver.go L228–L232][candidate] — fused here because alloy's
    /// `CallBuilder::send()` replaces the Go `TxManager.Send(candidate)`
    /// pattern.
    ///
    /// [src]:
    ///     https://github.com/ethereum-optimism/optimism/blob/op-proposer/v1.16.3-rc.1/op-proposer/proposer/driver.go#L235-L257
    /// [candidate]:
    ///     https://github.com/ethereum-optimism/optimism/blob/op-proposer/v1.16.3-rc.1/op-proposer/proposer/driver.go#L228-L232
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

        let tx_hash = *pending.tx_hash();
        let receipt = pending
            .with_required_confirmations(1)
            .with_timeout(Some(Duration::from_secs(120)))
            .get_receipt()
            .await
            .map_err(|e| OpProposerError::msg(format!("get_receipt({tx_hash:?}): {e}")))?;

        if !receipt.status() {
            error!(
                target: "exex::proposer",
                tx_hash = ?receipt.transaction_hash,
                "proposer tx successfully published but reverted",
            );
            // Surface this as a failure so `propose` records it via
            // `record_failure` and *not* `record_l2_proposal` — a reverted
            // tx must not bump `proposal_submissions` / `proposed_block_number`.
            return Err(OpProposerError::msg(format!(
                "proposer tx {:?} reverted on-chain",
                receipt.transaction_hash
            )));
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

    /// Polls the source sync status until it catches up with the current L1
    /// head.
    ///
    /// Mirrors: `(L2OutputSubmitter).waitNodeSync` in
    /// [driver.go L296–L312][src].
    ///
    /// [src]:
    ///     https://github.com/ethereum-optimism/optimism/blob/op-proposer/v1.16.3-rc.1/op-proposer/proposer/driver.go#L296-L312
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

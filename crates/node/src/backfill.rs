//! `NoopBackfillSync` — a [`BackfillSync`] implementation that is intentionally a no-op.
//!
//! # Why
//!
//! World Chain (a fork of op-reth) triggers a pipeline-based backfill sync whenever
//! P2P peers announce blocks more than 32 blocks ahead of the current canonical tip.
//! This is the correct behaviour for standalone EL nodes that use P2P as their source
//! of truth for chain progression.
//!
//! However, when a consensus client (e.g. `kona-node`) is the sync authority and drives
//! the chain via the Engine API, spontaneous P2P-triggered backfills are actively harmful:
//!
//! - reth transitions the engine into pipeline mode from a peer announcement.
//! - Pipeline mode causes reth to return `SYNCING` to every `engine_forkchoiceUpdated`
//!   call from the CL.
//! - `kona-node` sends a single FCU at startup, receives `SYNCING`, and enters
//!   `AwaitingELSyncCompletion` — where it waits for the EL to finish syncing before
//!   sending another FCU.
//! - The EL, meanwhile, is waiting for the CL to advance its target. Neither side
//!   progresses. Deadlock.
//!
//! By replacing [`PipelineSync`](reth_engine_tree::backfill::PipelineSync) with
//! [`NoopBackfillSync`], P2P-triggered backfill actions are silently dropped and the
//! CL retains exclusive control over chain advancement via the Engine API. This is the
//! desired behaviour for archive / verifier nodes that are always CL-driven.
//!
//! Enable via the `--engine.no-backfill` CLI flag.

use std::task::{Context, Poll};

use reth_engine_tree::backfill::{BackfillAction, BackfillEvent, BackfillSync};

/// A boxed [`BackfillSync`] implementation.
///
/// This lets our custom launcher use a single orchestrator type regardless of
/// which backfill sync backend is active — at runtime we swap between the
/// upstream `PipelineSync` and [`NoopBackfillSync`] behind this wrapper.
///
/// `Box<T>` is unconditionally `Unpin`, so this type satisfies the
/// `ChainOrchestrator` `P: BackfillSync + Unpin` bound without further work.
pub struct BoxedBackfillSync(pub Box<dyn BackfillSync + Send + 'static>);

impl BoxedBackfillSync {
    /// Wrap a concrete [`BackfillSync`] implementation.
    pub fn new<B: BackfillSync + Send + 'static>(inner: B) -> Self {
        Self(Box::new(inner))
    }
}

impl std::fmt::Debug for BoxedBackfillSync {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BoxedBackfillSync").finish_non_exhaustive()
    }
}

impl BackfillSync for BoxedBackfillSync {
    fn on_action(&mut self, action: BackfillAction) {
        self.0.on_action(action);
    }

    fn poll(&mut self, cx: &mut Context<'_>) -> Poll<BackfillEvent> {
        self.0.poll(cx)
    }
}

/// A [`BackfillSync`] implementation that never performs any backfill work.
///
/// All [`BackfillAction`]s are dropped, [`poll`](BackfillSync::poll) always returns
/// [`Poll::Pending`], and no [`BackfillEvent`]s are ever emitted. As a result the
/// engine tree's internal `backfill_sync_state` stays [`BackfillSyncState::Idle`],
/// which means `engine_forkchoiceUpdated` is never short-circuited to `SYNCING` from
/// a P2P-triggered pipeline.
///
/// [`BackfillSyncState::Idle`]: reth_engine_tree::backfill::BackfillSyncState::Idle
#[derive(Debug, Default, Clone, Copy)]
pub struct NoopBackfillSync;

impl NoopBackfillSync {
    /// Create a new [`NoopBackfillSync`].
    pub const fn new() -> Self {
        Self
    }
}

impl BackfillSync for NoopBackfillSync {
    fn on_action(&mut self, action: BackfillAction) {
        // Drop the action. Log at debug so operators running with
        // `--engine.no-backfill` can see peer-triggered backfill requests that
        // were intentionally ignored.
        tracing::debug!(
            target: "world_chain::backfill",
            ?action,
            "NoopBackfillSync: ignoring backfill action (CL is sync authority)"
        );
    }

    fn poll(&mut self, _cx: &mut Context<'_>) -> Poll<BackfillEvent> {
        // Never produce an event. The engine tree therefore never observes a
        // backfill start/finish transition originating from this component.
        Poll::Pending
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::future::poll_fn;
    use reth_stages_api::PipelineTarget;

    #[tokio::test]
    async fn noop_backfill_never_yields() {
        let mut noop = NoopBackfillSync::new();
        noop.on_action(BackfillAction::Start(PipelineTarget::Unwind(1_000)));

        // Poll a handful of times — must always be pending, never produce an event.
        for _ in 0..8 {
            let poll = poll_fn(|cx| {
                let p = noop.poll(cx);
                Poll::Ready(matches!(p, Poll::Pending))
            })
            .await;
            assert!(poll, "NoopBackfillSync must always be Poll::Pending");
        }
    }
}

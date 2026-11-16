use super::*;
use alloy_consensus::{Block, BlockBody, Header};
use alloy_rpc_types_engine::PayloadId;
use reth_optimism_primitives::OpTransactionSigned;
use reth_primitives_traits::{SealedBlock, SealedHeader};
use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};
use world_chain_builder::payload_builder_metrics::PayloadBuildMetrics;
use world_chain_p2p::protocol::handler::{Publisher, PublishingStatus};
use world_chain_primitives::ed25519_dalek::SigningKey;

#[allow(
    dead_code,
    reason = "shared conformance harness includes other signer operations"
)]
#[path = "../../../signer/tests/common/runtime.rs"]
mod runtime;

#[derive(Clone)]
struct Builder {
    payload: OpBuiltPayload,
    empty_builds: Arc<AtomicUsize>,
}

impl PayloadBuilder for Builder {
    type Attributes = OpPayloadAttrs;
    type BuiltPayload = OpBuiltPayload;

    fn try_build(
        &self,
        _args: BuildArguments<Self::Attributes, Self::BuiltPayload>,
    ) -> Result<BuildOutcome<Self::BuiltPayload>, PayloadBuilderError> {
        Ok(BuildOutcome::Aborted {
            fees: U256::ZERO,
            cached_reads: Default::default(),
        })
    }

    fn build_empty_payload(
        &self,
        _config: PayloadConfig<Self::Attributes, HeaderForPayload<Self::BuiltPayload>>,
    ) -> Result<Self::BuiltPayload, PayloadBuilderError> {
        self.empty_builds.fetch_add(1, Ordering::SeqCst);
        Ok(self.payload.clone())
    }
}

impl FlashblockPayloadBuilder for Builder {
    fn payload_build_metrics(&self) -> Arc<PayloadBuildMetrics> {
        Arc::new(PayloadBuildMetrics::default())
    }

    fn try_build_with_precommit(
        &self,
        args: BuildArguments<Self::Attributes, Self::BuiltPayload>,
        _committed: Option<&Self::BuiltPayload>,
    ) -> Result<
        (
            BuildOutcome<Self::BuiltPayload>,
            Option<FlashblockAccessList>,
        ),
        PayloadBuilderError,
    > {
        Ok((self.try_build(args)?, None))
    }
}

fn job(handle: FlashblocksHandle) -> FlashblocksPayloadJob<Builder> {
    let id = PayloadId::new([1; 8]);
    let block = SealedBlock::seal_slow(Block {
        header: Header {
            number: 1,
            timestamp: 1,
            ..Default::default()
        },
        body: BlockBody::<OpTransactionSigned>::default(),
    });
    let payload = OpBuiltPayload::new(id, Arc::new(block), U256::ZERO, None);
    let authorization = Authorization::new(
        id,
        1,
        &SigningKey::from_bytes(&[7; 32]),
        handle.publisher_key().expect("publisher"),
    );
    let until = tokio::time::Instant::now() + Duration::from_secs(60);
    let lease = handle
        .start_publishing_until(authorization, until)
        .expect("publishing intent");
    let (pending_block, _) = tokio::sync::watch::channel(None);
    let coordinator = FlashblocksExecutionCoordinator::new(handle.clone(), pending_block);
    let config = PayloadConfig::new(
        Arc::new(SealedHeader::seal_slow(Header::default())),
        OpPayloadAttrs::from(alloy_rpc_types_engine::PayloadAttributes {
            timestamp: 1,
            ..Default::default()
        }),
        id,
    );
    FlashblocksPayloadJob {
        pending_publication: None,
        _publishing_lease: Some(lease),
        config,
        executor: Runtime::test(),
        best_payload: (PayloadState::Best(payload.clone()), None),
        committed_payload: CommittedPayloadState::Empty,
        pending_block: None,
        payload_task_guard: PayloadTaskGuard::new(1),
        cached_reads: None,
        execution_cache: None,
        state_root_handle: None,
        leases: vec![],
        builder: Builder {
            payload,
            empty_builds: Arc::new(AtomicUsize::new(0)),
        },
        authorization: Some(authorization),
        deadline: Box::pin(tokio::time::sleep_until(until)),
        flashblock_deadline: Box::pin(tokio::time::sleep_until(
            tokio::time::Instant::now() - Duration::from_secs(1),
        )),
        flashblock_interval: Duration::from_millis(200),
        recommit_interval: tokio::time::interval_at(until, Duration::from_secs(60)),
        p2p_handler: handle,
        flashblocks_state: coordinator,
        block_index: 0,
    }
}

fn remote_handle(harness: &runtime::Harness, denied: bool) -> FlashblocksHandle {
    let mut config = harness.ed25519(false, "publisher");
    if denied {
        config.key_id = B256::repeat_byte(0xff);
    }
    FlashblocksHandle::with_publisher(
        SigningKey::from_bytes(&[7; 32]).verifying_key(),
        Some(Publisher::Remote(
            world_chain_signer::PublicationSigner::new(config).expect("remote publisher"),
        )),
        Default::default(),
        None,
    )
}

struct HeldSignature(Arc<AtomicBool>);
impl Future for HeldSignature {
    type Output = Result<AuthorizedPayload<FlashblocksPayloadV1>, FlashblocksP2PError>;
    fn poll(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Self::Output> {
        Poll::Pending
    }
}
impl Drop for HeldSignature {
    fn drop(&mut self) {
        self.0.store(true, Ordering::SeqCst);
    }
}

fn hold_signature(job: &mut FlashblocksPayloadJob<Builder>) -> Arc<AtomicBool> {
    let cancelled = Arc::new(AtomicBool::new(false));
    job.pending_publication = Some(PendingPublication {
        signing: Box::pin(HeldSignature(cancelled.clone())),
        payload: job.builder.payload.clone(),
        access_list: None,
    });
    cancelled
}

async fn clearance(job: &FlashblocksPayloadJob<Builder>) -> Result<(), FlashblocksP2PError> {
    tokio::time::timeout_at(
        runtime::deadline(),
        job.p2p_handler
            .await_authorized_clearance(job.authorization.expect("grant")),
    )
    .await
    .expect("bounded control signing")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn local_handover_preserves_the_execution_job_and_inflight_build() {
    let handle = FlashblocksHandle::new(
        SigningKey::from_bytes(&[7; 32]).verifying_key(),
        Some(SigningKey::from_bytes(&[8; 32])),
    );
    let mut job = job(handle);
    let (_outcome_tx, outcome_rx) = oneshot::channel();
    let (_access_tx, access_rx) = oneshot::channel();
    job.pending_block = Some(FlashblocksPendingPayload::new(
        CancelOnDrop::default(),
        outcome_rx,
        access_rx,
    ));
    job.p2p_handler
        .stop_publishing()
        .expect("peer handover fences publication");
    assert!(futures::poll!(&mut job).is_pending());
    assert!(job.authorization.is_none());
    assert!(
        job.pending_block.is_some(),
        "handover must not cancel execution work"
    );
    assert!(futures::poll!(&mut job).is_pending());
    let (result, _) = job.resolve_kind(PayloadKind::Earliest);
    assert_eq!(
        result
            .await
            .expect("execution payload remains resolvable")
            .block()
            .hash(),
        job.builder.payload.block().hash()
    );
    assert!(job.flashblocks_state.snapshot().latest_payload.is_none());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires the reviewed disposable mTLS signer harness"]
async fn remote_job_resolution_returns_execution_payload_and_cancels_publication() {
    let harness = runtime::Harness::start().await;
    let mut job = job(remote_handle(&harness, false));
    clearance(&job).await.expect("remote clearance");
    let cancelled = hold_signature(&mut job);
    let (resolved, keep) = job.resolve_kind(PayloadKind::WaitForPending);
    assert_eq!(keep, KeepPayloadJobAlive::No);
    assert_eq!(
        resolved
            .await
            .expect("built block does not need publication signature")
            .block()
            .hash(),
        job.builder.payload.block().hash()
    );
    assert!(cancelled.load(Ordering::SeqCst));
    assert_eq!(job.builder.empty_builds.load(Ordering::SeqCst), 0);
    assert!(job.flashblocks_state.snapshot().latest_payload.is_none());
    assert!(matches!(
        job.p2p_handler.publishing_status(),
        PublishingStatus::NotPublishing { .. }
    ));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires the reviewed disposable mTLS signer harness"]
async fn remote_job_publication_denial_keeps_execution_payload_available() {
    let harness = runtime::Harness::start().await;
    let mut job = job(remote_handle(&harness, true));
    assert!(clearance(&job).await.is_err());
    assert!(futures::poll!(&mut job).is_pending());
    assert!(job.authorization.is_none());
    let (resolved, _) = job.resolve_kind(PayloadKind::Earliest);
    assert_eq!(
        resolved
            .await
            .expect("publication denial does not invalidate execution")
            .block()
            .hash(),
        job.builder.payload.block().hash()
    );
    assert_eq!(job.builder.empty_builds.load(Ordering::SeqCst), 0);
    assert!(job.flashblocks_state.snapshot().latest_payload.is_none());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires the reviewed disposable mTLS signer harness"]
async fn remote_job_resolution_selects_the_newer_built_payload_over_its_committed_prefix() {
    let harness = runtime::Harness::start().await;
    let mut job = job(remote_handle(&harness, false));
    clearance(&job).await.expect("remote clearance");
    let earlier = SealedBlock::seal_slow(Block {
        header: Header {
            number: 1,
            timestamp: 1,
            state_root: B256::repeat_byte(9),
            ..Default::default()
        },
        body: BlockBody::<OpTransactionSigned>::default(),
    });
    job.committed_payload = CommittedPayloadState::Best {
        payload: OpBuiltPayload::new(
            job.builder.payload.id(),
            Arc::new(earlier),
            U256::ZERO,
            None,
        ),
        access_list: None,
    };
    let cancelled = hold_signature(&mut job);
    let (resolved, _) = job.resolve_kind(PayloadKind::Earliest);
    let payload = resolved.await.expect("latest completed execution");
    assert_eq!(payload.block().hash(), job.builder.payload.block().hash());
    assert_ne!(
        Some(payload.block().hash()),
        job.committed_payload.block_hash()
    );
    assert!(cancelled.load(Ordering::SeqCst));
    assert!(job.flashblocks_state.snapshot().latest_payload.is_none());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires the reviewed disposable mTLS signer harness"]
async fn remote_job_deadline_cancels_signing_without_publishing_expired_results() {
    let harness = runtime::Harness::start().await;
    let mut job = job(remote_handle(&harness, false));
    clearance(&job).await.expect("remote clearance");
    let cancelled = Arc::new(AtomicBool::new(false));
    let guard = HeldSignature(cancelled.clone());
    let authorization = job.authorization.expect("grant");
    let signed = AuthorizedPayload::new(
        &SigningKey::from_bytes(&[8; 32]),
        authorization,
        FlashblocksPayloadV1 {
            payload_id: authorization.payload_id,
            ..Default::default()
        },
    );
    job.pending_publication = Some(PendingPublication {
        signing: Box::pin(async move {
            let _guard = guard;
            Ok(signed)
        }),
        payload: job.builder.payload.clone(),
        access_list: None,
    });
    job.deadline
        .as_mut()
        .reset(tokio::time::Instant::now() - Duration::from_secs(1));
    assert!(matches!(futures::poll!(&mut job), Poll::Ready(Ok(()))));
    assert!(cancelled.load(Ordering::SeqCst));
    assert!(job.flashblocks_state.snapshot().latest_payload.is_none());
    assert!(matches!(
        job.p2p_handler.publishing_status(),
        PublishingStatus::NotPublishing { .. }
    ));
}

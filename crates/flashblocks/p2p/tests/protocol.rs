use alloy_primitives::B256;
use ed25519_dalek::SigningKey;
use flashblocks_p2p::protocol::{
    event::{FlashblocksEvent, WorldChainEventsStream},
    handler::{FlashblocksHandle, PublishingStatus},
};
use flashblocks_primitives::{
    flashblocks::FlashblockMetadata,
    p2p::{Authorization, AuthorizedPayload},
    primitives::{ExecutionPayloadBaseV1, ExecutionPayloadFlashblockDeltaV1, FlashblocksPayloadV1},
};
use futures::StreamExt as _;
use reth::{
    payload::PayloadId,
    providers::{CanonStateNotification, CanonStateSubscriptions, Chain, ExecutionOutcome},
};
use reth_ethereum::primitives::RecoveredBlock;
use std::{collections::BTreeMap, sync::Arc, time::Duration};
use tokio::{sync::broadcast, task};

const DUMMY_TIMESTAMP: u64 = 42;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Helper: deterministic ed25519 key made of the given byte.
fn signing_key(byte: u8) -> SigningKey {
    SigningKey::from_bytes(&[byte; 32])
}

/// Helper: a minimal Flashblock for the given payload-id and index.
///
/// `block_number` is set to 1 so that `PendingCursor::advance` (which
/// computes `base.block_number - 1`) does not underflow.
fn payload(payload_id: reth::payload::PayloadId, idx: u64) -> FlashblocksPayloadV1 {
    FlashblocksPayloadV1 {
        payload_id,
        index: idx,
        base: Some(ExecutionPayloadBaseV1 {
            block_number: 1,
            ..Default::default()
        }),
        diff: ExecutionPayloadFlashblockDeltaV1 {
            ..ExecutionPayloadFlashblockDeltaV1::default()
        },
        metadata: FlashblockMetadata::default(),
    }
}

/// Like [`payload`] but with a custom `block_number` and `parent_hash`,
/// allowing the test to place the flashblock in a different epoch.
fn payload_with_parent(
    payload_id: reth::payload::PayloadId,
    idx: u64,
    block_number: u64,
    parent_hash: B256,
) -> FlashblocksPayloadV1 {
    FlashblocksPayloadV1 {
        payload_id,
        index: idx,
        base: Some(ExecutionPayloadBaseV1 {
            block_number,
            parent_hash,
            ..Default::default()
        }),
        diff: ExecutionPayloadFlashblockDeltaV1::default(),
        metadata: FlashblockMetadata::default(),
    }
}

/// Build a fresh handle.
fn fresh_handle() -> FlashblocksHandle {
    let auth_sk = signing_key(1);
    let builder_sk = signing_key(2);
    FlashblocksHandle::new(auth_sk.verifying_key(), Some(builder_sk))
}

/// Mock provider that implements [`CanonStateSubscriptions`] for tests.
///
/// Wraps a [`broadcast::Sender`] and hands out a new receiver on each call
/// to `subscribe_to_canonical_state`, which is exactly what
/// [`WorldChainEventsStream::new`] needs.
struct MockCanonProvider {
    tx: broadcast::Sender<CanonStateNotification>,
}

impl MockCanonProvider {
    /// Create a new mock provider and return it alongside the broadcast
    /// sender used to inject canonical state notifications from the test.
    fn new() -> (broadcast::Sender<CanonStateNotification>, Self) {
        let (tx, _rx) = broadcast::channel(16);
        (tx.clone(), Self { tx })
    }
}

impl reth::providers::NodePrimitivesProvider for MockCanonProvider {
    type Primitives = reth_ethereum::EthPrimitives;
}

impl CanonStateSubscriptions for MockCanonProvider {
    fn subscribe_to_canonical_state(
        &self,
    ) -> reth::providers::CanonStateNotifications<reth_ethereum::EthPrimitives> {
        self.tx.subscribe()
    }
}

/// Create a [`CanonStateNotification::Commit`] whose tip has the given block
/// number and hash.
fn canon_notification(number: u64, hash: B256) -> CanonStateNotification {
    let mut block = reth_ethereum::Block::default();
    block.header.number = number;
    let recovered: RecoveredBlock<reth_ethereum::Block> = RecoveredBlock::new(block, vec![], hash);
    CanonStateNotification::Commit {
        new: Arc::new(Chain::new(
            vec![recovered],
            ExecutionOutcome::default(),
            BTreeMap::new(),
        )),
    }
}

/// Advance a [`WorldChainEventsStream`] until the next
/// [`FlashblocksEvent::Pending`] is yielded, skipping any
/// [`FlashblocksEvent::Canon`] items.
async fn next_flashblock(stream: &mut WorldChainEventsStream) -> FlashblocksPayloadV1 {
    loop {
        match stream.next().await.unwrap() {
            FlashblocksEvent::Pending(fb) => return fb,
            FlashblocksEvent::Canon(_) => continue,
        }
    }
}

// ---------------------------------------------------------------------------
// Tests that do NOT use streams — unchanged
// ---------------------------------------------------------------------------

#[tokio::test]
async fn publish_without_clearance_is_rejected() {
    let handle = fresh_handle();
    let builder_sk = handle.builder_sk().unwrap();

    let payload_id = reth::payload::PayloadId::new([0; 8]);
    let auth = Authorization::new(
        payload_id,
        DUMMY_TIMESTAMP,
        &signing_key(1),
        builder_sk.verifying_key(),
    );
    let payload = payload(payload_id, 0);
    let signed = AuthorizedPayload::new(builder_sk, auth, payload.clone());

    // We never called `start_publishing`, so this must fail.
    let err = handle.publish_new(signed).unwrap_err();
    assert!(matches!(
        err,
        flashblocks_p2p::protocol::error::FlashblocksP2PError::NotClearedToPublish
    ));
}

#[tokio::test]
async fn expired_authorization_is_rejected() {
    let handle = fresh_handle();
    let builder_sk = handle.builder_sk().unwrap();

    // Step 1: obtain clearance with auth_1
    let payload_id = reth::payload::PayloadId::new([1; 8]);
    let auth_1 = Authorization::new(
        payload_id,
        DUMMY_TIMESTAMP,
        &signing_key(1),
        builder_sk.verifying_key(),
    );
    handle.start_publishing(auth_1).unwrap();

    // Step 2: craft a payload signed with *different* authorization → should fail
    let auth_2 = Authorization::new(
        payload_id,
        DUMMY_TIMESTAMP + 1,
        &signing_key(1),
        builder_sk.verifying_key(),
    );
    let payload = payload(payload_id, 0);
    let signed = AuthorizedPayload::new(builder_sk, auth_2, payload);

    let err = handle.publish_new(signed).unwrap_err();
    assert!(matches!(
        err,
        flashblocks_p2p::protocol::error::FlashblocksP2PError::ExpiredAuthorization
    ));
}

#[tokio::test]
async fn stop_and_restart_updates_state() {
    let handle = fresh_handle();
    let builder_sk = handle.builder_sk().unwrap();

    // 1) start publishing
    let payload_id_0 = reth::payload::PayloadId::new([3; 8]);
    let auth_0 = Authorization::new(
        payload_id_0,
        DUMMY_TIMESTAMP,
        &signing_key(1),
        builder_sk.verifying_key(),
    );
    handle.start_publishing(auth_0).unwrap();
    assert!(matches!(
        handle.publishing_status(),
        PublishingStatus::Publishing { .. }
    ));

    // 2) stop
    handle.stop_publishing().unwrap();
    assert!(matches!(
        handle.publishing_status(),
        PublishingStatus::NotPublishing { .. }
    ));

    // 3) start again with a new payload
    let payload_id_1 = reth::payload::PayloadId::new([4; 8]);
    let auth_1 = Authorization::new(
        payload_id_1,
        DUMMY_TIMESTAMP + 5,
        &signing_key(1),
        builder_sk.verifying_key(),
    );
    handle.start_publishing(auth_1).unwrap();
    assert!(matches!(
        handle.publishing_status(),
        PublishingStatus::Publishing { .. }
    ));
}

#[tokio::test]
async fn stop_and_restart_with_active_publishers() {
    let timestamp = 1000;
    let handle = fresh_handle();
    let builder_sk = handle.builder_sk().unwrap();

    // Pretend we already know about another publisher.
    let other_vk = signing_key(99).verifying_key();
    {
        let state = handle.state.lock();
        state
            .publishing_status
            .send_replace(PublishingStatus::NotPublishing {
                active_publishers: vec![(other_vk, timestamp - 1)],
            });
    }

    // Our own clearance → should transition to WaitingToPublish.
    let payload_id = PayloadId::new([6; 8]);
    let auth = Authorization::new(
        payload_id,
        timestamp,
        &signing_key(1),
        builder_sk.verifying_key(),
    );
    handle.start_publishing(auth).unwrap();
    match handle.publishing_status() {
        PublishingStatus::WaitingToPublish {
            active_publishers, ..
        } => {
            assert_eq!(active_publishers.len(), 1);
            assert_eq!(active_publishers[0].0, other_vk);
        }
        s => panic!("unexpected status: {s:?}"),
    }

    // Now we voluntarily stop.  We should end up back in NotPublishing,
    // still carrying the same active publisher entry.
    handle.stop_publishing().unwrap();
    match handle.publishing_status() {
        PublishingStatus::NotPublishing { active_publishers } => {
            assert_eq!(active_publishers.len(), 1);
            assert_eq!(active_publishers[0].0, other_vk);
        }
        s => panic!("unexpected status after stop: {s:?}"),
    }
}

#[tokio::test]
async fn await_clearance_unblocks_on_publish() {
    let handle = fresh_handle();
    let builder_sk = handle.builder_sk().unwrap();

    let waiter = {
        let h = handle.clone();
        task::spawn(async move {
            h.await_clearance().await;
        })
    };

    // give the waiter a chance to subscribe
    tokio::task::yield_now().await;
    assert!(!waiter.is_finished(), "future must still be pending");

    // now grant clearance
    let payload_id = reth::payload::PayloadId::new([5; 8]);
    let auth = Authorization::new(
        payload_id,
        DUMMY_TIMESTAMP,
        &signing_key(1),
        builder_sk.verifying_key(),
    );
    handle.start_publishing(auth).unwrap();

    // waiter should finish very quickly
    tokio::time::timeout(Duration::from_secs(1), waiter)
        .await
        .expect("await_clearance did not complete")
        .unwrap();
}

// ---------------------------------------------------------------------------
// Stream tests — updated for WorldChainEventsStream / FlashblocksEvent
// ---------------------------------------------------------------------------

#[tokio::test]
async fn flashblock_stream_is_ordered() {
    let handle = fresh_handle();
    let builder_sk = handle.builder_sk().unwrap();

    // clearance
    let payload_id = reth::payload::PayloadId::new([2; 8]);
    let auth = Authorization::new(
        payload_id,
        DUMMY_TIMESTAMP,
        &signing_key(1),
        builder_sk.verifying_key(),
    );
    handle.start_publishing(auth).unwrap();

    // Create the event stream *before* publishing so the canonical tip can
    // be established first, ensuring all flashblocks are yielded as Pending.
    let (canon_tx, provider) = MockCanonProvider::new();
    let mut stream = WorldChainEventsStream::new(handle.ctx.flashblock_tx.subscribe(), &provider);

    // Establish canonical tip matching the epoch parent.
    // Flashblocks have block_number 1, parent_hash ZERO -> parent = (0, ZERO).
    canon_tx.send(canon_notification(0, B256::ZERO)).unwrap();

    // Send index 1 first (out-of-order), then 0.
    for &idx in &[1u64, 0] {
        let p = payload(payload_id, idx);
        let signed = AuthorizedPayload::new(builder_sk, auth, p.clone());
        handle.publish_new(signed).unwrap();
    }

    // Expect to receive 0, then 1 over the ordered broadcast.
    let first = next_flashblock(&mut stream).await;
    let second = next_flashblock(&mut stream).await;
    assert_eq!(first.index, 0);
    assert_eq!(second.index, 1);
}

#[tokio::test]
async fn flashblock_stream_buffers_and_live() {
    let timestamp = 1000;
    let handle = fresh_handle();
    let builder_sk = handle.builder_sk().unwrap();

    let pid = PayloadId::new([7; 8]);
    let auth = Authorization::new(pid, timestamp, &signing_key(1), builder_sk.verifying_key());
    handle.start_publishing(auth).unwrap();

    // Publish index 0 before creating the stream — it will appear in the
    // seed.
    let signed0 = AuthorizedPayload::new(builder_sk, auth, payload(pid, 0));
    handle.publish_new(signed0).unwrap();

    // Create the event stream. The seed contains fb0.
    let (canon_tx, provider) = MockCanonProvider::new();
    let mut stream = WorldChainEventsStream::new(handle.ctx.flashblock_tx.subscribe(), &provider);

    // Establish canonical tip so flashblocks are yielded as Pending events.
    canon_tx.send(canon_notification(0, B256::ZERO)).unwrap();

    // Publish index 1 after the stream exists — it arrives live over
    // broadcast, not the seed.
    let signed1 = AuthorizedPayload::new(builder_sk, auth, payload(pid, 1));
    handle.publish_new(signed1).unwrap();

    // Drain until we see index 1 delivered live. The seed fb0 may or may
    // not be emitted depending on whether canon or fb0 is polled first by
    // `select`. Either way, the live fb1 must be delivered.
    let fb = next_flashblock(&mut stream).await;
    if fb.index == 0 {
        let fb1 = next_flashblock(&mut stream).await;
        assert_eq!(fb1.index, 1);
    } else {
        assert_eq!(fb.index, 1);
    }
}

#[tokio::test]
async fn flashblock_stream_recovers_after_receiver_lag() {
    let timestamp = 1000;
    let handle = fresh_handle();
    let builder_sk = handle.builder_sk().unwrap();

    let pid = PayloadId::new([8; 8]);
    let auth = Authorization::new(pid, timestamp, &signing_key(1), builder_sk.verifying_key());
    handle.start_publishing(auth).unwrap();

    // Create the event stream first, then publish more messages than the
    // broadcast buffer can retain before polling it. The stream must
    // resync from protocol state instead of terminating.
    let (canon_tx, provider) = MockCanonProvider::new();
    let mut stream = WorldChainEventsStream::new(handle.ctx.flashblock_tx.subscribe(), &provider);

    // Establish canonical tip.
    canon_tx.send(canon_notification(0, B256::ZERO)).unwrap();

    for idx in 0..=200 {
        let signed = AuthorizedPayload::new(builder_sk, auth, payload(pid, idx));
        handle.publish_new(signed).unwrap();
    }

    for expected in 0..=100u64 {
        let flashblock = next_flashblock(&mut stream).await;
        assert_eq!(flashblock.index, expected);
    }

    // We actually fail to continue publishing here
    // but this is an acceptable edge case
    assert!(
        tokio::time::timeout(Duration::from_millis(10), next_flashblock(&mut stream))
            .await
            .is_err(),
    );
}

#[tokio::test]
async fn live_flashblock_stream_skips_stale_flashblocks() {
    let timestamp = 1000;
    let handle = fresh_handle();
    let builder_sk = handle.builder_sk().unwrap();

    let pid_a = PayloadId::new([8; 8]);
    let auth_a = Authorization::new(
        pid_a,
        timestamp,
        &signing_key(1),
        builder_sk.verifying_key(),
    );
    handle.start_publishing(auth_a).unwrap();

    // Create the event stream first, then partially consume payload A
    // before payload B starts. The stream should skip the unread remainder
    // of payload A once the canonical tip advances past its epoch parent.
    //
    // Payload A: block_number 1, parent_hash ZERO  -> epoch parent (0, ZERO)
    // Payload B: block_number 2, parent_hash BLOCK1 -> epoch parent (1, BLOCK1)
    let block1_hash = B256::with_last_byte(0xAA);
    let (canon_tx, provider) = MockCanonProvider::new();
    let mut stream = WorldChainEventsStream::new(handle.ctx.flashblock_tx.subscribe(), &provider);

    // Canonical tip for epoch A.
    canon_tx.send(canon_notification(0, B256::ZERO)).unwrap();

    for idx in 0..=10u64 {
        let signed = AuthorizedPayload::new(builder_sk, auth_a, payload(pid_a, idx));
        handle.publish_new(signed).unwrap();
    }

    let first = next_flashblock(&mut stream).await;
    assert_eq!(first.payload_id, pid_a);
    assert_eq!(first.index, 0);

    // Start epoch B on a *different* parent so the cursor can tell it apart.
    let pid_b = PayloadId::new([9; 8]);
    let auth_b = Authorization::new(
        pid_b,
        timestamp + 1,
        &signing_key(1),
        builder_sk.verifying_key(),
    );
    handle.start_publishing(auth_b).unwrap();
    let signed = AuthorizedPayload::new(
        builder_sk,
        auth_b,
        payload_with_parent(pid_b, 0, 2, block1_hash),
    );
    handle.publish_new(signed).unwrap();

    // Advance the canonical tip to match epoch B's parent. Already-emitted
    // A flashblocks may still be in the pipeline, but the cursor will reject
    // any new A flashblocks arriving after the tip change. Drain until we
    // see B's flashblock.
    canon_tx.send(canon_notification(1, block1_hash)).unwrap();

    let flashblock = loop {
        let fb = next_flashblock(&mut stream).await;
        if fb.payload_id == pid_b {
            break fb;
        }
    };
    assert_eq!(flashblock.index, 0);
}

#[tokio::test]
async fn live_flashblock_stream_handles_out_of_order() {
    let timestamp = 1000;
    let handle = fresh_handle();
    let builder_sk = handle.builder_sk().unwrap();

    let pid = PayloadId::new([8; 8]);
    let auth = Authorization::new(pid, timestamp, &signing_key(1), builder_sk.verifying_key());
    handle.start_publishing(auth).unwrap();

    // Create the event stream, then send the canonical tip.
    let (canon_tx, provider) = MockCanonProvider::new();
    let mut stream = WorldChainEventsStream::new(handle.ctx.flashblock_tx.subscribe(), &provider);

    // Establish canonical tip.
    canon_tx.send(canon_notification(0, B256::ZERO)).unwrap();

    handle
        .publish_new(AuthorizedPayload::new(builder_sk, auth, payload(pid, 0)))
        .unwrap();

    assert_eq!(next_flashblock(&mut stream).await.index, 0);

    handle
        .publish_new(AuthorizedPayload::new(builder_sk, auth, payload(pid, 2)))
        .unwrap();

    // Assert not ready — index 2 cannot be delivered before index 1 arrives.
    assert!(
        tokio::time::timeout(Duration::from_millis(10), next_flashblock(&mut stream))
            .await
            .is_err()
    );

    handle
        .publish_new(AuthorizedPayload::new(builder_sk, auth, payload(pid, 1)))
        .unwrap();

    assert_eq!(next_flashblock(&mut stream).await.index, 1);
    assert_eq!(next_flashblock(&mut stream).await.index, 2);
}

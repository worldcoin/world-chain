use super::*;
use crate::protocol::connection::FlashblocksPeerState;
use alloy_primitives::Bytes;
use alloy_rlp::{BytesMut, Decodable};
use std::{sync::Arc, time::Duration};
use tokio::sync::{mpsc, oneshot};
use world_chain_primitives::{p2p::AuthorizedPayload, primitives::FlashblocksPayloadV1};

#[allow(
    dead_code,
    reason = "the shared harness supplies multiple independent signer roles"
)]
#[path = "../../../../signer/tests/common/runtime.rs"]
mod runtime;

#[test]
fn complete_frame_preflight_rejects_limit_plus_one_and_length_overflow() {
    let auth = 111;
    let largest = (MAX_FRAME - 2048..MAX_FRAME)
        .rfind(|&n| validate_frame_lengths(n, auth).is_ok())
        .expect("message fits");
    assert_eq!(validate_frame_lengths(largest, auth), Ok(()));
    assert_eq!(
        validate_frame_lengths(largest + 1, auth),
        Err(FlashblocksP2PError::FrameTooLarge)
    );
    assert_eq!(
        validate_frame_lengths(usize::MAX, auth),
        Err(FlashblocksP2PError::FrameTooLarge)
    );
}

#[tokio::test]
async fn local_outdated_publication_retains_its_noop_result() {
    let handle = handle();
    let current = authorization(11);
    handle.start_publishing(current).expect("local clearance");
    handle
        .publish_new(AuthorizedPayload::new(
            handle.builder_sk().expect("key"),
            current,
            FlashblocksPayloadV1 {
                payload_id: current.payload_id,
                ..Default::default()
            },
        ))
        .expect("current payload");
    let older = authorization(10);
    handle
        .state
        .lock()
        .publishing_status
        .send_replace(PublishingStatus::Publishing {
            authorization: older,
        });
    handle
        .publish_new(AuthorizedPayload::new(
            handle.builder_sk().expect("key"),
            older,
            FlashblocksPayloadV1 {
                payload_id: older.payload_id,
                ..Default::default()
            },
        ))
        .expect("legacy local stale publication remains a no-op");
    let state = handle.state.lock();
    assert_eq!(state.payload_id, current.payload_id);
    assert_eq!(state.payload_timestamp, current.timestamp);
}

#[tokio::test]
async fn expired_results_cannot_grant_clearance_or_commit_a_payload() {
    let handle = handle();
    let auth = authorization(10);
    let mut pending = handle
        .prepare_start_publishing(auth)
        .expect("prepare")
        .expect("start");
    let signed = pending.sign_local(handle.builder_sk().expect("test key"));
    pending.deadline = Some(tokio::time::Instant::now());
    assert_eq!(
        handle.complete_control_message(pending, signed),
        Err(FlashblocksP2PError::Signing(
            world_chain_signer::Error::Deadline
        ))
    );
    assert!(matches!(
        handle.publishing_status(),
        PublishingStatus::NotPublishing { .. }
    ));
    handle.start_publishing(auth).expect("local start");
    let lease = PublishingLease {
        handle: handle.clone(),
        scope: handle.state.lock().pending_start.clone(),
        deadline: tokio::time::Instant::now(),
    };
    let payload = AuthorizedPayload::new(
        handle.builder_sk().expect("key"),
        auth,
        FlashblocksPayloadV1 {
            payload_id: auth.payload_id,
            ..Default::default()
        },
    );
    let mut committed = false;
    let result: Result<(), FlashblocksP2PError> =
        handle.publish_new_with_lease(payload, &lease, || {
            committed = true;
            Ok(())
        });
    assert_eq!(
        result,
        Err(FlashblocksP2PError::Signing(
            world_chain_signer::Error::Deadline
        ))
    );
    assert!(!committed);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires the reviewed disposable mTLS signer harness"]
async fn remote_controls_payload_denial_and_inflight_stop_preserve_publishing_boundaries() {
    use crate::protocol::handler::Publisher;
    use world_chain_signer::PublicationSigner;
    let harness = runtime::Harness::start().await;
    let fixture: serde_json::Value = serde_json::from_str(include_str!(
        "../../../../primitives/testdata/flashblock-signing-v1.json"
    ))
    .expect("frozen corpus");
    let auth: Bytes = fixture["authorizationRlp"]
        .as_str()
        .expect("RLP")
        .parse()
        .expect("hex");
    let auth = Authorization::decode(&mut auth.as_ref()).expect("grant");
    let client = PublicationSigner::new(harness.ed25519(false, "publisher")).expect("publisher");
    let handle = FlashblocksHandle::with_publisher(
        SigningKey::from_bytes(&[7; 32]).verifying_key(),
        Some(Publisher::Remote(client)),
        Default::default(),
        None,
    );
    let mut rx = peer(&handle);
    let lease = handle
        .start_publishing_until(auth, runtime::deadline())
        .expect("admitted start");
    tokio::time::timeout_at(runtime::deadline(), handle.await_authorized_clearance(auth))
        .await
        .expect("bounded clearance")
        .expect("verified remote start");
    let start: Bytes = fixture["messages"][1]["wire"]
        .as_str()
        .expect("start wire")
        .parse()
        .expect("hex");
    assert_eq!(
        rx.recv().await.expect("start frame").as_ref(),
        start.as_ref()
    );
    let message: Bytes = fixture["messages"][0]["messageRlp"]
        .as_str()
        .expect("message")
        .parse()
        .expect("hex");
    let AuthorizedMsg::FlashblocksPayloadV1(payload) =
        AuthorizedMsg::decode(&mut message.as_ref()).expect("payload")
    else {
        panic!("payload variant")
    };
    let authorized = handle
        .authorize_payload(payload, auth, runtime::deadline())
        .await
        .expect("remote payload signature");
    let expected: Bytes = fixture["messages"][0]["wire"]
        .as_str()
        .expect("payload wire")
        .parse()
        .expect("hex");
    assert_eq!(
        FlashblocksP2PMsg::Authorized(authorized.authorized.clone())
            .encode()
            .as_ref(),
        expected.as_ref()
    );
    handle
        .publish_new_with_lease(authorized.clone(), &lease, || {
            Ok::<_, FlashblocksP2PError>(())
        })
        .expect("active publishing session");
    handle.state.lock().payload_timestamp = auth.timestamp + 1;
    let mut stale_committed = false;
    let stale: Result<(), FlashblocksP2PError> =
        handle.publish_new_with_lease(authorized.clone(), &lease, || {
            stale_committed = true;
            Ok(())
        });
    assert_eq!(stale, Err(FlashblocksP2PError::StaleControlMessage));
    assert!(
        !stale_committed,
        "newer cached payload fences remote results"
    );
    handle.state.lock().payload_timestamp = auth.timestamp;
    handle.stop_publishing().expect("fence before stop I/O");
    let mut committed = false;
    let result: Result<(), FlashblocksP2PError> =
        handle.publish_new_with_commit(authorized, || {
            committed = true;
            Ok(())
        });
    assert_eq!(result, Err(FlashblocksP2PError::NotClearedToPublish));
    assert!(!committed);
    let stop: Bytes = fixture["messages"][2]["wire"]
        .as_str()
        .expect("stop wire")
        .parse()
        .expect("hex");
    assert_eq!(
        tokio::time::timeout_at(runtime::deadline(), rx.recv())
            .await
            .expect("bounded stop")
            .expect("stop frame")
            .as_ref(),
        stop.as_ref()
    );
    drop(lease);

    let a = handle
        .start_publishing_until(authorization(11), runtime::deadline())
        .expect("next job");
    tokio::time::timeout_at(
        runtime::deadline(),
        handle.await_authorized_clearance(authorization(11)),
    )
    .await
    .expect("handover deadline")
    .expect("next job cleared");
    let old_result = handle
        .authorize_payload(
            FlashblocksPayloadV1 {
                payload_id: authorization(11).payload_id,
                ..Default::default()
            },
            authorization(11),
            runtime::deadline(),
        )
        .await
        .expect("signed before intent changed");
    let b = handle
        .start_publishing_until(authorization(11), runtime::deadline())
        .expect("newer publishing generation");
    let mut stale_committed = false;
    let stale: Result<(), FlashblocksP2PError> =
        handle.publish_new_with_lease(old_result, &a, || {
            stale_committed = true;
            Ok(())
        });
    assert_eq!(stale, Err(FlashblocksP2PError::StaleControlMessage));
    assert!(
        !stale_committed,
        "same grant reused by a newer intent must reject the old result"
    );
    drop(a);
    assert_publishing(&handle, authorization(11));
    drop(b);
    let c = handle
        .start_publishing_until(authorization(13), runtime::deadline())
        .expect("one pending handover while cancelled I/O drains");
    tokio::time::timeout_at(
        runtime::deadline(),
        handle.await_authorized_clearance(authorization(13)),
    )
    .await
    .expect("bounded cancellation drain")
    .expect("latest job cleared without retry");
    drop(c);
    loop {
        let frame = tokio::time::timeout_at(runtime::deadline(), rx.recv())
            .await
            .expect("final stop delivered")
            .expect("control frame");
        if let FlashblocksP2PMsg::Authorized(signed) =
            FlashblocksP2PMsg::decode(&mut frame.as_ref()).expect("control RLP")
            && matches!(signed.msg, AuthorizedMsg::StopPublish(_))
            && signed.authorization == authorization(13)
        {
            break;
        }
    }

    let mut denied = harness.ed25519(false, "publisher");
    denied.key_id = alloy_primitives::B256::repeat_byte(0xff);
    let denied = FlashblocksHandle::with_publisher(
        SigningKey::from_bytes(&[7; 32]).verifying_key(),
        Some(Publisher::Remote(
            PublicationSigner::new(denied).expect("denied identity"),
        )),
        Default::default(),
        None,
    );
    let mut denied_rx = peer(&denied);
    let denied_lease = denied
        .start_publishing_until(auth, runtime::deadline())
        .expect("start admitted before remote decision");
    assert!(matches!(
        tokio::time::timeout_at(runtime::deadline(), denied.await_authorized_clearance(auth))
            .await
            .expect("denial must reach caller"),
        Err(FlashblocksP2PError::Signing(
            world_chain_signer::Error::Remote { code: -32001, .. }
        ))
    ));
    assert!(matches!(
        denied.publishing_status(),
        PublishingStatus::NotPublishing { .. }
    ));
    assert!(denied_rx.try_recv().is_err());
    drop(denied_lease);
    in_flight_control_is_cancelled_on_stop(&harness, auth).await;
}

async fn in_flight_control_is_cancelled_on_stop(harness: &runtime::Harness, auth: Authorization) {
    use std::sync::atomic::{AtomicBool, Ordering};
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::{TcpListener, TcpStream},
        sync::Notify,
    };
    let mut config = harness.ed25519(false, "publisher");
    let upstream = (
        config
            .connection
            .endpoint
            .host_str()
            .expect("test host")
            .to_owned(),
        config.connection.endpoint.port().expect("test port"),
    );
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("test TLS relay");
    config.connection.endpoint =
        format!("https://{}/", listener.local_addr().expect("relay address"))
            .parse()
            .expect("loopback URL");
    let paused = Arc::new(AtomicBool::new(false));
    let held = Arc::new(Notify::new());
    let resume = Arc::new(Notify::new());
    let (pause, seen, release) = (paused.clone(), held.clone(), resume.clone());
    let relay = tokio::spawn(async move {
        let (incoming, _) = tokio::time::timeout(Duration::from_secs(3), listener.accept())
            .await
            .expect("bounded relay accept")
            .expect("relay client");
        let outgoing = TcpStream::connect(upstream).await.expect("loopback signer");
        let (mut source, mut sink) = incoming.into_split();
        let (mut responses, mut requests) = outgoing.into_split();
        let forward = tokio::io::copy(&mut source, &mut requests);
        let reverse = async {
            let mut bytes = [0; 4096];
            loop {
                let n = responses.read(&mut bytes).await?;
                if n == 0 {
                    return Ok::<(), std::io::Error>(());
                }
                if pause.load(Ordering::Acquire) {
                    seen.notify_one();
                    release.notified().await;
                }
                sink.write_all(&bytes[..n]).await?;
            }
        };
        tokio::select! { result=forward=>{result.expect("caller cancellation closes relay");}, result=reverse=>{result.expect("TLS relay");} }
    });
    let client =
        world_chain_signer::PublicationSigner::new(config).expect("proxied mTLS publisher");
    client
        .health(runtime::deadline())
        .await
        .expect("authenticate before pausing output");
    paused.store(true, Ordering::Release);
    let handle = FlashblocksHandle::with_publisher(
        SigningKey::from_bytes(&[7; 32]).verifying_key(),
        Some(Publisher::Remote(client)),
        Default::default(),
        None,
    );
    let mut rx = peer(&handle);
    let lease = handle
        .start_publishing_until(auth, runtime::deadline())
        .expect("pending start");
    tokio::time::timeout_at(runtime::deadline(), held.notified())
        .await
        .expect("real signed response held before delivery");
    handle
        .stop_publishing()
        .expect("stop does not wait for remote response");
    assert!(matches!(
        handle.publishing_status(),
        PublishingStatus::NotPublishing { .. }
    ));
    tokio::time::timeout_at(runtime::deadline(), relay)
        .await
        .expect("cancelled remote connection terminates")
        .expect("relay task");
    assert!(
        rx.try_recv().is_err(),
        "late signed start must not be broadcast"
    );
    drop(lease);
}

fn handle() -> FlashblocksHandle {
    FlashblocksHandle::new(
        SigningKey::from_bytes(&[7; 32]).verifying_key(),
        Some(SigningKey::from_bytes(&[8; 32])),
    )
}

fn authorization(timestamp: u64) -> Authorization {
    peer_authorization(timestamp, 8)
}

fn peer_authorization(timestamp: u64, key: u8) -> Authorization {
    Authorization::new(
        PayloadId::new(timestamp.to_be_bytes()),
        timestamp,
        &SigningKey::from_bytes(&[7; 32]),
        SigningKey::from_bytes(&[key; 32]).verifying_key(),
    )
}

fn peer(handle: &FlashblocksHandle) -> mpsc::Receiver<BytesMut> {
    let peer_id = PeerId::random();
    let mut state = FlashblocksPeerState::new(peer_id);
    let (tx, rx) = mpsc::channel(8);
    state.outbound_tx = Some(tx);
    handle.state.lock().peers.insert(peer_id, state);
    rx
}

fn complete(
    handle: &FlashblocksHandle,
    pending: PendingControlMessage,
) -> Result<(), FlashblocksP2PError> {
    let signed = pending.sign_local(handle.builder_sk().expect("public test signer"));
    handle.complete_control_message(pending, signed)
}

fn assert_publishing(handle: &FlashblocksHandle, expected: Authorization) {
    assert!(matches!(
        handle.publishing_status(),
        PublishingStatus::Publishing { authorization } if authorization == expected
    ));
}

#[tokio::test]
async fn controls_preserve_frozen_wire_vectors() {
    let fixture: serde_json::Value = serde_json::from_str(include_str!(
        "../../../../primitives/testdata/flashblock-signing-v1.json"
    ))
    .expect("public Flashblock fixture");
    let bytes: Bytes = fixture["authorizationRlp"]
        .as_str()
        .expect("authorization RLP")
        .parse()
        .expect("hex bytes");
    let authorization = Authorization::decode(&mut bytes.as_ref()).expect("authorization");
    let handle = handle();
    let mut rx = peer(&handle);
    handle.start_publishing(authorization).expect("start");
    handle.stop_publishing().expect("stop");
    for message in &fixture["messages"].as_array().expect("messages")[1..] {
        let expected: Bytes = message["wire"]
            .as_str()
            .expect("wire hex")
            .parse()
            .expect("wire bytes");
        assert_eq!(
            rx.try_recv().expect("control frame").as_ref(),
            expected.as_ref()
        );
    }
    assert!(rx.try_recv().is_err());
}

#[tokio::test]
async fn delayed_start_cannot_beat_a_newer_pending_request() {
    let handle = handle();
    let mut rx = peer(&handle);
    let old = handle
        .prepare_start_publishing(authorization(1))
        .expect("old request")
        .expect("start notification");
    let (release, released) = oneshot::channel();
    let delayed_handle = handle.clone();
    let delayed = tokio::spawn(async move {
        released.await.expect("release delayed signer");
        complete(&delayed_handle, old)
    });

    assert!(
        handle.state.try_lock().is_some(),
        "signing holds no state lock"
    );
    let current = handle
        .prepare_start_publishing(authorization(2))
        .expect("new request")
        .expect("start notification");
    release.send(()).expect("release old result");
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(2), delayed)
            .await
            .expect("bounded delayed signing")
            .expect("signing task"),
        Err(FlashblocksP2PError::StaleControlMessage)
    );
    assert!(rx.try_recv().is_err());
    assert!(matches!(
        handle.publishing_status(),
        PublishingStatus::NotPublishing { .. }
    ));
    complete(&handle, current).expect("current result");
    assert_publishing(&handle, authorization(2));
    assert!(rx.try_recv().is_ok());
    assert!(rx.try_recv().is_err());
}

#[tokio::test]
async fn an_older_grant_does_not_cancel_a_newer_pending_start() {
    let handle = handle();
    let pending = handle
        .prepare_start_publishing(authorization(2))
        .expect("newer grant")
        .expect("notification");
    handle
        .start_publishing(authorization(1))
        .expect("older grant is ignored");
    complete(&handle, pending).expect("newer grant remains valid");
    assert_publishing(&handle, authorization(2));
}

#[tokio::test]
async fn rejected_grants_do_not_fence_the_active_publishing_lease() {
    let handle = handle();
    let current = authorization(10);
    let lease = handle
        .start_publishing_until(current, runtime::deadline())
        .expect("current intent");
    let conflicting = Authorization::new(
        PayloadId::new([99; 8]),
        current.timestamp,
        &SigningKey::from_bytes(&[7; 32]),
        SigningKey::from_bytes(&[8; 32]).verifying_key(),
    );
    for rejected in [authorization(9), conflicting] {
        assert!(matches!(
            handle.start_publishing_until(rejected, runtime::deadline()),
            Err(FlashblocksP2PError::StaleControlMessage)
        ));
        assert!(!lease.scope.is_cancelled());
        assert_eq!(lease.scope, handle.state.lock().pending_start);
        assert_publishing(&handle, current);
    }
    assert!(matches!(
        handle.start_publishing_until(authorization(11), tokio::time::Instant::now()),
        Err(FlashblocksP2PError::Signing(
            world_chain_signer::Error::Deadline
        ))
    ));
    assert!(!lease.scope.is_cancelled());
    assert_eq!(lease.scope, handle.state.lock().pending_start);
    assert_publishing(&handle, current);
}

#[tokio::test]
async fn stop_cancels_pending_start_even_after_the_same_authorization_restarts() {
    let handle = handle();
    let pending = handle
        .prepare_start_publishing(authorization(1))
        .expect("request")
        .expect("notification");
    handle.stop_publishing().expect("cancel before clearance");
    handle
        .start_publishing(authorization(1))
        .expect("fresh request with same grant");
    assert_eq!(
        complete(&handle, pending),
        Err(FlashblocksP2PError::StaleControlMessage)
    );
    assert_publishing(&handle, authorization(1));
}

#[tokio::test]
async fn pending_stop_revokes_clearance_and_cannot_stop_a_new_session() {
    let handle = handle();
    let mut rx = peer(&handle);
    let old = authorization(1);
    handle.start_publishing(old).expect("start");
    rx.try_recv().expect("initial start frame");
    let pending = handle
        .prepare_stop_publishing()
        .expect("stop")
        .expect("notification");
    let payload = FlashblocksPayloadV1 {
        payload_id: old.payload_id,
        ..Default::default()
    };
    let signed = AuthorizedPayload::new(handle.builder_sk().expect("signer"), old, payload);
    assert_eq!(
        handle.publish_new(signed),
        Err(FlashblocksP2PError::NotClearedToPublish)
    );
    handle
        .start_publishing(authorization(2))
        .expect("new session");
    rx.try_recv().expect("new start frame");
    assert_eq!(
        complete(&handle, pending),
        Err(FlashblocksP2PError::StaleControlMessage)
    );
    assert_publishing(&handle, authorization(2));
    assert!(rx.try_recv().is_err(), "old stop must not be broadcast");
}

#[tokio::test]
async fn handover_fences_publication_before_signing_its_stop() {
    let handle = handle();
    let mut rx = peer(&handle);
    let ours = authorization(1);
    let theirs = peer_authorization(2, 9);
    handle.start_publishing(ours).expect("start");
    rx.try_recv().expect("start frame");
    let pending = handle
        .prepare_peer_start(PeerId::random(), theirs)
        .expect("authenticated peer start")
        .expect("stop notification");
    assert!(matches!(
        handle.publishing_status(),
        PublishingStatus::NotPublishing { active_publishers }
            if active_publishers == vec![(theirs.builder_vk, theirs.timestamp)]
    ));
    assert!(rx.try_recv().is_err(), "no frame before signing");
    complete(&handle, pending).expect("stop result");
    let expected = FlashblocksP2PMsg::Authorized(Authorized::new(
        handle.builder_sk().expect("signer"),
        ours,
        StopPublish.into(),
    ))
    .encode();
    assert_eq!(rx.try_recv().expect("stop frame"), expected);
}

#[tokio::test]
async fn stale_handover_result_cannot_overwrite_a_new_waiting_session() {
    let handle = handle();
    handle.start_publishing(authorization(1)).expect("start");
    let pending = handle
        .prepare_peer_start(PeerId::random(), peer_authorization(2, 9))
        .expect("peer start")
        .expect("stop notification");
    let current = authorization(3);
    handle.start_publishing(current).expect("new request");
    assert_eq!(
        complete(&handle, pending),
        Err(FlashblocksP2PError::StaleControlMessage)
    );
    assert!(
        matches!(handle.publishing_status(), PublishingStatus::WaitingToPublish { authorization, .. } if authorization == current)
    );
}

#[tokio::test]
async fn repeated_stop_does_not_cancel_its_pending_notification() {
    let handle = handle();
    let mut rx = peer(&handle);
    handle.start_publishing(authorization(1)).expect("start");
    rx.try_recv().expect("start frame");
    let pending = handle
        .prepare_stop_publishing()
        .expect("stop")
        .expect("notification");
    handle.stop_publishing().expect("already stopped");
    complete(&handle, pending).expect("original stop remains valid");
    assert!(rx.try_recv().is_ok());
    assert!(rx.try_recv().is_err());
    assert!(matches!(
        handle.publishing_status(),
        PublishingStatus::NotPublishing { .. }
    ));
}

#[tokio::test]
async fn repeated_peer_start_does_not_cancel_the_handover_stop() {
    let handle = handle();
    handle.start_publishing(authorization(1)).expect("start");
    let theirs = peer_authorization(2, 9);
    let pending = handle
        .prepare_peer_start(PeerId::random(), theirs)
        .expect("peer start")
        .expect("notification");
    handle
        .peer_start_publishing(PeerId::random(), theirs)
        .expect("repeated peer start");
    complete(&handle, pending).expect("handover stop remains valid");
    assert!(
        matches!(handle.publishing_status(), PublishingStatus::NotPublishing { active_publishers } if active_publishers == vec![(theirs.builder_vk, theirs.timestamp)])
    );
}

#[tokio::test]
async fn older_peer_start_does_not_cancel_a_newer_pending_local_start() {
    let handle = handle();
    let mut rx = peer(&handle);
    let current = authorization(12);
    let pending = handle
        .prepare_start_publishing(current)
        .expect("newer local grant")
        .expect("start notification");
    let replay = FlashblocksP2PMsg::Authorized(Authorized::new(
        &SigningKey::from_bytes(&[9; 32]),
        peer_authorization(10, 9),
        StartPublish.into(),
    ))
    .encode();
    let world_chain_primitives::p2p::FlashblocksP2PMsgParts::Authorized(parts) =
        FlashblocksP2PMsg::parse(&replay).expect("original signed frame")
    else {
        panic!("authorized start");
    };
    parts
        .verify(SigningKey::from_bytes(&[7; 32]).verifying_key())
        .expect("replay retains valid authorizer and publisher signatures");
    let replay = parts.into_authorized().expect("start body").authorization;
    assert_eq!(handle.state.lock().payload_timestamp, 0);
    assert_eq!(
        handle.peer_start_publishing(PeerId::random(), replay),
        Err(FlashblocksP2PError::ExpiredAuthorization)
    );
    assert!(!pending.cancelled.is_cancelled());
    {
        let state = handle.state.lock();
        assert_eq!(state.pending_start_timestamp, Some(current.timestamp));
        assert!(state.publishing_error.borrow().is_none());
        assert!(matches!(
            &*state.publishing_status.borrow(),
            PublishingStatus::NotPublishing { active_publishers } if active_publishers.is_empty()
        ));
    }
    assert!(rx.try_recv().is_err());
    complete(&handle, pending).expect("newer local grant still completes");
    assert_publishing(&handle, current);
    assert!(rx.try_recv().is_ok());
    assert!(rx.try_recv().is_err());
}

#[tokio::test]
async fn older_peer_start_preserves_publishing_and_waiting_local_grants() {
    for waiting in [false, true] {
        let handle = handle();
        let mut rx = peer(&handle);
        let previous = peer_authorization(11, 9);
        if waiting {
            handle
                .peer_start_publishing(PeerId::random(), previous)
                .expect("previous publisher");
        }
        let current = authorization(12);
        handle.start_publishing(current).expect("newer local grant");
        rx.try_recv().expect("local start frame");
        let generation = handle.state.lock().pending_start.clone();
        assert_eq!(
            handle.peer_start_publishing(PeerId::random(), peer_authorization(10, 10)),
            Err(FlashblocksP2PError::ExpiredAuthorization)
        );
        assert!(!generation.is_cancelled());
        let state = handle.state.lock();
        assert_eq!(state.payload_timestamp, 0);
        assert_eq!(state.pending_start, generation);
        assert!(state.publishing_error.borrow().is_none());
        match &*state.publishing_status.borrow() {
            PublishingStatus::WaitingToPublish {
                authorization,
                active_publishers,
            } if waiting => {
                assert_eq!(*authorization, current);
                assert_eq!(active_publishers, &vec![(previous.builder_vk, 11)]);
            }
            PublishingStatus::Publishing { authorization } if !waiting => {
                assert_eq!(*authorization, current);
            }
            status => panic!("local publishing intent changed: {status:?}"),
        }
        assert!(rx.try_recv().is_err(), "rejected replay emits no stop");
    }
}

#[tokio::test]
async fn same_timestamp_and_newer_peer_starts_still_handover() {
    for timestamp in [12, 13] {
        for pending_start in [false, true] {
            let handle = handle();
            let current = authorization(12);
            let pending = if pending_start {
                handle
                    .prepare_start_publishing(current)
                    .expect("local request")
            } else {
                handle.start_publishing(current).expect("local publisher");
                None
            };
            let generation = handle.state.lock().pending_start.clone();
            let theirs = peer_authorization(timestamp, 9);
            handle
                .peer_start_publishing(PeerId::random(), theirs)
                .expect("legitimate handover");
            assert!(generation.is_cancelled());
            assert!(matches!(
                handle.publishing_status(),
                PublishingStatus::NotPublishing { active_publishers }
                    if active_publishers == vec![(theirs.builder_vk, timestamp)]
            ));
            if let Some(pending) = pending {
                assert_eq!(
                    complete(&handle, pending),
                    Err(FlashblocksP2PError::StaleControlMessage)
                );
            }
        }
    }
}

#[tokio::test]
async fn peer_start_cancels_a_delayed_local_start() {
    let handle = handle();
    let pending = handle
        .prepare_start_publishing(authorization(1))
        .expect("request")
        .expect("notification");
    let theirs = peer_authorization(2, 9);
    handle
        .peer_start_publishing(PeerId::random(), theirs)
        .expect("peer start");
    assert_eq!(
        complete(&handle, pending),
        Err(FlashblocksP2PError::StaleControlMessage)
    );
    assert!(
        matches!(handle.publishing_status(), PublishingStatus::NotPublishing { active_publishers } if active_publishers == vec![(theirs.builder_vk, theirs.timestamp)])
    );
}

#[tokio::test]
async fn completion_uses_the_current_active_publisher_list() {
    let handle = handle();
    let theirs = peer_authorization(1, 9);
    handle
        .peer_start_publishing(PeerId::random(), theirs)
        .expect("peer start");
    let pending = handle
        .prepare_start_publishing(authorization(2))
        .expect("request")
        .expect("notification");
    handle.state.lock().publishing_status.send_modify(|status| {
        if let PublishingStatus::NotPublishing { active_publishers } = status {
            active_publishers.clear();
        }
    });
    complete(&handle, pending).expect("peer stopped during signing");
    assert_publishing(&handle, authorization(2));
}

#[tokio::test]
async fn invalid_control_results_never_grant_clearance() {
    let handle = handle();
    let mut rx = peer(&handle);
    for case in 0..4 {
        let pending = handle
            .prepare_start_publishing(authorization(1))
            .expect("request")
            .expect("notification");
        let mut signed = pending.sign_local(handle.builder_sk().expect("signer"));
        match case {
            0 => signed = pending.sign_local(&SigningKey::from_bytes(&[9; 32])),
            1 => signed.msg = StopPublish.into(),
            2 => signed.authorization = authorization(2),
            _ => signed.actor_sig = ed25519_dalek::Signature::from_bytes(&[0; 64]),
        }
        assert_eq!(
            handle.complete_control_message(pending, signed),
            Err(FlashblocksP2PError::InvalidControlMessage)
        );
        assert!(matches!(
            handle.publishing_status(),
            PublishingStatus::NotPublishing { .. }
        ));
        assert!(rx.try_recv().is_err());
    }
}

#[tokio::test]
async fn invalid_stop_result_does_not_restore_clearance() {
    let handle = handle();
    let mut rx = peer(&handle);
    handle.start_publishing(authorization(1)).expect("start");
    rx.try_recv().expect("start frame");
    let pending = handle
        .prepare_stop_publishing()
        .expect("stop request")
        .expect("notification");
    let signed = pending.sign_local(&SigningKey::from_bytes(&[9; 32]));
    assert_eq!(
        handle.complete_control_message(pending, signed),
        Err(FlashblocksP2PError::InvalidControlMessage)
    );
    assert!(matches!(
        handle.publishing_status(),
        PublishingStatus::NotPublishing { .. }
    ));
    assert!(rx.try_recv().is_err());
}

#[tokio::test]
async fn payload_changes_discard_control_results() {
    for change_timestamp in [false, true] {
        let handle = handle();
        let pending = handle
            .prepare_start_publishing(authorization(1))
            .expect("request")
            .expect("notification");
        {
            let mut state = handle.state.lock();
            if change_timestamp {
                state.payload_timestamp += 1;
            } else {
                state.payload_id = PayloadId::new([3; 8]);
            }
        }
        assert_eq!(
            complete(&handle, pending),
            Err(FlashblocksP2PError::StaleControlMessage)
        );
        assert!(matches!(
            handle.publishing_status(),
            PublishingStatus::NotPublishing { .. }
        ));
    }
}

#[tokio::test]
async fn arrival_of_the_requested_payload_does_not_discard_its_control() {
    let handle = handle();
    let requested = authorization(1);
    let pending = handle
        .prepare_start_publishing(requested)
        .expect("request")
        .expect("notification");
    {
        let mut state = handle.state.lock();
        state.payload_id = requested.payload_id;
        state.payload_timestamp = requested.timestamp;
    }
    complete(&handle, pending).expect("requested payload arrived during signing");
    assert_publishing(&handle, requested);
}

#[tokio::test]
async fn abandoning_a_request_never_grants_or_restores_clearance() {
    let handle = handle();
    drop(
        handle
            .prepare_start_publishing(authorization(1))
            .expect("request"),
    );
    assert!(matches!(
        handle.publishing_status(),
        PublishingStatus::NotPublishing { .. }
    ));
    handle.start_publishing(authorization(1)).expect("start");
    drop(handle.prepare_stop_publishing().expect("stop"));
    assert!(matches!(
        handle.publishing_status(),
        PublishingStatus::NotPublishing { .. }
    ));
}

#[tokio::test]
async fn invalid_grant_does_not_cancel_a_valid_request() {
    let handle = handle();
    let pending = handle
        .prepare_start_publishing(authorization(1))
        .expect("request")
        .expect("notification");
    assert_eq!(
        handle.start_publishing(peer_authorization(2, 9)),
        Err(FlashblocksP2PError::InvalidControlMessage)
    );
    let mut wrong_authorizer = authorization(2);
    wrong_authorizer.authorizer_sig = peer_authorization(3, 9).authorizer_sig;
    assert_eq!(
        handle.start_publishing(wrong_authorizer),
        Err(FlashblocksP2PError::InvalidControlMessage)
    );
    complete(&handle, pending).expect("valid request survives");
    assert_publishing(&handle, authorization(1));
}

#[tokio::test]
async fn waiting_retains_the_existing_two_second_failover() {
    let handle = handle();
    handle
        .peer_start_publishing(PeerId::random(), peer_authorization(10, 9))
        .expect("peer start");
    for timestamp in [10, 11] {
        handle
            .start_publishing(authorization(timestamp))
            .expect("waiting request");
        assert!(matches!(
            handle.publishing_status(),
            PublishingStatus::WaitingToPublish { .. }
        ));
    }
    handle
        .start_publishing(authorization(12))
        .expect("failover");
    assert_publishing(&handle, authorization(12));
}

#[tokio::test]
async fn handover_timeout_does_not_overflow_at_maximum_timestamp() {
    let handle = handle();
    handle
        .peer_start_publishing(PeerId::random(), peer_authorization(u64::MAX - 1, 9))
        .expect("peer start");
    for timestamp in [u64::MAX - 1, u64::MAX] {
        handle
            .start_publishing(authorization(timestamp))
            .expect("wait");
        assert!(matches!(
            handle.publishing_status(),
            PublishingStatus::WaitingToPublish { .. }
        ));
    }
}

#[tokio::test]
async fn missing_key_and_expired_peer_start_preserve_existing_behavior() {
    let no_key = FlashblocksHandle::new(SigningKey::from_bytes(&[7; 32]).verifying_key(), None);
    assert_eq!(
        no_key.start_publishing(authorization(1)),
        Err(FlashblocksP2PError::MissingBuilderSk)
    );
    assert_eq!(
        no_key.stop_publishing(),
        Err(FlashblocksP2PError::MissingBuilderSk)
    );
    no_key
        .peer_start_publishing(PeerId::random(), peer_authorization(2, 9))
        .expect("receive-only peer start is ignored");
    let handle = handle();
    handle.state.lock().payload_timestamp = 3;
    let pending = handle
        .prepare_start_publishing(authorization(4))
        .expect("request")
        .expect("notification");
    assert!(matches!(
        handle.prepare_peer_start(PeerId::random(), peer_authorization(2, 9)),
        Err(FlashblocksP2PError::ExpiredAuthorization)
    ));
    complete(&handle, pending).expect("expired peer request did not cancel valid request");
    assert_publishing(&handle, authorization(4));
}

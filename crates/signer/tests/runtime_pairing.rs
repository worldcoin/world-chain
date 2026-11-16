mod common;

use alloy_consensus::{SignableTransaction, TxEip1559, TxEnvelope};
use alloy_eips::{Decodable2718, Encodable2718};
use alloy_primitives::{Address, B256, Bytes, TxKind};
use common::{FRAME_LIMIT, Harness, bytes, complete_frame, corpus, deadline, large_publication};
use ed25519_dalek::{Signature, Signer, SigningKey};
use std::{sync::Arc, time::Duration};
use world_chain_signer::{
    Error, GrantSigner, PbhSigner, PublicationSigner, bridge::PbhBridge, validate_publication,
};

fn transactions() -> Vec<(TxEip1559, Bytes)> {
    let corpus: serde_json::Value =
        serde_json::from_str(include_str!("../testdata/pbh-runtime-v1.json"))
            .expect("public cast corpus");
    corpus["cases"]
        .as_array()
        .expect("cases")
        .iter()
        .map(|v| {
            let raw = bytes(&v["signedTransaction"]);
            let TxEnvelope::Eip1559(signed) =
                TxEnvelope::decode_2718(&mut raw.as_ref()).expect("typed transaction")
            else {
                panic!("type 2")
            };
            (signed.tx().clone(), raw)
        })
        .collect()
}

#[test]
fn complete_maximum_world_chain_frame_passes_remote_preflight() {
    let frame = large_publication();
    assert_eq!(frame.wire.len(), FRAME_LIMIT);
    assert!(frame.message.len() > FRAME_LIMIT / 2);
    assert_eq!(
        validate_publication(frame.message.len(), frame.authorization.len()),
        Ok(())
    );
    assert_eq!(
        complete_frame(&frame.message, &frame.authorization, &frame.signature),
        frame.wire
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires the reviewed disposable signer harness binary and revision"]
async fn actual_role_clients_pair_with_the_pinned_runtime() {
    let harness = Harness::start().await;
    let grant = GrantSigner::new(harness.ed25519(true, "authorizer")).expect("grant client");
    let publication =
        PublicationSigner::new(harness.ed25519(false, "publisher")).expect("publication client");
    let pbh = PbhSigner::new(harness.pbh("pbh")).expect("PBH client");
    check_cancelled_handshake_releases_admission(&harness).await;
    check_bridge_cancellation_and_cross_job_admission(&harness).await;
    grant.health(deadline()).await.expect("protocol health");
    publication
        .health(deadline())
        .await
        .expect("protocol health");
    pbh.health(deadline()).await.expect("protocol health");
    let corpus = corpus();
    let payload_id = corpus["payloadId"]
        .as_str()
        .expect("payload ID")
        .parse()
        .expect("eight bytes");
    let publisher = B256::from(publication.public_key().to_bytes());
    let signature = grant
        .sign(payload_id, 10, publisher, deadline())
        .await
        .expect("grant");
    assert_eq!(
        signature.to_bytes().as_slice(),
        bytes(&corpus["authorizationSignature"]).as_ref()
    );
    let authorization = bytes(&corpus["authorizationRlp"]);
    for vector in corpus["messages"].as_array().expect("vectors") {
        let message = bytes(&vector["messageRlp"]);
        let signature = publication
            .sign(&message, &authorization, deadline())
            .await
            .expect("payload/start/stop publication");
        assert_eq!(
            signature.to_bytes().as_slice(),
            bytes(&vector["publisherSignature"]).as_ref()
        );
        assert_eq!(
            complete_frame(&message, &authorization, &signature),
            bytes(&vector["wire"]).as_ref()
        );
    }
    // Metadata keeps its original signed JSON spacing instead of being normalized.
    let raw: serde_json::Value = serde_json::from_str(include_str!(
        "../../primitives/testdata/flashblock-evidence-v1.json"
    ))
    .expect("raw metadata corpus");
    {
        let wire = bytes(&raw["evidence"]["wire"]);
        let mut outer = &wire[1..];
        alloy_rlp::Header::decode(&mut outer).expect("authorized envelope");
        let mut body = outer;
        let header = alloy_rlp::Header::decode(&mut body).expect("message header");
        let message = &outer[..outer.len() - body.len() + header.payload_length];
        let mut digest = blake3::Hasher::new();
        digest.update(message);
        digest.update(&authorization);
        let expected = SigningKey::from_bytes(&[8; 32]).sign(digest.finalize().as_bytes());
        assert_eq!(
            publication
                .sign(message, &authorization, deadline())
                .await
                .expect("original metadata publication"),
            expected
        );
    }
    for (tx, raw) in transactions() {
        let signature = pbh.sign(&tx, deadline()).await.expect("PBH signature");
        assert_eq!(tx.into_signed(signature).encoded_2718(), raw.as_ref());
    }
    let (tx, expected_raw) = transactions().remove(0);
    let bridge = Arc::new(PbhBridge::new(pbh.clone()));
    let handle = tokio::runtime::Handle::current();
    let signature = tokio::task::spawn_blocking(move || {
        handle.block_on(async {
            bridge.sign(
                &tx,
                std::time::Instant::now() + Duration::from_secs(3),
                || false,
            )
        })
    })
    .await
    .expect("existing build-worker topology")
    .expect("bounded PBH bridge");
    assert_eq!(
        transactions()
            .remove(0)
            .0
            .into_signed(signature)
            .encoded_2718(),
        expected_raw.as_ref()
    );

    let denied = GrantSigner::new(harness.ed25519(true, "publisher")).expect("wrong role client");
    assert!(matches!(
        denied.sign(payload_id, 10, publisher, deadline()).await,
        Err(Error::Remote { code: -32001, .. })
    ));
    let mut wrong_key = harness.ed25519(false, "publisher");
    wrong_key.expected_public_key =
        B256::from(SigningKey::from_bytes(&[9; 32]).verifying_key().to_bytes());
    let wrong_key = PublicationSigner::new(wrong_key).expect("independently configured identity");
    assert_eq!(
        wrong_key.sign(&[0xc1, 1], &authorization, deadline()).await,
        Err(Error::InvalidSignature)
    );
    let mut wrong_ca = harness.pbh("pbh");
    wrong_ca.connection.tls.ca = harness.connection("authorizer").tls.certificate;
    let wrong_ca = PbhSigner::new(wrong_ca).expect("untrusted CA configuration");
    assert!(matches!(
        wrong_ca.health(deadline()).await,
        Err(Error::Transport {
            operation: "mTLS handshake",
            ..
        })
    ));
    let unenrolled = PbhSigner::new(harness.pbh("unenrolled")).expect("unenrolled client");
    assert!(matches!(
        unenrolled
            .sign(&transactions().remove(0).0, deadline())
            .await,
        Err(Error::Remote { code: -32001, .. })
    ));
    for mutation in [0, 1] {
        let (mut tx, _) = transactions().remove(0);
        if mutation == 0 {
            tx.chain_id = 4801;
        } else {
            tx.to = TxKind::Call(Address::ZERO);
        }
        assert!(matches!(
            pbh.sign(&tx, deadline()).await,
            Err(Error::Remote { code: -32001, .. })
        ));
    }
    assert_eq!(
        publication
            .sign(&[0xc1, 1], &authorization, tokio::time::Instant::now())
            .await,
        Err(Error::Deadline)
    );
    // Expiry must be enforced on these already-open authenticated connections.
    harness.stop_renewal();
    tokio::time::sleep(Duration::from_secs(9)).await;
    assert!(matches!(
        grant.sign(payload_id, 10, publisher, deadline()).await,
        Err(Error::Remote { code: -32002, .. })
    ));
    assert!(matches!(
        publication
            .sign(&[0xc1, 1], &authorization, deadline())
            .await,
        Err(Error::Remote { code: -32002, .. })
    ));
    assert!(matches!(
        pbh.sign(&transactions().remove(0).0, deadline()).await,
        Err(Error::Remote { code: -32002, .. })
    ));
    grant
        .health(deadline())
        .await
        .expect("health is not signing readiness");
}

async fn check_cancelled_handshake_releases_admission(harness: &Harness) {
    use tokio::{io::AsyncReadExt, net::TcpListener, sync::oneshot};
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("disposable stall listener");
    let mut config = harness.pbh("pbh");
    config.connection.endpoint = format!(
        "https://{}/",
        listener.local_addr().expect("listener address")
    )
    .parse()
    .expect("loopback endpoint");
    let client = PbhSigner::new(config).expect("bounded client");
    let mut oversized = harness.ed25519(false, "publisher");
    oversized.connection.endpoint = format!(
        "https://{}/",
        listener.local_addr().expect("listener address")
    )
    .parse()
    .expect("loopback endpoint");
    let oversized = PublicationSigner::new(oversized).expect("publication client");
    assert_eq!(
        oversized
            .sign(&vec![0; FRAME_LIMIT + 1], &[], deadline())
            .await,
        Err(Error::TooLarge("publication RLP"))
    );
    assert!(
        tokio::time::timeout(Duration::from_millis(50), listener.accept())
            .await
            .is_err(),
        "oversize preflight performs no network I/O"
    );
    let (accepted, connected) = oneshot::channel();
    let stalled = tokio::spawn(async move {
        let mut accepted = Some(accepted);
        for _ in 0..2 {
            let (mut stream, _) = tokio::time::timeout(Duration::from_secs(3), listener.accept())
                .await
                .expect("bounded test accept")
                .expect("test connection");
            if let Some(accepted) = accepted.take() {
                accepted.send(()).expect("admission notification");
            }
            let mut buffer = [0; 4096];
            tokio::time::timeout(Duration::from_secs(3), async {
                while stream
                    .read(&mut buffer)
                    .await
                    .expect("stalled handshake bytes")
                    != 0
                {}
            })
            .await
            .expect("cancellation/deadline closes TCP");
        }
        assert!(
            tokio::time::timeout(Duration::from_millis(50), listener.accept())
                .await
                .is_err(),
            "no automatic retries"
        );
    });
    let held = client.clone();
    let pending = tokio::spawn(async move { held.health(deadline()).await });
    connected.await.expect("first operation holds admission");
    assert_eq!(client.health(deadline()).await, Err(Error::Busy));
    pending.abort();
    assert!(
        pending
            .await
            .expect_err("cancelled operation")
            .is_cancelled()
    );
    assert_eq!(
        client
            .health(tokio::time::Instant::now() + Duration::from_millis(80))
            .await,
        Err(Error::Deadline)
    );
    stalled
        .await
        .expect("no connection leaked after cancellation or deadline");
}

async fn check_bridge_cancellation_and_cross_job_admission(harness: &Harness) {
    use std::sync::atomic::{AtomicBool, Ordering};
    use tokio::{io::AsyncReadExt, net::TcpListener, sync::oneshot};
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bridge stall listener");
    let mut config = harness.pbh("pbh");
    config.connection.endpoint = format!(
        "https://{}/",
        listener.local_addr().expect("listener address")
    )
    .parse()
    .expect("loopback endpoint");
    let bridge = PbhBridge::new(PbhSigner::new(config).expect("bounded PBH client"));
    let (accepted, connected) = oneshot::channel();
    let (closed, disconnected) = oneshot::channel();
    let stalled = tokio::spawn(async move {
        let mut accepted = Some(accepted);
        let mut closed = Some(closed);
        for _ in 0..2 {
            let (mut stream, _) = tokio::time::timeout(Duration::from_secs(3), listener.accept())
                .await
                .expect("bounded accept")
                .expect("bridge connection");
            if let Some(sender) = accepted.take() {
                sender.send(()).expect("admission observation");
            }
            let mut buffer = [0; 4096];
            tokio::time::timeout(Duration::from_secs(3), async {
                while stream.read(&mut buffer).await.expect("bridge TLS bytes") != 0 {}
            })
            .await
            .expect("abandoned bridge I/O terminates");
            if let Some(sender) = closed.take() {
                sender.send(()).expect("closure observation");
            }
        }
    });
    let cancelled = Arc::new(AtomicBool::new(false));
    let worker_cancelled = cancelled.clone();
    let wallet = bridge.clone();
    let handle = tokio::runtime::Handle::current();
    let first = tokio::task::spawn_blocking(move || {
        handle.block_on(async {
            wallet.sign(
                &transactions().remove(0).0,
                std::time::Instant::now() + Duration::from_secs(3),
                || worker_cancelled.load(Ordering::Acquire),
            )
        })
    });
    connected.await.expect("first payload job admitted");
    let wallet = bridge.clone();
    let second = tokio::task::spawn_blocking(move || {
        wallet.sign(
            &transactions().remove(0).0,
            std::time::Instant::now() + Duration::from_secs(3),
            || false,
        )
    });
    assert_eq!(
        second.await.expect("competing payload job"),
        Err(Error::Busy)
    );
    cancelled.store(true, Ordering::Release);
    assert_eq!(
        first.await.expect("cancelled build worker"),
        Err(Error::Cancelled)
    );
    disconnected.await.expect("in-flight I/O actually stopped");
    let last = tokio::task::spawn_blocking(move || {
        bridge.sign(
            &transactions().remove(0).0,
            std::time::Instant::now() + Duration::from_millis(80),
            || false,
        )
    });
    assert_eq!(
        last.await.expect("post-cancellation payload job"),
        Err(Error::Deadline)
    );
    stalled.await.expect("bridge leaves no connection running");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires a reviewed 16 MiB signer harness; run separately from old-profile pairing"]
async fn actual_client_signs_complete_maximum_world_chain_frame() {
    let frame = large_publication();
    let harness = Harness::start().await;
    let signer =
        PublicationSigner::new(harness.ed25519(false, "publisher")).expect("publication client");
    let signature: Signature = signer
        .sign(&frame.message, &frame.authorization, deadline())
        .await
        .expect("full frame signing within v1 deadline");
    assert_eq!(signature, frame.signature);
    assert_eq!(
        complete_frame(&frame.message, &frame.authorization, &signature),
        frame.wire
    );
}

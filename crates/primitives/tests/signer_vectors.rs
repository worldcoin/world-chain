use alloy_primitives::{B256, Bytes};
use alloy_rlp::{Decodable, Encodable};
use alloy_rpc_types_engine::PayloadId;
use ed25519_dalek::SigningKey;
use serde::Deserialize;
use world_chain_primitives::p2p::{Authorization, Authorized, AuthorizedMsg, FlashblocksP2PMsg};

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Vectors {
    version: u32,
    authorizer_public_key: B256,
    publisher_public_key: B256,
    payload_id: PayloadId,
    timestamp: u64,
    authorization_preimage: Bytes,
    authorization_digest: B256,
    authorization_signature: Bytes,
    authorization_rlp: Bytes,
    messages: Vec<MessageVector>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct MessageVector {
    kind: String,
    message_rlp: Bytes,
    publisher_preimage: Bytes,
    publisher_digest: B256,
    publisher_signature: Bytes,
    wire: Bytes,
}

fn vectors() -> Vectors {
    serde_json::from_str(include_str!("../testdata/flashblock-signing-v1.json"))
        .expect("public signing vectors")
}

#[test]
fn signing_vectors_preserve_all_flashblock_message_bytes() {
    let vectors = vectors();
    assert_eq!(vectors.version, 1);
    let authorizer = SigningKey::from_bytes(&[7; 32]);
    let publisher = SigningKey::from_bytes(&[8; 32]);
    assert_eq!(
        authorizer.verifying_key().as_bytes(),
        &vectors.authorizer_public_key.0
    );
    assert_eq!(
        publisher.verifying_key().as_bytes(),
        &vectors.publisher_public_key.0
    );
    let authorization = Authorization::new(
        vectors.payload_id,
        vectors.timestamp,
        &authorizer,
        publisher.verifying_key(),
    );
    let mut preimage = vectors.payload_id.0.to_vec();
    preimage.extend_from_slice(&vectors.timestamp.to_le_bytes());
    preimage.extend_from_slice(publisher.verifying_key().as_bytes());
    assert_eq!(preimage, vectors.authorization_preimage.as_ref());
    assert_eq!(
        blake3::hash(&preimage).as_bytes(),
        &vectors.authorization_digest.0
    );
    assert_eq!(
        authorization.authorizer_sig.to_bytes(),
        vectors.authorization_signature.as_ref()
    );
    assert_eq!(
        alloy_rlp::encode(authorization),
        vectors.authorization_rlp.as_ref()
    );

    assert_eq!(vectors.messages.len(), 3);
    for (vector, expected_kind) in
        vectors
            .messages
            .iter()
            .zip(["flashblock", "startPublish", "stopPublish"])
    {
        assert_eq!(vector.kind, expected_kind);
        let mut remaining = vector.message_rlp.as_ref();
        let msg = AuthorizedMsg::decode(&mut remaining).expect("message RLP");
        assert!(remaining.is_empty());
        assert!(matches!(
            (&*vector.kind, &msg),
            ("flashblock", AuthorizedMsg::FlashblocksPayloadV1(_))
                | ("startPublish", AuthorizedMsg::StartPublish(_))
                | ("stopPublish", AuthorizedMsg::StopPublish(_))
        ));
        let mut preimage = alloy_rlp::encode(&msg);
        authorization.encode(&mut preimage);
        assert_eq!(preimage, vector.publisher_preimage.as_ref());
        assert_eq!(
            blake3::hash(&preimage).as_bytes(),
            &vector.publisher_digest.0
        );
        let signed = Authorized::new(&publisher, authorization, msg);
        assert_eq!(
            signed.actor_sig.to_bytes(),
            vector.publisher_signature.as_ref()
        );
        assert_eq!(
            FlashblocksP2PMsg::Authorized(signed).encode().as_ref(),
            vector.wire.as_ref()
        );
        let mut remaining = vector.wire.as_ref();
        let parsed = FlashblocksP2PMsg::parse(&vector.wire).expect("original wire");
        let world_chain_primitives::p2p::FlashblocksP2PMsgParts::Authorized(parts) = parsed else {
            panic!("expected authorized vector");
        };
        parts
            .verify(authorizer.verifying_key())
            .expect("original signatures");
        let FlashblocksP2PMsg::Authorized(decoded) =
            FlashblocksP2PMsg::decode(&mut remaining).expect("full frame")
        else {
            panic!("expected authorized vector");
        };
        assert!(remaining.is_empty());
        decoded
            .verify(authorizer.verifying_key())
            .expect("decoded signatures");
    }
}

#[test]
fn signing_vectors_reject_changed_context_and_wrong_publisher() {
    let vectors = vectors();
    let authorizer = SigningKey::from_bytes(&[7; 32]);
    let mut remaining = vectors.authorization_rlp.as_ref();
    let authorization = Authorization::decode(&mut remaining).expect("authorization RLP");
    assert!(remaining.is_empty());
    for change in 0..3 {
        let mut changed = authorization;
        match change {
            0 => changed.timestamp ^= 1,
            1 => changed.payload_id.0[0] ^= 1,
            _ => changed.builder_vk = authorizer.verifying_key(),
        }
        assert!(changed.verify(authorizer.verifying_key()).is_err());
    }
    for vector in vectors.messages {
        let mut remaining = vector.message_rlp.as_ref();
        let msg = AuthorizedMsg::decode(&mut remaining).expect("message RLP");
        let signed_with_authorizer_key = Authorized::new(&authorizer, authorization, msg);
        assert!(
            signed_with_authorizer_key
                .verify(authorizer.verifying_key())
                .is_err()
        );
    }
}

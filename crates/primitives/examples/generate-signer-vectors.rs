//! Generate public compatibility vectors using deterministic test keys only.
use alloy_primitives::{B256, Bytes};
use alloy_rlp::Encodable;
use ed25519_dalek::SigningKey;
use world_chain_primitives::p2p::{
    Authorization, Authorized, AuthorizedMsg, FlashblocksP2PMsg, StartPublish, StopPublish,
};

fn main() -> eyre::Result<()> {
    let authorizer = SigningKey::from_bytes(&[7; 32]);
    let publisher = SigningKey::from_bytes(&[8; 32]);
    let existing: serde_json::Value =
        serde_json::from_str(include_str!("../testdata/flashblock-evidence-v1.json"))?;
    let wire: Bytes = serde_json::from_value(existing["evidence"]["wire"].clone())?;
    let FlashblocksP2PMsg::Authorized(payload) = FlashblocksP2PMsg::decode(&mut wire.as_ref())?
    else {
        eyre::eyre::bail!("expected authorized fixture");
    };
    let authorization = Authorization::new(
        payload.authorization.payload_id,
        payload.authorization.timestamp,
        &authorizer,
        publisher.verifying_key(),
    );
    let mut authorization_preimage = authorization.payload_id.0.to_vec();
    authorization_preimage.extend_from_slice(&authorization.timestamp.to_le_bytes());
    authorization_preimage.extend_from_slice(publisher.verifying_key().as_bytes());
    let authorization_rlp = alloy_rlp::encode(authorization);
    let mut messages = Vec::new();
    for (kind, msg) in [
        ("flashblock", payload.msg),
        ("startPublish", AuthorizedMsg::from(StartPublish)),
        ("stopPublish", AuthorizedMsg::from(StopPublish)),
    ] {
        let message_rlp = alloy_rlp::encode(&msg);
        let mut publisher_preimage = message_rlp.clone();
        authorization.encode(&mut publisher_preimage);
        let signed = Authorized::new(&publisher, authorization, msg);
        messages.push(serde_json::json!({
            "kind": kind,
            "messageRlp": Bytes::from(message_rlp),
            "publisherPreimage": Bytes::from(publisher_preimage.clone()),
            "publisherDigest": B256::from(*blake3::hash(&publisher_preimage).as_bytes()),
            "publisherSignature": Bytes::copy_from_slice(&signed.actor_sig.to_bytes()),
            "wire": Bytes::copy_from_slice(&FlashblocksP2PMsg::Authorized(signed).encode()),
        }));
    }
    println!(
        "{}",
        serde_json::to_string_pretty(&serde_json::json!({
            "version": 1,
            "authorizerPublicKey": B256::from(*authorizer.verifying_key().as_bytes()),
            "publisherPublicKey": B256::from(*publisher.verifying_key().as_bytes()),
            "payloadId": authorization.payload_id,
            "timestamp": authorization.timestamp,
            "authorizationPreimage": Bytes::from(authorization_preimage.clone()),
            "authorizationDigest": B256::from(*blake3::hash(&authorization_preimage).as_bytes()),
            "authorizationSignature": Bytes::copy_from_slice(&authorization.authorizer_sig.to_bytes()),
            "authorizationRlp": Bytes::from(authorization_rlp),
            "messages": messages,
        }))?
    );
    Ok(())
}

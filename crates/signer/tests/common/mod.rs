use alloy_primitives::{Address, Bytes, FixedBytes, U256};
use alloy_rlp::{Encodable, Header};
use ed25519_dalek::{Signature, Signer, SigningKey};
use serde_json::Value;
use world_chain_primitives::{
    access_list::{FlashblockAccessList, FlashblockAccessListData, access_list_hash},
    p2p::{Authorization, FlashblocksP2PMsg},
    primitives::FlashblocksPayloadV1,
};
mod runtime;
pub use runtime::{Harness, deadline};

pub const FRAME_LIMIT: usize = 16 * 1024 * 1024;

pub fn corpus() -> Value {
    serde_json::from_str(include_str!(
        "../../../primitives/testdata/flashblock-signing-v1.json"
    ))
    .expect("public corpus")
}

pub fn bytes(value: &Value) -> Bytes {
    value
        .as_str()
        .expect("hex string")
        .parse()
        .expect("hex bytes")
}

pub struct LargePublication {
    pub message: Vec<u8>,
    pub authorization: Vec<u8>,
    pub signature: Signature,
    pub wire: Vec<u8>,
}

pub fn complete_frame(message: &[u8], authorization: &[u8], signature: &Signature) -> Vec<u8> {
    let signature = Bytes::copy_from_slice(&signature.to_bytes());
    let len = message.len() + authorization.len() + signature.length();
    let mut wire = vec![0];
    Header {
        list: true,
        payload_length: len,
    }
    .encode(&mut wire);
    wire.extend_from_slice(message);
    wire.extend_from_slice(authorization);
    signature.encode(&mut wire);
    wire
}

pub fn large_publication() -> LargePublication {
    let authorizer = SigningKey::from_bytes(&[7; 32]);
    let publisher = SigningKey::from_bytes(&[8; 32]);
    let payload_id = FixedBytes::new([1; 8]);
    let authorization = Authorization::new(
        payload_id.into(),
        10,
        &authorizer,
        publisher.verifying_key(),
    );
    let authorization = alloy_rlp::encode(authorization);
    let mut padding = FRAME_LIMIT - 1024;
    for _ in 0..4 {
        let mut payload: FlashblocksPayloadV1 = FlashblocksPayloadV1 {
            payload_id: payload_id.into(),
            ..Default::default()
        };
        payload.diff.transactions = vec![Bytes::from(vec![1; padding])];
        let mut access_list = FlashblockAccessList {
            min_tx_index: 0,
            max_tx_index: 1,
            ..Default::default()
        };
        access_list.changes.push(Default::default());
        access_list.changes[0].address = Address::with_last_byte(0x42);
        access_list.changes[0].storage_reads.push(U256::from(4));
        payload.diff.access_list_data = Some(FlashblockAccessListData {
            access_list_hash: access_list_hash(&access_list),
            access_list,
        });
        let mut message = Vec::new();
        Header {
            list: true,
            payload_length: 0u8.length() + payload.length(),
        }
        .encode(&mut message);
        0u8.encode(&mut message);
        payload.encode(&mut message);
        let mut hash = blake3::Hasher::new();
        hash.update(&message);
        hash.update(&authorization);
        let signature = publisher.sign(hash.finalize().as_bytes());
        let wire = complete_frame(&message, &authorization, &signature);
        if wire.len() == FRAME_LIMIT {
            let parsed = FlashblocksP2PMsg::parse(&wire).expect("complete World Chain frame");
            let world_chain_primitives::p2p::FlashblocksP2PMsgParts::Authorized(parts) = parsed
            else {
                panic!("authorized frame")
            };
            parts
                .verify(authorizer.verifying_key())
                .expect("independently signed original RLP");
            parts.into_authorized().expect("valid payload and metadata");
            return LargePublication {
                message,
                authorization,
                signature,
                wire,
            };
        }
        if wire.len() < FRAME_LIMIT {
            padding += FRAME_LIMIT - wire.len();
        } else {
            padding -= wire.len() - FRAME_LIMIT;
        }
    }
    panic!("failed to construct exact complete-frame limit");
}

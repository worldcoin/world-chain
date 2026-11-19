use crate::{
    Error, Result,
    config::Ed25519Config,
    transport::{AUTHORIZATION_RLP, Method, PUBLICATION_RLP, Transport, decode_hex},
};
use alloy_primitives::{B256, FixedBytes};
use ed25519_dalek::{Signature, VerifyingKey};
use serde::{Serialize, Serializer};
use std::sync::Arc;
use tokio::time::Instant;

pub(crate) struct HexBytes<'a>(pub &'a [u8]);
impl Serialize for HexBytes<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error> {
        serializer.serialize_str(&alloy_primitives::hex::encode_prefixed(self.0))
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct GrantRequest {
    key_id: B256,
    payload_id: FixedBytes<8>,
    timestamp: String,
    publisher_public_key: B256,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct PublicationRequest<'a> {
    key_id: B256,
    message_rlp: HexBytes<'a>,
    authorization_rlp: HexBytes<'a>,
}

fn verifying_key(config: &Ed25519Config) -> Result<VerifyingKey> {
    let key = VerifyingKey::from_bytes(&config.expected_public_key.0)
        .map_err(|_| Error::Config("invalid expected Ed25519 key"))?;
    if key.is_weak() {
        return Err(Error::Config("weak expected Ed25519 key"));
    }
    Ok(key)
}

fn verify(key: &VerifyingKey, digest: &blake3::Hash, value: &str) -> Result<Signature> {
    let bytes = decode_hex(value, 64)?;
    let signature = Signature::from_slice(&bytes).map_err(|_| Error::InvalidSignature)?;
    key.verify_strict(digest.as_bytes(), &signature)
        .map_err(|_| Error::InvalidSignature)?;
    Ok(signature)
}

/// Only the enrolled grant operation; this client cannot publish or sign arbitrary digests.
#[derive(Clone, Debug)]
pub struct GrantSigner {
    transport: Arc<Transport>,
    key_id: B256,
    expected: VerifyingKey,
}

impl GrantSigner {
    pub fn new(config: Ed25519Config) -> Result<Self> {
        let expected = verifying_key(&config)?;
        Ok(Self {
            transport: Arc::new(Transport::new(config.connection, "flashblock_authorizer")?),
            key_id: config.key_id,
            expected,
        })
    }
    pub fn public_key(&self) -> VerifyingKey {
        self.expected
    }
    pub async fn health(&self, deadline: Instant) -> Result<()> {
        self.transport.health(deadline).await
    }
    pub async fn wait_until_idle(&self, deadline: Instant) -> Result<()> {
        self.transport.wait_until_idle(deadline).await
    }
    pub async fn sign(
        &self,
        payload_id: FixedBytes<8>,
        timestamp: u64,
        publisher: B256,
        deadline: Instant,
    ) -> Result<Signature> {
        let request = GrantRequest {
            key_id: self.key_id,
            payload_id,
            timestamp: format!("0x{timestamp:x}"),
            publisher_public_key: publisher,
        };
        let mut preimage = [0u8; 48];
        preimage[..8].copy_from_slice(payload_id.as_slice());
        preimage[8..16].copy_from_slice(&timestamp.to_le_bytes());
        preimage[16..].copy_from_slice(publisher.as_slice());
        let digest = blake3::hash(&preimage);
        self.transport
            .call(Method::Grant, Some(&request), deadline, |value| {
                verify(&self.expected, &digest, value)
            })
            .await
    }
}

/// Signs exact, caller-owned RLP without decoding or normalizing metadata.
#[derive(Clone, Debug)]
pub struct PublicationSigner {
    transport: Arc<Transport>,
    key_id: B256,
    expected: VerifyingKey,
}

/// Reject oversized encoded objects before hashing, JSON allocation or remote I/O.
pub fn validate_publication(message_len: usize, authorization_len: usize) -> Result<()> {
    if message_len > PUBLICATION_RLP {
        return Err(Error::TooLarge("publication RLP"));
    }
    if authorization_len > AUTHORIZATION_RLP {
        return Err(Error::TooLarge("authorization RLP"));
    }
    Ok(())
}

impl PublicationSigner {
    pub fn new(config: Ed25519Config) -> Result<Self> {
        let expected = verifying_key(&config)?;
        Ok(Self {
            transport: Arc::new(Transport::new(config.connection, "flashblock_publisher")?),
            key_id: config.key_id,
            expected,
        })
    }
    pub fn public_key(&self) -> VerifyingKey {
        self.expected
    }
    /// Callers must bound pending intents; this only waits for previous I/O to leave.
    pub async fn wait_until_idle(&self, deadline: Instant) -> Result<()> {
        self.transport.wait_until_idle(deadline).await
    }
    pub async fn health(&self, deadline: Instant) -> Result<()> {
        self.transport.health(deadline).await
    }
    pub async fn sign(
        &self,
        message: &[u8],
        authorization: &[u8],
        deadline: Instant,
    ) -> Result<Signature> {
        validate_publication(message.len(), authorization.len())?;
        let request = PublicationRequest {
            key_id: self.key_id,
            message_rlp: HexBytes(message),
            authorization_rlp: HexBytes(authorization),
        };
        self.transport
            .call(Method::Publication, Some(&request), deadline, |value| {
                let mut hash = blake3::Hasher::new();
                hash.update(message);
                hash.update(authorization);
                verify(&self.expected, &hash.finalize(), value)
            })
            .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn publication_preflight_covers_the_approved_decoded_bound() {
        for len in [8 * 1024 * 1024 + 1, 16 * 1024 * 1024 - 1, 16 * 1024 * 1024] {
            assert_eq!(validate_publication(len, 256), Ok(()));
        }
        assert_eq!(
            validate_publication(16 * 1024 * 1024 + 1, 256),
            Err(Error::TooLarge("publication RLP"))
        );
        assert_eq!(
            validate_publication(1, 257),
            Err(Error::TooLarge("authorization RLP"))
        );
        assert_eq!(
            validate_publication(usize::MAX, 1),
            Err(Error::TooLarge("publication RLP"))
        );
    }
}

use crate::{Error, Result, bridge::PbhBridge};
use alloy_consensus::{SignableTransaction, TxEip1559};
use alloy_primitives::{Address, Signature, hex, keccak256};
use k256::ecdsa::SigningKey;
use std::{fmt, str::FromStr, time::Instant};
use zeroize::Zeroizing;

/// The in-process PBH transaction signer.
#[derive(Clone, PartialEq, Eq)]
pub struct LocalPbhSigner {
    key: SigningKey,
    address: Address,
}

impl LocalPbhSigner {
    pub fn from_bytes(bytes: &[u8; 32]) -> Result<Self> {
        let key = SigningKey::from_bytes(bytes.into()).map_err(|_| Error::InvalidLocalKey)?;
        let point = key.verifying_key().to_sec1_point(false);
        let address = Address::from_word(keccak256(&point.as_bytes()[1..]));
        Ok(Self { key, address })
    }

    fn sign(&self, tx: &TxEip1559) -> Result<Signature> {
        let (signature, recovery) = self
            .key
            .sign_prehash_recoverable(tx.signature_hash().as_slice());
        // Ethereum encodes only the y parity.
        if recovery.is_x_reduced() {
            return Err(Error::LocalSigning);
        }
        Ok(Signature::from_bytes_and_parity(
            &signature.to_bytes(),
            recovery.is_y_odd(),
        ))
    }
}

impl fmt::Debug for LocalPbhSigner {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("LocalPbhSigner")
            .field("address", &self.address)
            .finish()
    }
}

impl FromStr for LocalPbhSigner {
    type Err = Error;

    fn from_str(value: &str) -> Result<Self> {
        let mut bytes = Zeroizing::new([0; 32]);
        hex::decode_to_slice(value, bytes.as_mut()).map_err(|_| Error::InvalidLocalKey)?;
        Self::from_bytes(&bytes)
    }
}

/// Explicit backend selection; a remote failure never chooses a local key.
#[derive(Clone, Debug)]
pub enum PbhWallet {
    Local(LocalPbhSigner),
    Remote(PbhBridge),
}

impl From<LocalPbhSigner> for PbhWallet {
    fn from(key: LocalPbhSigner) -> Self {
        Self::Local(key)
    }
}

impl PbhWallet {
    pub fn address(&self) -> Address {
        match self {
            Self::Local(key) => key.address,
            Self::Remote(client) => client.address(),
        }
    }

    pub fn is_remote(&self) -> bool {
        matches!(self, Self::Remote(_))
    }

    pub fn sign(
        &self,
        tx: &mut TxEip1559,
        deadline: Option<Instant>,
        cancelled: impl Fn() -> bool,
    ) -> Result<Signature> {
        if cancelled() {
            return Err(Error::Cancelled);
        }
        match self {
            Self::Local(key) => key.sign(tx),
            Self::Remote(client) => client.sign(
                tx,
                deadline.ok_or(Error::Config(
                    "remote PBH signing requires the payload job deadline",
                ))?,
                cancelled,
            ),
        }
    }
}

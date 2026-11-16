use crate::{Error, Result, bridge::PbhBridge};
use alloy_consensus::TxEip1559;
use alloy_network::TxSignerSync;
use alloy_primitives::{Address, Signature};
use alloy_signer_local::PrivateKeySigner;
use std::time::Instant;

/// Explicit backend selection; a remote failure never chooses a local key.
#[derive(Clone, Debug)]
pub enum PbhWallet {
    Local(PrivateKeySigner),
    Remote(PbhBridge),
}

impl From<PrivateKeySigner> for PbhWallet {
    fn from(key: PrivateKeySigner) -> Self {
        Self::Local(key)
    }
}

impl PbhWallet {
    pub fn address(&self) -> Address {
        match self {
            Self::Local(key) => key.address(),
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
            Self::Local(key) => key
                .sign_transaction_sync(tx)
                .map_err(|_| Error::LocalSigning),
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

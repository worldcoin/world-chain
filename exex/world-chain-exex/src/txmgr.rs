//! Minimal L1 transaction manager.
//!
//! Rust analogue of the slice of `op-service/txmgr` the proposer actually
//! exercises: build → sign → submit → await receipt, with a single bump &
//! rebroadcast on stall. The full upstream txmgr (RBF batching, queues,
//! multi-network) is overkill — the proposer only has one in-flight tx at a
//! time.

use std::{sync::Arc, time::Duration};

use alloy_network::{EthereumWallet, TransactionBuilder};
use alloy_primitives::{Address, B256, Bytes, U256, hex};
use alloy_provider::{DynProvider, PendingTransactionBuilder, Provider, ProviderBuilder};
use alloy_rpc_types::TransactionRequest;
use alloy_signer_local::{MnemonicBuilder, PrivateKeySigner, coins_bip39::English};
use thiserror::Error;
use tracing::{info, warn};
use url::Url;

/// Outcome of a transaction submission.
#[derive(Debug, Clone)]
pub struct TxReceiptSummary {
    pub tx_hash: B256,
    pub block_number: Option<u64>,
    pub block_hash: Option<B256>,
    pub status: bool,
}

/// Candidate transaction to submit. Mirrors `txmgr.TxCandidate`.
#[derive(Debug, Clone)]
pub struct TxCandidate {
    pub to: Address,
    pub value: U256,
    pub data: Bytes,
    pub gas_limit: Option<u64>,
}

/// Per-`TxManager` configuration.
#[derive(Debug, Clone)]
pub struct TxManagerConfig {
    pub network_timeout: Duration,
    pub resubmission_timeout: Duration,
    pub num_confirmations: u64,
}

/// Minimal L1 tx manager.
pub struct TxManager {
    provider: Arc<DynProvider>,
    signer_address: Address,
    config: TxManagerConfig,
}

impl TxManager {
    pub async fn new(
        l1_rpc_url: &str,
        signer: SignerKey,
        config: TxManagerConfig,
    ) -> Result<Self, TxManagerError> {
        let url = Url::parse(l1_rpc_url).map_err(|e| TxManagerError::Config(e.to_string()))?;
        let signer_address = signer.address();
        let wallet = EthereumWallet::new(signer.inner);
        let provider = ProviderBuilder::new().wallet(wallet).connect_http(url);
        let provider = Arc::new(provider.erased());
        Ok(Self {
            provider,
            signer_address,
            config,
        })
    }

    /// Address that signs and pays for proposer transactions.
    pub fn from_address(&self) -> Address {
        self.signer_address
    }

    /// Returns a clone of the underlying erased provider for read-only ops.
    pub fn provider(&self) -> Arc<DynProvider> {
        self.provider.clone()
    }

    /// Current L1 block number.
    pub async fn block_number(&self) -> Result<u64, TxManagerError> {
        let fut = self.provider.get_block_number();
        let n = tokio::time::timeout(self.config.network_timeout, fut)
            .await
            .map_err(|_| TxManagerError::Timeout)?
            .map_err(|e| TxManagerError::Rpc(e.to_string()))?;
        Ok(n)
    }

    /// Build → sign → submit → await `num_confirmations` confirmations.
    ///
    /// On stall (resubmission timeout reached without inclusion), the tx is
    /// rebroadcast with bumped priority fee (alloy's fillers recompute caps
    /// on each `send_transaction`).
    pub async fn send(&self, candidate: TxCandidate) -> Result<TxReceiptSummary, TxManagerError> {
        let mut req = TransactionRequest::default()
            .with_from(self.signer_address)
            .with_to(candidate.to)
            .with_value(candidate.value)
            .with_input(candidate.data);
        if let Some(gas) = candidate.gas_limit {
            req = req.with_gas_limit(gas);
        }

        let mut last_hash: Option<B256> = None;
        for attempt in 0..3 {
            let pending: PendingTransactionBuilder<_> = self
                .provider
                .send_transaction(req.clone())
                .await
                .map_err(|e| TxManagerError::Rpc(e.to_string()))?;
            let tx_hash = *pending.tx_hash();
            last_hash = Some(tx_hash);
            info!(
                target: "exex::proposer::txmgr",
                ?tx_hash,
                attempt,
                "submitted L1 proposal tx",
            );

            let confirmed = pending.with_required_confirmations(self.config.num_confirmations);
            match tokio::time::timeout(self.config.resubmission_timeout, confirmed.get_receipt())
                .await
            {
                Ok(Ok(receipt)) => {
                    return Ok(TxReceiptSummary {
                        tx_hash: receipt.transaction_hash,
                        block_number: receipt.block_number,
                        block_hash: receipt.block_hash,
                        status: receipt.status(),
                    });
                }
                Ok(Err(e)) => {
                    return Err(TxManagerError::Rpc(format!(
                        "receipt fetch failed for 0x{}: {e}",
                        hex::encode(tx_hash)
                    )));
                }
                Err(_) => {
                    warn!(
                        target: "exex::proposer::txmgr",
                        ?tx_hash,
                        attempt,
                        "resubmission timeout reached; bumping fees",
                    );
                    if let Some(max_priority) = req.max_priority_fee_per_gas {
                        req = req.with_max_priority_fee_per_gas(max_priority.saturating_mul(5) / 4);
                    }
                    continue;
                }
            }
        }

        Err(TxManagerError::Stalled(last_hash))
    }

    /// Best-effort drain of any in-flight resources. Currently a noop.
    pub fn close(&self) {}
}

/// Wrapper around a constructed signer.
#[derive(Debug, Clone)]
pub struct SignerKey {
    pub inner: PrivateKeySigner,
}

impl SignerKey {
    pub fn address(&self) -> Address {
        <PrivateKeySigner as alloy_signer::Signer>::address(&self.inner)
    }

    pub fn from_private_key_hex(raw: &str) -> Result<Self, TxManagerError> {
        let s = raw.trim_start_matches("0x");
        let bytes = hex::decode(s).map_err(|e| TxManagerError::Signer(e.to_string()))?;
        if bytes.len() != 32 {
            return Err(TxManagerError::Signer(format!(
                "expected 32-byte key, got {}",
                bytes.len()
            )));
        }
        let mut arr = [0u8; 32];
        arr.copy_from_slice(&bytes);
        let inner = PrivateKeySigner::from_bytes(&arr.into())
            .map_err(|e| TxManagerError::Signer(e.to_string()))?;
        Ok(Self { inner })
    }

    pub fn from_mnemonic(mnemonic: &str, hd_path: &str) -> Result<Self, TxManagerError> {
        let inner = MnemonicBuilder::<English>::default()
            .phrase(mnemonic)
            .derivation_path(hd_path)
            .map_err(|e| TxManagerError::Signer(e.to_string()))?
            .build()
            .map_err(|e| TxManagerError::Signer(e.to_string()))?;
        Ok(Self { inner })
    }
}

#[derive(Debug, Error)]
pub enum TxManagerError {
    #[error("config error: {0}")]
    Config(String),
    #[error("signer error: {0}")]
    Signer(String),
    #[error("rpc error: {0}")]
    Rpc(String),
    #[error("rpc call timed out")]
    Timeout,
    #[error("transaction stalled without inclusion (last attempt: {0:?})")]
    Stalled(Option<B256>),
}

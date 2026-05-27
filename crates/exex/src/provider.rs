//! L1 provider builder.
//!
//! Adapted from `world-id-protocol/services/common/src/provider.rs`. Produces
//! a [`DynProvider`] with:
//!
//! * **Fallback** across multiple HTTP endpoints (alloy [`FallbackLayer`]).
//! * **Retry / rate-limit** backoff (alloy [`RetryBackoffLayer`]).
//! * **Default gas estimation** + our [`GasEstimateWithFallbackFiller`] (3/2
//!   safety margin, fallback gas on revert).
//! * **Cached, monotonic nonce management** (alloy [`CachedNonceManager`]).
//! * **`chain_id` pre-fetch**, so signed txs go out with the correct chain.
//! * **`EthereumWallet`** signer (private-key or BIP-39 mnemonic).
//!
//! Deliberately small surface: a single `build()` entry-point that takes a
//! resolved [`L1ProviderConfig`] and returns the provider — the contract
//! instance (and from-address) needed by the proposer.

use std::{num::NonZeroUsize, time::Duration};

use alloy_network::EthereumWallet;
use alloy_primitives::Address;
use alloy_provider::{DynProvider, Provider, ProviderBuilder, fillers::CachedNonceManager};
use alloy_rpc_client::RpcClient;
use alloy_signer_local::{MnemonicBuilder, PrivateKeySigner, coins_bip39::English};
use alloy_transport::layers::{FallbackLayer, RetryBackoffLayer};
use alloy_transport_http::{Http, reqwest};
use thiserror::Error;
use tower::ServiceBuilder;
use url::Url;

use crate::tx_fillers::GasEstimateWithFallbackFiller;

/// Resolved configuration for the L1 provider.
#[derive(Debug, Clone)]
pub struct L1ProviderConfig {
    /// HTTP RPC endpoints, in priority order. At least one is required.
    pub http_urls: Vec<Url>,
    /// Per-request HTTP timeout.
    pub timeout: Duration,
    /// Maximum rate-limit retries before giving up.
    pub max_rate_limit_retries: u32,
    /// Initial backoff (ms) used by the retry layer.
    pub initial_backoff_ms: u64,
    /// Compute units per second — see alloy's [`RetryBackoffLayer::new`].
    pub compute_units_per_second: u64,
    /// Signer for the proposer EOA.
    pub signer: SignerKind,
}

/// Supported signer kinds. Mirrors the subset of
/// `world-id-protocol/services/common`'s `SignerConfig` we need for the
/// proposer; AWS KMS support can be re-added later if needed.
#[derive(Debug, Clone)]
pub enum SignerKind {
    /// Hex-encoded private key (with or without `0x`).
    PrivateKey(String),
    /// BIP-39 mnemonic + HD path.
    Mnemonic { phrase: String, hd_path: String },
}

/// A built L1 provider, plus the resolved `from` address of the wallet.
#[derive(Clone)]
pub struct L1Provider {
    pub provider: DynProvider,
    pub from: Address,
}

#[derive(Debug, Error)]
pub enum ProviderError {
    #[error("no HTTP URLs configured")]
    NoHttpUrls,
    #[error("invalid private key: {0}")]
    InvalidPrivateKey(String),
    #[error("invalid mnemonic: {0}")]
    InvalidMnemonic(String),
    #[error("http client build failed: {0}")]
    HttpClient(String),
}

impl L1ProviderConfig {
    /// Build the configured L1 provider.
    pub fn build(self) -> Result<L1Provider, ProviderError> {
        if self.http_urls.is_empty() {
            return Err(ProviderError::NoHttpUrls);
        }

        // Resolve the signer up-front so we can stamp its address into the wallet.
        let signer = build_signer(&self.signer)?;
        let from = signer.address();
        let wallet = EthereumWallet::from(signer);

        // Per-request HTTP timeout at the client level so hanging connections
        // surface as errors the retry layer can act on.
        let http_client = reqwest::Client::builder()
            .timeout(self.timeout)
            .build()
            .map_err(|e| ProviderError::HttpClient(e.to_string()))?;

        let transports: Vec<Http<reqwest::Client>> = self
            .http_urls
            .iter()
            .cloned()
            .map(|url| Http::with_client(http_client.clone(), url))
            .collect();

        let active = NonZeroUsize::new(transports.len()).expect("non-empty checked above");
        let fallback_layer = FallbackLayer::default().with_active_transport_count(active);
        let retry_layer = RetryBackoffLayer::new(
            self.max_rate_limit_retries,
            self.initial_backoff_ms,
            self.compute_units_per_second,
        );

        let transport = ServiceBuilder::new()
            .layer(retry_layer)
            .layer(fallback_layer)
            .service(transports);
        let client = RpcClient::builder().transport(transport, false);

        // Filler stack:
        //   default gas estimator → our 3/2 fallback filler → blob gas →
        //   cached nonce → chain id → wallet.
        // Order matters: the default `with_gas_estimation` must run first so
        // the custom filler only kicks in when the standard path errors.
        let provider = ProviderBuilder::default()
            .with_gas_estimation()
            .filler(GasEstimateWithFallbackFiller)
            .with_blob_gas_estimation()
            .with_nonce_management(CachedNonceManager::default())
            .fetch_chain_id()
            .wallet(wallet)
            .connect_client(client)
            .erased();

        Ok(L1Provider { provider, from })
    }
}

fn build_signer(kind: &SignerKind) -> Result<PrivateKeySigner, ProviderError> {
    match kind {
        SignerKind::PrivateKey(pk) => pk
            .trim_start_matches("0x")
            .parse::<PrivateKeySigner>()
            .map_err(|e| ProviderError::InvalidPrivateKey(e.to_string())),
        SignerKind::Mnemonic { phrase, hd_path } => MnemonicBuilder::<English>::default()
            .phrase(phrase.as_str())
            .derivation_path(hd_path)
            .map_err(|e| ProviderError::InvalidMnemonic(e.to_string()))?
            .build()
            .map_err(|e| ProviderError::InvalidMnemonic(e.to_string())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_empty_http_urls() {
        let cfg = L1ProviderConfig {
            http_urls: vec![],
            timeout: Duration::from_secs(10),
            max_rate_limit_retries: 5,
            initial_backoff_ms: 500,
            compute_units_per_second: 660,
            signer: SignerKind::PrivateKey(
                "ac0974bec39a17e36ba4a6b4d238ff944bacb478cbed5efcae784d7bf4f2ff80".into(),
            ),
        };
        assert!(matches!(cfg.build(), Err(ProviderError::NoHttpUrls)));
    }

    #[test]
    fn rejects_invalid_private_key() {
        let cfg = L1ProviderConfig {
            http_urls: vec![Url::parse("http://localhost:0").unwrap()],
            timeout: Duration::from_secs(10),
            max_rate_limit_retries: 5,
            initial_backoff_ms: 500,
            compute_units_per_second: 660,
            signer: SignerKind::PrivateKey("not-a-key".into()),
        };
        assert!(matches!(
            cfg.build(),
            Err(ProviderError::InvalidPrivateKey(_))
        ));
    }
}

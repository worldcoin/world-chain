//! Shared L1 transaction-signer construction for proof-system services.

use alloy_network::EthereumWallet;
use alloy_provider::{Provider, ProviderBuilder};
use alloy_signer_aws::{AwsSigner, AwsSignerError};
use alloy_signer_local::PrivateKeySigner;
use alloy_transport::TransportError;
use thiserror::Error;
use url::Url;

/// An L1 transaction signer backed by a local private key or AWS KMS.
#[derive(Clone)]
pub enum TransactionSigner {
    Local(PrivateKeySigner),
    Aws(AwsSigner),
}

enum TransactionSignerSource {
    Local(PrivateKeySigner),
    AwsKms(String),
}

fn select_signer_source(
    private_key: Option<PrivateKeySigner>,
    aws_kms_key_id: Option<String>,
) -> Result<TransactionSignerSource, TransactionSignerError> {
    match (private_key, aws_kms_key_id) {
        (Some(signer), None) => Ok(TransactionSignerSource::Local(signer)),
        (None, Some(key_id)) if !key_id.trim().is_empty() => {
            Ok(TransactionSignerSource::AwsKms(key_id))
        }
        _ => Err(TransactionSignerError::InvalidConfiguration),
    }
}

/// Builds a transaction signer from exactly one local private key or AWS KMS key ID.
pub async fn build_transaction_signer(
    private_key: Option<PrivateKeySigner>,
    aws_kms_key_id: Option<String>,
    rpc_url: &Url,
) -> Result<TransactionSigner, TransactionSignerError> {
    match select_signer_source(private_key, aws_kms_key_id)? {
        TransactionSignerSource::Local(signer) => Ok(TransactionSigner::Local(signer)),
        TransactionSignerSource::AwsKms(key_id) => {
            let chain_id = ProviderBuilder::new()
                .connect_http(rpc_url.clone())
                .get_chain_id()
                .await
                .map_err(TransactionSignerError::ChainId)?;
            let sdk_config = aws_config::defaults(aws_config::BehaviorVersion::latest())
                .load()
                .await;
            let signer = AwsSigner::new(
                aws_sdk_kms::Client::new(&sdk_config),
                key_id,
                Some(chain_id),
            )
            .await
            .map_err(|error| TransactionSignerError::AwsKms(Box::new(error)))?;
            Ok(TransactionSigner::Aws(signer))
        }
    }
}

impl TransactionSigner {
    /// Wraps this signer in an Ethereum wallet suitable for Alloy providers.
    #[must_use]
    pub fn wallet(&self) -> EthereumWallet {
        match self {
            Self::Local(signer) => EthereumWallet::from(signer.clone()),
            Self::Aws(signer) => EthereumWallet::from(signer.clone()),
        }
    }
}

/// Errors returned while selecting or initializing an L1 transaction signer.
#[derive(Debug, Error)]
pub enum TransactionSignerError {
    #[error("configure exactly one local private key or AWS KMS key ID")]
    InvalidConfiguration,
    #[error("failed to fetch L1 chain ID: {0}")]
    ChainId(TransportError),
    #[error("failed to initialize AWS KMS signer: {0}")]
    AwsKms(Box<AwsSignerError>),
}

#[cfg(test)]
mod tests {
    use alloy_network::{Ethereum, NetworkWallet};
    use alloy_primitives::B256;

    use super::*;

    fn local_signer() -> PrivateKeySigner {
        PrivateKeySigner::from_bytes(&B256::with_last_byte(1)).expect("valid private key")
    }

    #[test]
    fn requires_exactly_one_signer_source() {
        assert!(matches!(
            select_signer_source(None, None),
            Err(TransactionSignerError::InvalidConfiguration)
        ));
        assert!(matches!(
            select_signer_source(Some(local_signer()), Some("alias/test".to_owned())),
            Err(TransactionSignerError::InvalidConfiguration)
        ));
        assert!(matches!(
            select_signer_source(None, Some("  ".to_owned())),
            Err(TransactionSignerError::InvalidConfiguration)
        ));
        assert!(matches!(
            select_signer_source(None, Some("alias/test".to_owned())),
            Ok(TransactionSignerSource::AwsKms(key_id)) if key_id == "alias/test"
        ));
    }

    #[tokio::test]
    async fn builds_local_wallet_with_derived_address() {
        let expected = local_signer().address();
        let rpc_url = "http://127.0.0.1:8545".parse().expect("valid URL");
        let wallet = build_transaction_wallet(Some(local_signer()), None, &rpc_url)
            .await
            .expect("local wallet builds");

        assert_eq!(wallet.default_signer().address(), expected);
        assert!(<EthereumWallet as NetworkWallet<Ethereum>>::has_signer_for(
            &wallet, &expected
        ));
    }
}

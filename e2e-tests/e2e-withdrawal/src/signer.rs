//! Local and AWS KMS wallet construction shared by both chains.

use crate::{retry, rpc, types::StepError};
use alloy_network::EthereumWallet;
use alloy_provider::{Provider, ProviderBuilder};
use alloy_signer_aws::AwsSigner;
use alloy_signer_local::PrivateKeySigner;
use aws_sdk_kms::config::{retry::RetryConfig, timeout::TimeoutConfig};
use eyre::eyre::{WrapErr, bail};

pub fn validate(
    private_key: Option<&str>,
    kms_key_id: Option<&str>,
    field: &'static str,
) -> Result<(), StepError> {
    match (private_key, kms_key_id) {
        (Some(key), None) if key.parse::<PrivateKeySigner>().is_ok() => Ok(()),
        (None, Some(id)) if !id.trim().is_empty() => Ok(()),
        _ => Err(StepError::InvalidConfiguration { field }),
    }
}

pub async fn wallet(
    private_key: Option<&str>,
    kms_key_id: Option<&str>,
    endpoint: &str,
    rpc_field: &'static str,
    signer_field: &'static str,
) -> Result<EthereumWallet, StepError> {
    validate(private_key, kms_key_id, signer_field)?;
    if let Some(key) = private_key {
        let signer =
            key.parse::<PrivateKeySigner>()
                .map_err(|_| StepError::InvalidConfiguration {
                    field: signer_field,
                })?;
        return Ok(EthereumWallet::from(signer));
    }

    // Also bound credential discovery and the chain-ID lookup, not only KMS requests.
    tokio::time::timeout(
        retry::DEFAULT_TIMEOUT,
        Box::pin(async {
            let chain_id = ProviderBuilder::new()
                .connect_client(rpc::client(endpoint, rpc_field)?)
                .get_chain_id()
                .await
                .wrap_err_with(|| format!("failed to fetch chain ID via {rpc_field}"))?;
            let config = aws_config::defaults(aws_config::BehaviorVersion::latest())
                .load()
                .await;
            if config.region().is_none() {
                bail!("AWS region missing; set AWS_REGION or AWS_DEFAULT_REGION");
            }
            let config = aws_sdk_kms::config::Builder::from(&config)
                .timeout_config(
                    TimeoutConfig::builder()
                        .operation_timeout(retry::DEFAULT_TIMEOUT)
                        .operation_attempt_timeout(retry::DEFAULT_TIMEOUT)
                        .build(),
                )
                .retry_config(RetryConfig::standard().with_max_attempts(retry::RPC_MAX_ATTEMPTS))
                .build();
            let signer = AwsSigner::new(
                aws_sdk_kms::Client::from_conf(config),
                kms_key_id.expect("validated KMS source").to_owned(),
                Some(chain_id),
            )
            .await
            .wrap_err("failed to retrieve AWS KMS signing public key")?;
            Ok::<_, eyre::Report>(EthereumWallet::from(signer))
        }),
    )
    .await
    .wrap_err_with(|| format!("{signer_field}: signer initialization timed out after 30 seconds"))?
    .wrap_err_with(|| format!("{signer_field}: signer initialization failed"))
    .map_err(StepError::from)
}

#[cfg(test)]
mod tests {
    use super::*;
    const KEY: &str = "0000000000000000000000000000000000000000000000000000000000000001";

    #[test]
    fn validates_exactly_one_source() {
        for (key, kms, valid) in [
            (Some(KEY), None, true),
            (None, Some("alias/test"), true),
            (None, None, false),
            (Some(KEY), Some("alias/test"), false),
            (Some("invalid-secret"), None, false),
            (None, Some("  "), false),
            (None, Some(""), false),
        ] {
            assert_eq!(validate(key, kms, "signer").is_ok(), valid);
        }
        let error = validate(Some("invalid-secret"), None, "signer").unwrap_err();
        assert!(!format!("{error:?}").contains("invalid-secret"));
    }

    #[tokio::test]
    async fn local_wallet_needs_no_network() {
        let wallet = wallet(Some(KEY), None, "invalid-url", "RPC", "signer")
            .await
            .unwrap();
        assert_eq!(
            wallet.default_signer().address(),
            KEY.parse::<PrivateKeySigner>().unwrap().address()
        );
    }
}

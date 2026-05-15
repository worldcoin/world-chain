use alloy_eips::BlockNumberOrTag;
use alloy_provider::{Provider, RootProvider};
use alloy_rpc_client::RpcClient;
use alloy_transport::{
    TransportError,
    layers::{RateLimitRetryPolicy, RetryBackoffLayer},
};
use alloy_transport_http::reqwest::{
    Client,
    header::{HeaderMap, HeaderName, HeaderValue},
};
use eyre::eyre::{WrapErr, eyre};

use super::config::{CloudflareAccess, Config};

#[derive(Clone)]
pub(super) struct RpcEnv {
    provider: RootProvider,
    config: Config,
}

impl RpcEnv {
    pub(super) async fn connect() -> eyre::Result<Option<Self>> {
        reth_tracing::init_test_tracing();

        let Some(config) = Config::from_env()? else {
            return Ok(None);
        };
        let policy = RateLimitRetryPolicy::default().or(|err: &TransportError| {
            let msg = err.to_string();
            msg.contains("connection error")
                || msg.contains("SendRequest")
                || msg.contains("error sending request")
        });
        let retry = RetryBackoffLayer::new_with_policy(4, 100, 330, policy);
        let http_client = http_client(config.cloudflare_access.as_ref())?;
        let client = RpcClient::builder()
            .layer(retry)
            .http_with_client(http_client, config.rpc_url.clone());
        let provider = RootProvider::new(client);

        Ok(Some(Self { provider, config }))
    }

    pub(super) fn config(&self) -> &Config {
        &self.config
    }

    pub(super) async fn chain_id(&self) -> eyre::Result<u64> {
        Ok(self.provider.get_chain_id().await?)
    }

    pub(super) async fn latest_block_number(&self) -> eyre::Result<u64> {
        Ok(self.provider.get_block_number().await?)
    }

    pub(super) async fn latest_block(&self) -> eyre::Result<alloy_rpc_types_eth::Block> {
        self.provider
            .get_block_by_number(BlockNumberOrTag::Latest)
            .await?
            .ok_or_else(|| eyre!("latest block missing"))
    }
}

fn http_client(access: Option<&CloudflareAccess>) -> eyre::Result<Client> {
    let mut builder = Client::builder();

    if let Some(access) = access {
        let mut headers = HeaderMap::new();
        headers.insert(
            HeaderName::from_static("cf-access-client-id"),
            HeaderValue::from_str(&access.client_id)
                .wrap_err("failed to build CF-Access-Client-Id header")?,
        );
        headers.insert(
            HeaderName::from_static("cf-access-client-secret"),
            HeaderValue::from_str(&access.client_secret)
                .wrap_err("failed to build CF-Access-Client-Secret header")?,
        );
        builder = builder.default_headers(headers);
    }

    builder
        .build()
        .wrap_err("failed to build acceptance test HTTP client")
}
